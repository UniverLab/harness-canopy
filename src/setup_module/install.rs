//! Vendor-official harness installation with explicit user consent.
//!
//! Runs the platform's documented `[[install_commands]]` entry directly via
//! `portable_pty` and the platform shell (`sh -c` on Unix, `cmd /C` on
//! Windows), streaming output live while retaining the last 20 lines.

use crate::domain::cli_config::platform_display_name;
use crate::domain::cli_strategy::resolve_binary_in;
use crate::domain::registry_baseline::RegistryBaseline;
use crate::setup_module::harness_list::{resolve_harness_binary, resolve_on_path};
use crate::setup_module::models::{InstallCommand, Platform};
use crate::setup_module::registry_pending::load_pending;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, IsTerminal, Read, Write};
use std::path::Path;

/// Maximum number of output lines retained for failure reporting.
pub(crate) const TAIL_LINE_LIMIT: usize = 20;

/// Plain warning shown on every interactive confirmation screen.
pub(crate) const CONSENT_WARNING: &str =
    "this runs a command from the harness vendor on your machine";

/// Availability of an installer for a platform on the current OS.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum InstallAvail {
    Ready(InstallCommand),
    Needs {
        missing: String,
        cmd: InstallCommand,
    },
    NoCommand {
        docs_url: Option<String>,
    },
}

/// Running OS identifier matching `[[install_commands]].os`.
pub(crate) fn running_os() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        "linux"
    }
}

/// Select the best `InstallCommand` for `os` given `path_env`:
/// first OS-matching entry whose `requires` are all on `PATH`, or else
/// the first OS-matching entry with its first missing prerequisite.
pub(crate) fn select_install_command_for(
    platform: &Platform,
    os: &str,
    path_env: &str,
) -> InstallAvail {
    let matching: Vec<&InstallCommand> = platform
        .install_commands
        .iter()
        .filter(|c| c.os == os)
        .collect();

    if matching.is_empty() {
        return InstallAvail::NoCommand {
            docs_url: platform.docs_url.clone(),
        };
    }

    for cmd in &matching {
        if cmd
            .requires
            .iter()
            .all(|req| resolve_binary_in(req, path_env).is_ok())
        {
            return InstallAvail::Ready((*cmd).clone());
        }
    }

    let primary = matching[0];
    let missing = primary
        .requires
        .iter()
        .find(|req| resolve_binary_in(req, path_env).is_err())
        .cloned()
        .unwrap_or_default();

    InstallAvail::Needs {
        missing,
        cmd: primary.clone(),
    }
}

/// Select the install command for the running OS and current `PATH`.
pub(crate) fn select_install_command(platform: &Platform) -> InstallAvail {
    let path_env = std::env::var("PATH").unwrap_or_default();
    select_install_command_for(platform, running_os(), &path_env)
}

/// True when `registry-pending.json` holds an unapproved `install_commands`
/// change for `name`.
pub(crate) fn pending_install_blocks(canopy_dir: &Path, name: &str) -> bool {
    load_pending(canopy_dir)
        .iter()
        .any(|c| c.cli == name && c.field == "install_commands")
}

/// Resolve the platform definition safe to use for installation.
///
/// If `install_commands` has a pending change in `registry-pending.json`,
/// never run the staged command: fall back to the baseline's approved
/// `install_commands` if non-empty, otherwise return an error instructing
/// the user to run `canopy registry apply`.
pub(crate) fn resolve_approved_platform(
    canopy_dir: &Path,
    fetched: &Platform,
) -> Result<Platform, String> {
    if !pending_install_blocks(canopy_dir, &fetched.name) {
        return Ok(fetched.clone());
    }

    if let Some(baseline) = RegistryBaseline::load(canopy_dir) {
        if let Some(approved) = baseline.platforms.iter().find(|p| p.name == fetched.name) {
            if !approved.install_commands.is_empty() {
                return Ok(approved.clone());
            }
        }
    }

    Err(format!(
        "pending installer update for '{}' awaits `canopy registry apply`",
        fetched.name
    ))
}

/// Vendor display name for the confirmation screen.
pub(crate) fn vendor_label(platform: &Platform) -> String {
    platform_display_name(
        &platform.name,
        platform.provider.as_deref(),
        platform.tool_name.as_deref(),
    )
}

