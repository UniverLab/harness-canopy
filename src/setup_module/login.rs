//! Interactive harness login: run the vendor's login command in a local PTY
//! and verify authentication via a non-interactive check command.
//!
//! Credentials are never read, logged or copied by canopy; only the exit code
//! of `login_check_command` is used.

use crate::domain::registry_baseline::RegistryBaseline;
use crate::setup_module::models::{is_platform_available, Platform};
use crate::setup_module::registry_pending::load_pending;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

/// Instruction printed before running an interactive login command.
pub(crate) const LOGIN_INSTRUCTION: &str =
    "A browser window opens; finish the sign-in there, then press Enter here";

/// Timeout for a single `login_check_command`.
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of a login PTY run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LoginRun {
    /// Child exited with this code.
    Exited(i32),
    /// User pressed Esc; child was killed.
    Cancelled,
    /// Failed to start or run the PTY.
    Failed(String),
}

/// Authentication status for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginStatus {
    /// `login_check_command` exited 0.
    Authenticated,
    /// `login_check_command` exited non-zero or timed out.
    NotAuthenticated,
    /// No `login_check_command`; cannot assert state.
    NotVerifiable,
    /// No `login_command`; show docs/unknown.
    NoCommand,
    /// Check has not run yet.
    Unknown,
}

/// Format the login status/label for a harness row.
pub(crate) fn format_login_label(platform: &Platform, status: Option<LoginStatus>) -> String {
    if platform.login_command.is_none() {
        return match platform.docs_url.as_deref() {
            Some(url) => format!("login: see {url}"),
            None => "login: unknown".to_string(),
        };
    }
    match status.unwrap_or(LoginStatus::Unknown) {
        LoginStatus::Authenticated => "authenticated".to_string(),
        LoginStatus::NotAuthenticated => "not authenticated yet".to_string(),
        LoginStatus::NotVerifiable => "login: not verifiable".to_string(),
        LoginStatus::NoCommand => match platform.docs_url.as_deref() {
            Some(url) => format!("login: see {url}"),
            None => "login: unknown".to_string(),
        },
        LoginStatus::Unknown => "login: unknown".to_string(),
    }
}

/// Resolve the platform definition safe to use for login.
///
/// If `login_command` or `login_check_command` has a pending change, never run
/// the staged value: fall back to the baseline's approved value when present.
/// A pending `login_command` with no approved baseline is an error instructing
/// the user to run `canopy registry apply`. A pending `login_check_command`
/// with no approved baseline becomes not verifiable.
pub(crate) fn resolve_login_platform(
    canopy_dir: &Path,
    fetched: &Platform,
) -> Result<Platform, String> {
    let pending = load_pending(canopy_dir);
    let login_cmd_pending = pending
        .iter()
        .any(|c| c.cli == fetched.name && c.field == "login_command");
    let login_check_pending = pending
        .iter()
        .any(|c| c.cli == fetched.name && c.field == "login_check_command");
    if !login_cmd_pending && !login_check_pending {
        return Ok(fetched.clone());
    }

    let baseline = RegistryBaseline::load(canopy_dir);
    let approved_base = baseline
        .as_ref()
        .and_then(|b| b.platforms.iter().find(|p| p.name == fetched.name));

    let mut out = fetched.clone();
    if login_cmd_pending {
        match approved_base.and_then(|p| p.login_command.clone()) {
            Some(cmd) => out.login_command = Some(cmd),
            None => {
                return Err(format!(
                    "pending login update for '{}' awaits `canopy registry apply`",
                    fetched.name
                ))
            }
        }
    }
    if login_check_pending {
        out.login_check_command = approved_base.and_then(|p| p.login_check_command.clone());
    }
    Ok(out)
}

/// Run `login_check_command` with a 5 s timeout. Returns true only on exit 0.
pub(crate) fn run_check(command: &str) -> bool {
    run_check_with_timeout(command, CHECK_TIMEOUT)
}

fn run_check_with_timeout(command: &str, timeout: Duration) -> bool {
    let mut cmd = build_shell_command(command);
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::null());
    cmd.stdin(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn build_shell_command(command: &str) -> std::process::Command {
    #[cfg(unix)]
    {
        let mut c = std::process::Command::new("sh");
        c.arg("-c").arg(command);
        c
    }
    #[cfg(windows)]
    {
        let mut c = std::process::Command::new("cmd");
        c.arg("/C").arg(command);
        c
    }
}

fn approved_login_check_command(canopy_dir: &Path, platform: &Platform) -> Option<String> {
    let pending = load_pending(canopy_dir);
    let check_pending = pending
        .iter()
        .any(|c| c.cli == platform.name && c.field == "login_check_command");
    if !check_pending {
        return platform.login_check_command.clone();
    }
    RegistryBaseline::load(canopy_dir)
        .and_then(|b| b.platforms.into_iter().find(|p| p.name == platform.name))
        .and_then(|p| p.login_check_command)
}

/// Compute login status for one platform by running its approved check command.
/// Never executes a staged/unapproved `login_check_command`.
pub(crate) fn status_for_platform(platform: &Platform, canopy_dir: &Path) -> LoginStatus {
    let Some(check) = approved_login_check_command(canopy_dir, platform) else {
        if platform.login_command.is_some() {
            return LoginStatus::NotVerifiable;
        }
        return LoginStatus::NoCommand;
    };
    if run_check(&check) {
        LoginStatus::Authenticated
    } else {
        LoginStatus::NotAuthenticated
    }
}

/// Check login status for all platforms in parallel. Never executes a staged/unapproved
/// `login_check_command`.
pub(crate) fn check_all_parallel(
    platforms: &[Platform],
    canopy_dir: &Path,
) -> Vec<(String, LoginStatus)> {
    std::thread::scope(|s| {
        let handles: Vec<_> = platforms
            .iter()
            .map(|p| {
                let name = p.name.clone();
                let p = p.clone();
                s.spawn(move || (name, status_for_platform(&p, canopy_dir)))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| ("".to_string(), LoginStatus::Unknown))
            })
            .filter(|(n, _)| !n.is_empty())
            .collect()
    })
}

/// Run a login command in a local PTY. Inherits the current environment.
/// Pressing Esc kills the child and returns `LoginRun::Cancelled`.
pub(crate) fn run_login_pty(command: &str) -> LoginRun {
    match run_login_pty_inner(command) {
        Ok(run) => run,
        Err(err) => LoginRun::Failed(err.to_string()),
    }
}

struct RawModeGuard(bool);

impl RawModeGuard {
    fn enter() -> Self {
        let ok = ratatui::crossterm::terminal::enable_raw_mode().is_ok();
        Self(ok)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.0 {
            let _ = ratatui::crossterm::terminal::disable_raw_mode();
        }
    }
}

fn run_login_pty_inner(command: &str) -> anyhow::Result<LoginRun> {
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

    let child: Box<dyn portable_pty::Child + Send> = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let child = Arc::new(Mutex::new(child));
    let done = Arc::new(AtomicBool::new(false));
    let cancelled = Arc::new(AtomicBool::new(false));

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let out_done = Arc::clone(&done);
    let out_handle = std::thread::spawn(move || {
        stream_pty_output(reader);
        out_done.store(true, Ordering::SeqCst);
    });

    let _raw = RawModeGuard::enter();
    let in_child = Arc::clone(&child);
    let in_done = Arc::clone(&done);
    let in_cancelled = Arc::clone(&cancelled);
    let in_handle =
        std::thread::spawn(move || forward_stdin(writer, &in_child, &in_done, &in_cancelled));

    let status = wait_for_child(&child);
    done.store(true, Ordering::SeqCst);
    let _ = in_handle.join();
    let _ = out_handle.join();
    drop(_raw);

    if cancelled.load(Ordering::SeqCst) {
        return Ok(LoginRun::Cancelled);
    }
    let code = status.map(|s| s.exit_code() as i32).unwrap_or(1);
    Ok(LoginRun::Exited(code))
}

/// Wait for the PTY child without holding the child lock across the wait:
/// the Esc handler needs the same lock to kill the child, so a blocking
/// `wait()` under the lock would deadlock Esc instead of cancelling.
fn wait_for_child(
    child: &Arc<Mutex<Box<dyn portable_pty::Child + Send>>>,
) -> std::io::Result<portable_pty::ExitStatus> {
    loop {
        {
            let mut guarded = child.lock().expect("child lock");
            match guarded.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) => {}
                Err(err) => return Err(err),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn stream_pty_output(mut reader: Box<dyn Read + Send>) {
    let mut buf = [0u8; 4096];
    let mut stdout = std::io::stdout();
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let _ = stdout.write_all(&buf[..n]);
                let _ = stdout.flush();
            }
        }
    }
}