/// Format the confirmation lines shown before running an installer.
pub(crate) fn format_confirmation_lines(platform: &Platform, cmd: &InstallCommand) -> Vec<String> {
    vec![
        format!("Vendor: {}", vendor_label(platform)),
        format!("Command: {}", cmd.command),
        format!("Source: {}", cmd.source_url),
        format!("Warning: {CONSENT_WARNING}"),
    ]
}

/// Format the no-command status label for a harness row or CLI error.
pub(crate) fn format_no_command_label(docs_url: Option<&str>) -> String {
    match docs_url {
        Some(url) => format!("install: see {url}"),
        None => "install: not available for this OS".to_string(),
    }
}

/// Check whether `platform` is installed only in `install_paths` and not on
/// `PATH`, returning the directory the user should add to `PATH`.
pub(crate) fn post_install_path_notice(
    platform: &Platform,
    home: Option<&Path>,
    path_env: &str,
) -> Option<String> {
    let resolved = resolve_harness_binary(platform, home, path_env)?;
    if resolve_on_path(platform, path_env).is_some() {
        return None;
    }
    let dir = resolved
        .parent()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| resolved.display().to_string());
    Some(format!(
        "Installed at {}, which is not on PATH. Add {} to your PATH.",
        resolved.display(),
        dir
    ))
}

/// Run a vendor install command in a local PTY via the platform shell,
/// streaming output live to stdout while retaining the last 20 lines.
pub(crate) fn run_vendor_install(command: &str) -> anyhow::Result<(i32, Vec<String>)> {
    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    #[cfg(unix)]
    let cmd = {
        let mut c = CommandBuilder::new("sh");
        c.arg("-c");
        c.arg(command);
        c
    };
    #[cfg(windows)]
    let cmd = {
        let mut c = CommandBuilder::new("cmd");
        c.arg("/C");
        c.arg(command);
        c
    };

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let reader = pair.master.try_clone_reader()?;
    let tail = stream_and_collect_tail(reader);
    let status = child.wait()?;
    let code = status.exit_code() as i32;
    Ok((code, tail))
}

/// Stream PTY bytes to stdout and keep the last `TAIL_LINE_LIMIT` lines.
fn stream_and_collect_tail(reader: Box<dyn Read + Send>) -> Vec<String> {
    let mut tail: VecDeque<String> = VecDeque::with_capacity(TAIL_LINE_LIMIT);
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    let mut stdout = std::io::stdout();

    loop {
        line.clear();
        match buf_reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let _ = stdout.write_all(line.as_bytes());
                let _ = stdout.flush();
                let cleaned = line.trim_end_matches(['\r', '\n']).to_string();
                if tail.len() == TAIL_LINE_LIMIT {
                    tail.pop_front();
                }
                tail.push_back(cleaned);
            }
        }
    }

    tail.into_iter().collect()
}