fn forward_stdin(
    mut writer: Box<dyn Write + Send>,
    child: &Arc<Mutex<Box<dyn portable_pty::Child + Send>>>,
    done: &Arc<AtomicBool>,
    cancelled: &Arc<AtomicBool>,
) {
    #[cfg(unix)]
    {
        forward_stdin_unix(&mut writer, child, done, cancelled);
    }
    #[cfg(not(unix))]
    {
        let _ = (writer, child, done, cancelled);
    }
}

#[cfg(unix)]
fn forward_stdin_unix(
    writer: &mut Box<dyn Write + Send>,
    child: &Arc<Mutex<Box<dyn portable_pty::Child + Send>>>,
    done: &Arc<AtomicBool>,
    cancelled: &Arc<AtomicBool>,
) {
    use std::os::unix::io::AsRawFd;
    let stdin_fd = std::io::stdin().as_raw_fd();
    let mut buf = [0u8; 1024];
    while !done.load(Ordering::SeqCst) {
        let mut pfd = libc::pollfd {
            fd: stdin_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
        if rc <= 0 {
            continue;
        }
        if (pfd.revents & libc::POLLIN) == 0 {
            continue;
        }
        let n = match std::io::stdin().read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if n == 1 && buf[0] == 0x1b {
            // Lone Esc? Wait briefly for escape sequence continuation.
            let mut pfd2 = libc::pollfd {
                fd: stdin_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let rc2 = unsafe { libc::poll(&mut pfd2, 1, 50) };
            if rc2 > 0 && (pfd2.revents & libc::POLLIN) != 0 {
                // Escape sequence - forward ESC + following bytes.
                let mut seq = vec![0x1b];
                let m = match std::io::stdin().read(&mut buf) {
                    Ok(0) | Err(_) => 0,
                    Ok(m) => m,
                };
                seq.extend_from_slice(&buf[..m]);
                let _ = writer.write_all(&seq);
                let _ = writer.flush();
                continue;
            }
            cancelled.store(true, Ordering::SeqCst);
            let mut c = child.lock().expect("child lock");
            let _ = c.kill();
            break;
        }
        let _ = writer.write_all(&buf[..n]);
        let _ = writer.flush();
    }
}

/// Run `canopy setup --login <platform>`.
pub fn run_login_cmd(platform_name: &str) -> i32 {
    let is_tty = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let Some(home) = dirs::home_dir() else {
        eprintln!("Error: could not determine home directory");
        return 1;
    };
    run_login_cmd_with(
        platform_name,
        is_tty,
        &home.join(".canopy"),
        super::registry_fetch::fetch_registry,
        run_login_pty,
    )
}

/// Testable core of `run_login_cmd`.
pub(crate) fn run_login_cmd_with(
    platform_name: &str,
    is_tty: bool,
    canopy_dir: &Path,
    fetch: impl FnOnce() -> anyhow::Result<(crate::setup_module::models::RegistryRaw, String)>,
    runner: impl FnOnce(&str) -> LoginRun,
) -> i32 {
    if !is_tty {
        eprintln!("Error: `canopy setup --login` requires an interactive terminal");
        return 1;
    }
    let registry = match fetch() {
        Ok((r, _)) => r,
        Err(err) => {
            eprintln!("Error fetching registry: {err}");
            return 1;
        }
    };
    let Some(fetched) = registry.platforms.iter().find(|p| p.name == platform_name) else {
        eprintln!("Error: unknown platform '{platform_name}'");
        return 1;
    };
    if !is_platform_available(fetched) {
        eprintln!("Error: platform '{platform_name}' is not installed");
        return 1;
    }
    let approved = match resolve_login_platform(canopy_dir, fetched) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("Error: {msg}");
            return 1;
        }
    };
    let Some(login_cmd) = approved.login_command.as_deref() else {
        eprintln!(
            "Error: {}",
            format_login_label(&approved, Some(LoginStatus::NoCommand))
        );
        return 1;
    };

    println!("{LOGIN_INSTRUCTION}");
    let run = runner(login_cmd);
    let login_ok = matches!(run, LoginRun::Exited(0));
    if !login_ok {
        match &run {
            LoginRun::Cancelled => eprintln!("login cancelled"),
            LoginRun::Failed(msg) => eprintln!("login failed: {msg}"),
            LoginRun::Exited(_) => eprintln!("login failed"),
        }
        return 1;
    }

    match approved.login_check_command.as_deref() {
        Some(check) => {
            if run_check(check) {
                0
            } else {
                eprintln!("not authenticated yet");
                1
            }
        }
        None => 0,
    }
}

/// Run `canopy setup --check-logins`.
pub fn run_check_logins_cmd() -> i32 {
    let Some(home) = dirs::home_dir() else {
        eprintln!("Error: could not determine home directory");
        return 1;
    };
    run_check_logins_cmd_with(
        super::registry_fetch::fetch_registry,
        &home.join(".canopy"),
        &mut std::io::stdout(),
    )
}

/// Testable core of `run_check_logins_cmd`.
pub(crate) fn run_check_logins_cmd_with(
    fetch: impl FnOnce() -> anyhow::Result<(crate::setup_module::models::RegistryRaw, String)>,
    canopy_dir: &Path,
    out: &mut dyn std::io::Write,
) -> i32 {
    let registry = match fetch() {
        Ok((r, _)) => r,
        Err(err) => {
            eprintln!("Error fetching registry: {err}");
            return 1;
        }
    };
    let installed: Vec<Platform> = registry
        .platforms
        .into_iter()
        .filter(is_platform_available)
        .collect();

    let statuses = check_all_parallel(&installed, canopy_dir);
    let status_map: std::collections::HashMap<String, LoginStatus> = statuses.into_iter().collect();

    let mut any_not_authenticated = false;
    for platform in &installed {
        let status = status_map
            .get(&platform.name)
            .copied()
            .unwrap_or(LoginStatus::Unknown);
        let line = match status {
            LoginStatus::Authenticated => format!("{}: authenticated", platform.name),
            LoginStatus::NotAuthenticated => {
                any_not_authenticated = true;
                format!("{}: not authenticated yet", platform.name)
            }
            LoginStatus::NotVerifiable | LoginStatus::NoCommand | LoginStatus::Unknown => {
                format!("{}: not verifiable", platform.name)
            }
        };
        let _ = writeln!(out, "{line}");
    }

    if any_not_authenticated {
        1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_module::models::{Platform, RegistryRaw};
    use std::collections::HashMap;

    fn platform(name: &str) -> Platform {
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
            provider: None,
            tool_name: None,
            project_paths: vec![],
            docs_url: None,
            free_tier: None,
            free_tier_checked_at: None,
            free_tier_source: None,
            install_paths: vec![],
            install_commands: vec![],
            login_command: None,
            login_check_command: None,
            login_checked_at: None,
            cli: Some(serde_json::json!({"binary": name})),
        }
    }

    fn registry(platforms: Vec<Platform>) -> RegistryRaw {
        RegistryRaw {
            platforms,
            canonical_servers: Default::default(),
        }
    }

    #[test]
    fn format_login_label_cases() {
        let mut p = platform("demo");
        assert_eq!(format_login_label(&p, None), "login: unknown");
        p.docs_url = Some("https://example.com".to_string());
        assert_eq!(
            format_login_label(&p, None),
            "login: see https://example.com"
        );

        p.login_command = Some("demo login".to_string());
        assert_eq!(
            format_login_label(&p, Some(LoginStatus::NotVerifiable)),
            "login: not verifiable"
        );
        assert_eq!(
            format_login_label(&p, Some(LoginStatus::Authenticated)),
            "authenticated"
        );
        assert_eq!(
            format_login_label(&p, Some(LoginStatus::NotAuthenticated)),
            "not authenticated yet"
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_check_success_failure_and_timeout() {
        assert!(run_check("true"));
        assert!(!run_check("false"));
        assert!(!run_check_with_timeout(
            "sleep 6",
            Duration::from_millis(100)
        ));
    }

    #[test]
    fn login_check_timeout_is_five_seconds() {
        assert_eq!(CHECK_TIMEOUT, Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn status_for_platform_respects_check_command() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut p = platform("demo");
        p.login_command = Some("demo login".to_string());
        assert_eq!(
            status_for_platform(&p, &canopy_dir),
            LoginStatus::NotVerifiable
        );

        p.login_check_command = Some("true".to_string());
        assert_eq!(
            status_for_platform(&p, &canopy_dir),
            LoginStatus::Authenticated
        );

        p.login_check_command = Some("false".to_string());
        assert_eq!(
            status_for_platform(&p, &canopy_dir),
            LoginStatus::NotAuthenticated
        );
    }

    #[test]
    fn resolve_login_platform_blocks_pending_without_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut p = platform("demo");
        p.login_command = Some("new login".to_string());
        crate::setup_module::registry_pending::save_pending(
            &canopy_dir,
            &[crate::setup_module::registry_pending::PendingChange {
                cli: "demo".to_string(),
                field: "login_command".to_string(),
                old: serde_json::json!("old login"),
                new: serde_json::json!("new login"),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();
        let err = resolve_login_platform(&canopy_dir, &p).unwrap_err();
        assert!(err.contains("canopy registry apply"));
    }

    #[test]
    fn resolve_login_platform_uses_approved_baseline_when_pending() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut baseline_p = platform("demo");
        baseline_p.login_command = Some("approved login".to_string());
        crate::domain::registry_baseline::RegistryBaseline {
            clis: vec![],
            platforms: vec![baseline_p],
        }
        .save(&canopy_dir)
        .unwrap();
        let mut fetched = platform("demo");
        fetched.login_command = Some("staged login".to_string());
        crate::setup_module::registry_pending::save_pending(
            &canopy_dir,
            &[crate::setup_module::registry_pending::PendingChange {
                cli: "demo".to_string(),
                field: "login_command".to_string(),
                old: serde_json::json!("approved login"),
                new: serde_json::json!("staged login"),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();
        let resolved = resolve_login_platform(&canopy_dir, &fetched).unwrap();
        assert_eq!(resolved.login_command.as_deref(), Some("approved login"));
    }

    #[test]
    fn run_login_cmd_non_tty_exits_1() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let p = platform("demo");
        let code = run_login_cmd_with(
            "demo",
            false,
            &canopy_dir,
            || Ok((registry(vec![p]), "test".to_string())),
            |_| unreachable!("should not run"),
        );
        assert_eq!(code, 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_check_logins_cmd_reports_format_and_exit_codes() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut ok = platform("ok");
        ok.cli = Some(serde_json::json!({"binary": "sh"}));
        ok.login_command = Some("sh -c true".to_string());
        ok.login_check_command = Some("true".to_string());

        let mut bad = platform("bad");
        bad.cli = Some(serde_json::json!({"binary": "ls"}));
        bad.login_command = Some("ls".to_string());
        bad.login_check_command = Some("false".to_string());

        let mut unverifiable = platform("nv");
        unverifiable.cli = Some(serde_json::json!({"binary": "pwd"}));
        unverifiable.login_command = Some("pwd".to_string());
        unverifiable.login_check_command = None;

        let mut out_buf = Vec::new();
        let code = run_check_logins_cmd_with(
            || {
                Ok((
                    registry(vec![ok.clone(), bad.clone(), unverifiable.clone()]),
                    "test".to_string(),
                ))
            },
            &canopy_dir,
            &mut out_buf,
        );
        let out = String::from_utf8(out_buf).unwrap();
        assert_eq!(code, 1);
        assert!(out.contains("ok: authenticated"), "{out}");
        assert!(out.contains("bad: not authenticated yet"), "{out}");
        assert!(out.contains("nv: not verifiable"), "{out}");

        let mut out_ok = Vec::new();
        let code_ok = run_check_logins_cmd_with(
            || Ok((registry(vec![ok, unverifiable]), "test".to_string())),
            &canopy_dir,
            &mut out_ok,
        );
        assert_eq!(code_ok, 0);
    }

    #[test]
    fn resolve_login_platform_gates_pending_login_check_command() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut baseline_p = platform("demo");
        baseline_p.login_command = Some("approved login".to_string());
        baseline_p.login_check_command = None;
        crate::domain::registry_baseline::RegistryBaseline {
            clis: vec![],
            platforms: vec![baseline_p],
        }
        .save(&canopy_dir)
        .unwrap();

        let mut fetched = platform("demo");
        fetched.login_command = Some("approved login".to_string());
        fetched.login_check_command = Some("staged-check".to_string());
        crate::setup_module::registry_pending::save_pending(
            &canopy_dir,
            &[crate::setup_module::registry_pending::PendingChange {
                cli: "demo".to_string(),
                field: "login_check_command".to_string(),
                old: serde_json::Value::Null,
                new: serde_json::json!("staged-check"),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();

        let resolved = resolve_login_platform(&canopy_dir, &fetched).unwrap();
        assert_eq!(resolved.login_command.as_deref(), Some("approved login"));
        assert_eq!(resolved.login_check_command, None);
    }

    #[cfg(unix)]
    #[test]
    fn status_for_platform_ignores_staged_login_check_command() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut p = platform("demo");
        p.login_command = Some("demo login".to_string());
        p.login_check_command = Some("true".to_string());
        crate::setup_module::registry_pending::save_pending(
            &canopy_dir,
            &[crate::setup_module::registry_pending::PendingChange {
                cli: "demo".to_string(),
                field: "login_check_command".to_string(),
                old: serde_json::Value::Null,
                new: serde_json::json!("true"),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();
        // Staged check must never execute; with no approved baseline it is not verifiable.
        assert_eq!(
            status_for_platform(&p, &canopy_dir),
            LoginStatus::NotVerifiable
        );

        // When an approved baseline check exists, run it instead of the staged one.
        let mut baseline_p = platform("demo");
        baseline_p.login_command = Some("demo login".to_string());
        baseline_p.login_check_command = Some("true".to_string());
        crate::domain::registry_baseline::RegistryBaseline {
            clis: vec![],
            platforms: vec![baseline_p],
        }
        .save(&canopy_dir)
        .unwrap();
        let mut staged_bad = platform("demo");
        staged_bad.login_command = Some("demo login".to_string());
        staged_bad.login_check_command = Some("false".to_string());
        crate::setup_module::registry_pending::save_pending(
            &canopy_dir,
            &[crate::setup_module::registry_pending::PendingChange {
                cli: "demo".to_string(),
                field: "login_check_command".to_string(),
                old: serde_json::json!("true"),
                new: serde_json::json!("false"),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();
        assert_eq!(
            status_for_platform(&staged_bad, &canopy_dir),
            LoginStatus::Authenticated
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_check_logins_cmd_ignores_staged_login_check_command() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut p = platform("demo");
        p.cli = Some(serde_json::json!({"binary": "sh"}));
        p.login_command = Some("demo login".to_string());
        p.login_check_command = Some("true".to_string());
        crate::setup_module::registry_pending::save_pending(
            &canopy_dir,
            &[crate::setup_module::registry_pending::PendingChange {
                cli: "demo".to_string(),
                field: "login_check_command".to_string(),
                old: serde_json::Value::Null,
                new: serde_json::json!("true"),
                fetched_at: "2026-11-26T00:00:00Z".to_string(),
            }],
        )
        .unwrap();

        let code = run_check_logins_cmd_with(
            || Ok((registry(vec![p]), "test".to_string())),
            &canopy_dir,
            &mut Vec::new(),
        );
        assert_eq!(code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn run_login_cmd_success_paths_match_check_command() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut p = platform("demo");
        p.cli = Some(serde_json::json!({"binary": "sh"}));
        p.login_command = Some("sh -c true".to_string());

        // No check command: login exit 0 is success.
        let no_check = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p.clone()]), "test".to_string())),
            |_| LoginRun::Exited(0),
        );
        assert_eq!(no_check, 0);

        // Passing check: success.
        p.login_check_command = Some("true".to_string());
        let ok = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p.clone()]), "test".to_string())),
            |_| LoginRun::Exited(0),
        );
        assert_eq!(ok, 0);

        // Failing check: exit 1 even though login exited 0.
        p.login_check_command = Some("false".to_string());
        let bad_check = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p.clone()]), "test".to_string())),
            |_| LoginRun::Exited(0),
        );
        assert_eq!(bad_check, 1);

        // Failed login never consults the check: exit 1 despite a passing check.
        p.login_check_command = Some("true".to_string());
        let bad_login = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p]), "test".to_string())),
            |_| LoginRun::Exited(2),
        );
        assert_eq!(bad_login, 1);
    }

    #[cfg(unix)]
    #[test]
    fn run_login_cmd_cancelled_or_failed_exits_1_even_if_check_passes() {
        let dir = tempfile::tempdir().unwrap();
        let canopy_dir = dir.path().join(".canopy");
        let mut p = platform("demo");
        p.cli = Some(serde_json::json!({"binary": "sh"}));
        p.login_command = Some("sh -c true".to_string());
        p.login_check_command = Some("true".to_string());

        let cancelled = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p.clone()]), "test".to_string())),
            |_| LoginRun::Cancelled,
        );
        assert_eq!(cancelled, 1);

        let failed = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p.clone()]), "test".to_string())),
            |_| LoginRun::Failed("boom".to_string()),
        );
        assert_eq!(failed, 1);

        let bad_exit = run_login_cmd_with(
            "demo",
            true,
            &canopy_dir,
            || Ok((registry(vec![p]), "test".to_string())),
            |_| LoginRun::Exited(2),
        );
        assert_eq!(bad_exit, 1);
    }
}