/// Prompt `Install? [y/N]` on stdin; true only for `y` or `yes`.
pub(crate) fn prompt_yes_no() -> bool {
    print!("Install? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}

/// Run `canopy setup --install <platform> [--yes]`.
pub fn run_install_cmd(platform_name: &str, yes: bool) -> i32 {
    let is_tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    run_install_cmd_with(platform_name, yes, is_tty, || {
        super::registry_fetch::fetch_registry()
    })
}

/// Testable core of `run_install_cmd` with injectable terminal flag and
/// registry fetcher.
pub(crate) fn run_install_cmd_with(
    platform_name: &str,
    yes: bool,
    is_tty: bool,
    fetch: impl FnOnce() -> anyhow::Result<crate::setup_module::models::RegistryRaw>,
) -> i32 {
    if !yes && !is_tty {
        eprintln!(
            "Error: --yes is required when running `canopy setup --install` non-interactively"
        );
        return 1;
    }

    let registry = match fetch() {
        Ok(r) => r,
        Err(err) => {
            eprintln!("Error fetching registry: {err}");
            return 1;
        }
    };

    let Some(fetched) = registry.platforms.iter().find(|p| p.name == platform_name) else {
        eprintln!("Error: unknown platform '{platform_name}'");
        return 1;
    };

    let Some(home) = dirs::home_dir() else {
        eprintln!("Error: could not determine home directory");
        return 1;
    };
    let approved_platform = match resolve_approved_platform(&home.join(".canopy"), fetched) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("Error: {msg}");
            return 1;
        }
    };

    let cmd = match select_install_command(&approved_platform) {
        InstallAvail::Ready(cmd) => cmd,
        InstallAvail::Needs { missing, .. } => {
            eprintln!("Error: needs {missing} first");
            return 1;
        }
        InstallAvail::NoCommand { docs_url } => {
            eprintln!("Error: {}", format_no_command_label(docs_url.as_deref()));
            return 1;
        }
    };

    for line in format_confirmation_lines(&approved_platform, &cmd) {
        println!("{line}");
    }

    if !yes && !prompt_yes_no() {
        println!("Installation cancelled.");
        return 0;
    }

    match run_vendor_install(&cmd.command) {
        Ok((0, _)) => {
            let path_env = std::env::var("PATH").unwrap_or_default();
            if let Some(notice) =
                post_install_path_notice(&approved_platform, Some(&home), &path_env)
            {
                println!("{notice}");
            }
            0
        }
        Ok((code, _)) => {
            eprintln!("failed: {code}");
            if code == 0 {
                1
            } else {
                code
            }
        }
        Err(err) => {
            eprintln!("Error running installer: {err}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_module::models::RegistryRaw;
    use crate::setup_module::registry_pending::{save_pending, PendingChange};
    use std::collections::HashMap;

    fn test_platform(
        name: &str,
        commands: Vec<InstallCommand>,
        docs_url: Option<&str>,
    ) -> Platform {
        Platform {
            name: name.to_string(),
            config_path: format!(".config/{name}"),
            config_format: None,
            toml_array_format: false,
            command_format: "separate".to_string(),
            mcp_servers_key: vec![],
            deprecated_keys: vec![],
            unsupported_keys: vec![],
            fields_mapping: HashMap::new(),
            required_fields: HashMap::new(),
            server_extras: HashMap::new(),
            skills_dir: None,
            instruction_file: None,
            user_instruction_file: None,
            instruction_precedence: vec![],
            provider: Some("Acme".to_string()),
            tool_name: Some("Acme CLI".to_string()),
            project_paths: vec![],
            docs_url: docs_url.map(str::to_string),
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: vec![],
            install_commands: commands,
            cli: Some(serde_json::json!({"binary": name})),
        }
    }

    fn cmd(os: &str, command: &str, requires: &[&str]) -> InstallCommand {
        InstallCommand {
            os: os.to_string(),
            command: command.to_string(),
            description: "Install tool".to_string(),
            requires: requires.iter().map(|s| s.to_string()).collect(),
            source_url: "https://example.com/install".to_string(),
        }
    }

    fn make_exec(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
        }
        p
    }

    #[test]
    fn select_install_command_picks_first_matching_with_requires_on_path() {
        let dir = tempfile::tempdir().unwrap();
        make_exec(dir.path(), "curl", "exit 0");
        make_exec(dir.path(), "npm", "exit 0");
        let path_env = dir.path().display().to_string();

        let p = test_platform(
            "demo",
            vec![
                cmd("linux", "brew install demo", &["brew"]),
                cmd("linux", "npm i -g demo", &["npm"]),
                cmd("linux", "curl -fsSL https://x | sh", &["curl"]),
            ],
            None,
        );

        let avail = select_install_command_for(&p, "linux", &path_env);
        assert_eq!(
            avail,
            InstallAvail::Ready(cmd("linux", "npm i -g demo", &["npm"]))
        );
    }

    #[test]
    fn select_install_command_falls_back_to_first_for_os_on_missing_requires() {
        let p = test_platform(
            "demo",
            vec![
                cmd("macos", "brew install demo", &["brew"]),
                cmd("linux", "npm i -g demo", &["nonexistent_npm_xyz"]),
                cmd(
                    "linux",
                    "curl -fsSL https://x | sh",
                    &["nonexistent_curl_xyz"],
                ),
            ],
            None,
        );

        let avail = select_install_command_for(&p, "linux", "/nonexistent");
        assert_eq!(
            avail,
            InstallAvail::Needs {
                missing: "nonexistent_npm_xyz".to_string(),
                cmd: cmd("linux", "npm i -g demo", &["nonexistent_npm_xyz"]),
            }
        );
    }

    #[test]
    fn select_install_command_no_command_for_os_with_and_without_docs() {
        let with_docs = test_platform(
            "demo",
            vec![cmd("windows", "winget install demo", &[])],
            Some("https://example.com/docs"),
        );
        assert_eq!(
            select_install_command_for(&with_docs, "linux", ""),
            InstallAvail::NoCommand {
                docs_url: Some("https://example.com/docs".to_string())
            }
        );
        assert_eq!(
            format_no_command_label(Some("https://example.com/docs")),
            "install: see https://example.com/docs"
        );

        let without_docs = test_platform("demo", vec![], None);
        assert_eq!(
            select_install_command_for(&without_docs, "linux", ""),
            InstallAvail::NoCommand { docs_url: None }
        );
        assert_eq!(
            format_no_command_label(None),
            "install: not available for this OS"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_vendor_install_captures_exit_and_last_20_lines() {
        let dir = tempfile::tempdir().unwrap();
        let script = make_exec(
            dir.path(),
            "fail_install.sh",
            "i=1; while [ $i -le 30 ]; do echo \"line $i\"; i=$((i+1)); done; exit 7",
        );

        let (code, tail) = run_vendor_install(&script.display().to_string()).unwrap();
        assert_eq!(code, 7);
        assert_eq!(tail.len(), 20);
        assert_eq!(tail.first().map(String::as_str), Some("line 11"));
        assert_eq!(tail.last().map(String::as_str), Some("line 30"));
    }

    #[test]
    fn run_install_cmd_requires_yes_non_interactively() {
        let p = test_platform("demo", vec![cmd(running_os(), "echo hi", &[])], None);
        let code = run_install_cmd_with("demo", false, false, || {
            Ok(RegistryRaw {
                platforms: vec![p],
                canonical_servers: Default::default(),
            })
        });
        assert_eq!(code, 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_install_cmd_with_yes_runs_stub_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran.txt");
        let command = format!("echo installed > {}", marker.display());
        let p = test_platform("demo", vec![cmd(running_os(), &command, &[])], None);

        let code = run_install_cmd_with("demo", true, false, || {
            Ok(RegistryRaw {
                platforms: vec![p],
                canonical_servers: Default::default(),
            })
        });
        assert_eq!(code, 0);
        assert!(marker.exists());
    }

    #[test]
    fn resolve_approved_platform_blocks_pending_without_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let fetched = test_platform("demo", vec![cmd("linux", "evil", &[])], None);
        save_pending(
            &canopy_dir,
            &[PendingChange {
                cli: "demo".to_string(),
                field: "install_commands".to_string(),
                old: serde_json::json!([]),
                new: serde_json::to_value(&fetched.install_commands).unwrap(),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();

        let err = resolve_approved_platform(&canopy_dir, &fetched).unwrap_err();
        assert!(err.contains("canopy registry apply"));
    }

    #[test]
    fn post_install_path_notice_reports_install_paths_dir_when_not_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin_dir = dir.path().join(".local/bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        make_exec(&bin_dir, "demo", "exit 0");

        let mut p = test_platform("demo", vec![], None);
        p.install_paths = vec!["~/.local/bin/demo".to_string()];

        let notice = post_install_path_notice(&p, Some(dir.path()), "/usr/bin").unwrap();
        assert!(notice.contains(&bin_dir.display().to_string()));
    }

    #[cfg(unix)]
    #[test]
    fn run_install_cmd_with_yes_prints_command_and_source_before_running() {
        use std::os::unix::io::AsRawFd;

        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran.txt");
        let command = format!("echo installed > {}", marker.display());
        let p = test_platform("demo", vec![cmd(running_os(), &command, &[])], None);

        let mut pipe_fds = [0i32; 2];
        let rc = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe failed");
        let stdout_fd = std::io::stdout().as_raw_fd();
        let saved_stdout = unsafe { libc::dup(stdout_fd) };
        assert!(saved_stdout >= 0, "dup failed");
        unsafe { libc::dup2(pipe_fds[1], stdout_fd) };
        unsafe { libc::close(pipe_fds[1]) };

        let code = run_install_cmd_with("demo", true, false, || {
            Ok(RegistryRaw {
                platforms: vec![p],
                canonical_servers: Default::default(),
            })
        });

        unsafe { libc::dup2(saved_stdout, stdout_fd) };
        unsafe { libc::close(saved_stdout) };

        let mut output = String::new();
        use std::os::unix::io::FromRawFd;
        let mut reader = unsafe { std::fs::File::from_raw_fd(pipe_fds[0]) };
        use std::io::Read as _;
        let _ = reader.read_to_string(&mut output);

        assert_eq!(code, 0);
        assert!(marker.exists());
        assert!(output.contains(&command), "stdout: {output}");
        assert!(
            output.contains("https://example.com/install"),
            "stdout: {output}"
        );
    }
}
