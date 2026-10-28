use super::*;

/// Doctor's CLI-resolution section (B40): per-CLI report of resolved or
/// not, by which step, absolute path. Also warns when a CLI is reachable
/// from the current process's PATH but not from the daemon's captured PATH
/// (different environments).
pub(super) fn report_cli_resolution(
    config: &crate::domain::canopy_config::CanopyConfig,
    issues: &mut Vec<String>,
) {
    let daemon_path = crate::domain::cli_strategy::daemon_path();

    if config.clis.is_empty() {
        println!(" \x1b[33m⚠\x1b[0m No harnesses configured (run 'canopy setup')");
        issues.push("Run 'canopy setup' to detect and configure harnesses".to_string());
        return;
    }

    for cli_config in &config.clis {
        let label = doctor_cli_label(cli_config);
        match cli_config.resolve() {
            // declaration, partially: `resolve()`'s PATH-search step
            // (`which`) does confirm a real file, but its absolute-path
            // step accepts the string as-is without checking the file
            // exists — so a stale absolute path in config.toml would
            // otherwise resolve "Ok" and print a green tick for a
            // binary that isn't there. `binary_is_executable` (the
            // probe the service-unit check already uses) closes that
            // gap instead of duplicating a second existence check.
            Ok((resolved, step)) if !binary_is_executable(&resolved) => {
                println!(
                    " \x1b[31m✗\x1b[0m {} → {} (via {} — file missing or not executable)",
                    label,
                    resolved.display(),
                    step.label()
                );
                issues.push(format!(
                    "'{}' resolved to {} but that file is missing or not executable.",
                    cli_config.name,
                    resolved.display()
                ));
            }
            Ok((resolved, step)) => {
                // CB44: identity check — the resolved binary may be a
                // different program answering to the same bare name
                // (e.g. `/usr/bin/blackbox` is the Blackbox X11 window
                // manager, not the Blackbox CLI). Runs once per doctor
                // invocation, never on dispatch. Platforms with no
                // declared check behave exactly as today.
                if let Some(wb) = diagnose_cli_identity(cli_config, &resolved) {
                    let check = cli_config
                        .identity_check
                        .as_ref()
                        .map_or("", |c| c.contains.as_str());
                    println!(
                        " \x1b[31m✗\x1b[0m {} → {} (via {} — wrong binary: expected '{}' in output; saw: {})",
                        label,
                        resolved.display(),
                        step.label(),
                        check,
                        wb.output,
                    );
                    let check_cmd = cli_config
                        .identity_check
                        .as_ref()
                        .map_or("", |c| c.cmd.as_str());
                    issues.push(wb.report(&cli_config.binary, check_cmd));
                    continue;
                }
                // CB85: identity_args/identity_pattern — the same verdict the
                // probe and dispatch paths produce, cached per path+mtime so
                // a doctor run checks each binary once. A mismatch is reported
                // as not-installed rather than a skew warning.
                if let Some(error) = diagnose_binary_identity(cli_config, &resolved) {
                    println!(
                        " \x1b[31m✗\x1b[0m {} → {} (via {} — {})",
                        label,
                        resolved.display(),
                        step.label(),
                        error.message()
                    );
                    issues.push(error.message());
                    continue;
                }
                // Check daemon reachability: does the binary also
                // resolve under the daemon's captured PATH?
                let daemon_reachable = match &daemon_path {
                    Some(dp) => cli_config.resolve_against(dp).is_ok(),
                    // No daemon PATH (macOS/launchd) → no mismatch possible
                    None => true,
                };
                if daemon_reachable {
                    success(format!(
                        "{} → {} (via {})",
                        label,
                        resolved.display(),
                        step.label()
                    ));
                } else {
                    println!(
                        " \x1b[33m⚠\x1b[0m {} → {} (via {} — reachable now but NOT from the daemon)",
                        label,
                        resolved.display(),
                        step.label()
                    );
                    issues.push(format!(
                        "'{}' is on your interactive PATH but not the daemon's. \
                         Re-run `canopy daemon install` to update the daemon's PATH, \
                         or add the directory to the systemd unit's Environment=PATH=.",
                        cli_config.name
                    ));
                }
            }
            Err(e) => {
                println!(" \x1b[31m✗\x1b[0m {} — not found ({})", label, e);
                issues.push(format!(
                    "'{}' binary '{}' not found. {}",
                    cli_config.name, e.binary, e.path
                ));
            }
        }
    }
}

/// Where this platform's service unit lives, and which manager owns it.
/// `None` on platforms with no supported service manager (doctor's
/// service-unit section is then simply skipped).
pub(super) fn service_unit_location(home: &Path) -> Option<(&'static str, PathBuf)> {
    if cfg!(target_os = "macos") {
        Some((
            "launchd",
            home.join("Library/LaunchAgents/com.canopy.plist"),
        ))
    } else if cfg!(target_os = "linux") {
        Some((
            "systemd",
            home.join(".config/systemd/user").join("canopy.service"),
        ))
    } else {
        None
    }
}

/// Extract the binary path named by a systemd unit's `ExecStart=` line — the
/// first whitespace-separated token, before its arguments (`serve --port
/// ...`). `None` when the unit has no `ExecStart=` line.
pub(super) fn parse_systemd_exec_start_binary(unit_content: &str) -> Option<PathBuf> {
    unit_content
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .and_then(|rest| rest.split_whitespace().next())
        .map(PathBuf::from)
}

/// Extract the binary path named by a launchd plist's `ProgramArguments`
/// array — its first `<string>` entry, before `serve`, `--port`, `<port>`.
/// `None` when the plist has no `ProgramArguments` array or it's empty.
pub(super) fn parse_launchd_program_binary(plist_content: &str) -> Option<PathBuf> {
    let after_key = plist_content.split_once("<key>ProgramArguments</key>")?.1;
    let after_array = after_key.split_once("<array>")?.1;
    let inside_string = after_array.split_once("<string>")?.1;
    let (binary, _) = inside_string.split_once("</string>")?;
    Some(PathBuf::from(binary.trim()))
}

/// Extract the binary a service unit's contents name, dispatching on which
/// manager owns it. Pure — takes the unit's contents as a string rather than
/// a path, so the systemd/launchd formats are tested without real unit
/// files or a live service manager.
pub(super) fn parse_unit_binary(manager: &str, unit_content: &str) -> Option<PathBuf> {
    match manager {
        "launchd" => parse_launchd_program_binary(unit_content),
        _ => parse_systemd_exec_start_binary(unit_content),
    }
}

/// CB70: does this systemd unit's content define a non-empty Environment=BROWSER= ?
/// Pure over &str so tests drive it without real unit files.
pub(super) fn unit_defines_browser(unit_content: &str) -> bool {
    unit_content.lines().any(|line| {
        // CB73: drop-in files are often hand-written with indented lines —
        // accept ` Environment=BROWSER=...` the same as a flush-left one.
        line.trim_start()
            .strip_prefix("Environment=BROWSER=")
            .is_some_and(|v| {
                // Mirror parse_existing_browser_env: one layer of surrounding
                // quotes is not a value — `Environment=BROWSER=""` is empty.
                let unquoted = if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
                    &v[1..v.len() - 1]
                } else {
                    v
                };
                !unquoted.is_empty()
            })
    })
}

/// CB73 FR1: is BROWSER set in the *effective* environment systemd would
/// launch the unit with? Parses the stdout of
/// `systemctl --user show canopy.service -p Environment` — one
/// `Environment=...` line that already merges every drop-in. Pure over
/// &str so tests inject canned output instead of shelling out.
pub(super) fn browser_set_in_systemctl_show(output: &str) -> bool {
    let Some(rest) = output
        .lines()
        .find_map(|line| line.strip_prefix("Environment="))
    else {
        return false;
    };
    split_env_tokens(rest).into_iter().any(|token| {
        token.strip_prefix("BROWSER=").is_some_and(|v| {
            // Same one-layer quote rule as `unit_defines_browser`: the
            // quotes wrap a value, they aren't one.
            let unquoted = if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
                &v[1..v.len() - 1]
            } else {
                v
            };
            !unquoted.is_empty()
        })
    })
}

/// Split an `Environment=` remainder into whitespace-separated tokens,
/// keeping one layer of double quotes so `BROWSER="/x/my browser"` stays a
/// single token. Tiny on purpose — no shell-word crate for one check.
pub(super) fn split_env_tokens(rest: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut in_quotes = false;
    for (i, c) in rest.char_indices() {
        if c == '"' {
            in_quotes = !in_quotes;
            if start.is_none() {
                start = Some(i);
            }
        } else if c.is_whitespace() && !in_quotes {
            if let Some(from) = start.take() {
                tokens.push(&rest[from..i]);
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(from) = start {
        tokens.push(&rest[from..]);
    }
    tokens
}

/// CB73 FR2: is BROWSER set in a process's raw `/proc/<pid>/environ`
/// bytes? NUL-separated `KEY=value` entries; values are literal
/// environment values, so no quote stripping applies here. Pure over
/// `&[u8]` so tests inject bytes rather than reading `/proc`.
pub(super) fn browser_set_in_proc_environ(data: &[u8]) -> bool {
    data.split(|&b| b == 0).any(|entry| {
        entry.strip_prefix(b"BROWSER=").is_some_and(|v| {
            // Environ values are literal (no quote rule); a stray trailing
            // newline isn't part of the value.
            !v.strip_suffix(b"\n").unwrap_or(v).is_empty()
        })
    })
}

/// CB73 FR2+FR3: the verdict doctor prints for the BROWSER check. An enum
/// (not threaded booleans) so every combination of "unit effective
/// environment" × "running daemon's environ" is an exhaustive, testable
/// arm and the fix hint can only be attached to the arms where the
/// effective unit value is missing.
// The shared `Unit` prefix is deliberate: every variant is a verdict about
// the unit's effective environment, so the prefix is meaning, not noise.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BrowserEnvStatus {
    /// No orphan to judge, and the effective environment has BROWSER —
    /// success, no hint.
    UnitSet,
    /// No orphan to judge, and the effective environment lacks BROWSER —
    /// warning + fix hint.
    UnitMissing,
    /// The orphan serving the port has BROWSER and so does the unit's
    /// effective environment — success with both labels; no hint (FR3:
    /// nothing is missing from the effective environment).
    UnitSetOrphanSet,
    /// Unit is fine but the orphan actually serving the port lacks
    /// BROWSER — the daemon must be restarted through the unit. No
    /// drop-in hint: the unit already defines it.
    UnitSetOrphanMissing,
    /// The orphan has BROWSER today but the unit's effective environment
    /// lacks it — a restart through the unit would drop it. FR3 allows
    /// the hint here (the effective unit environment IS missing).
    UnitMissingOrphanSet,
    /// Neither the effective unit environment nor the orphan's environ
    /// has BROWSER — warning + fix hint + both labels.
    UnitMissingOrphanMissing,
}

/// Combine the two observed facts into the verdict. `orphan_environ_set`
/// is `None` when there is no orphan to judge (port holder is the unit's
/// MainPID, nothing holds the port, or no manager), `Some(..)` otherwise.
/// FR2: an orphan present means BOTH labels get reported whatever their
/// values — every combination is its own arm, none collapsed, so a
/// running daemon that HAS BROWSER is never printed as "missing".
pub(super) fn diagnose_browser_env(
    unit_set: bool,
    orphan_environ_set: Option<bool>,
) -> BrowserEnvStatus {
    match (unit_set, orphan_environ_set) {
        (true, None) => BrowserEnvStatus::UnitSet,
        (false, None) => BrowserEnvStatus::UnitMissing,
        (true, Some(true)) => BrowserEnvStatus::UnitSetOrphanSet,
        (true, Some(false)) => BrowserEnvStatus::UnitSetOrphanMissing,
        (false, Some(true)) => BrowserEnvStatus::UnitMissingOrphanSet,
        (false, Some(false)) => BrowserEnvStatus::UnitMissingOrphanMissing,
    }
}

/// What doctor should report about a service unit's binary, given facts a
/// caller has already gathered by touching the filesystem/PATH. Pure
/// comparison — no I/O — so every branch is reachable from synthetic inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServiceUnitBinaryStatus {
    /// The unit's binary is missing or not executable — the exact failure
    /// mode that leaves systemd/launchd retry-looping with nothing on the
    /// port.
    Missing,
    /// The unit's binary exists but isn't the same file as the `canopy` on
    /// PATH.
    Skew(PathBuf),
    /// The unit's binary exists and either matches the one on PATH, or
    /// there's nothing on PATH to disagree with it.
    Consistent,
}

pub(super) fn diagnose_service_unit_binary(
    unit_binary: &Path,
    binary_exists: bool,
    path_binary: Option<&Path>,
) -> ServiceUnitBinaryStatus {
    if !binary_exists {
        return ServiceUnitBinaryStatus::Missing;
    }
    match path_binary {
        Some(p) if p != unit_binary => ServiceUnitBinaryStatus::Skew(p.to_path_buf()),
        _ => ServiceUnitBinaryStatus::Consistent,
    }
}

/// Does `path` exist and carry an execute bit? Doctor only ever reads unit
/// files and stats binaries — this never shells out to `systemctl`, so it
/// works the same in a CI container as on a developer's machine.
#[cfg(unix)]
pub(super) fn binary_is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
pub(super) fn binary_is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Human label for a platform in doctor output: the product display name
/// ("Provider · Tool" when the registry knows both), with the slug in
/// brackets when it differs so the user still knows what to type in
/// config.toml and flags. A platform with no provider/tool_name renders
/// the legacy exact slug string. CM35: a platform marked `headless = false`
/// is appended ` (interactive only)` so doctor names it as interactive-only
/// rather than implying it can run a graph node. Delegates to
/// `CliConfig::display_name`; never used for matching.
pub(super) fn doctor_cli_label(cli_config: &crate::domain::cli_config::CliConfig) -> String {
    let display = cli_config.display_name();
    let base = if display == cli_config.name {
        display
    } else {
        format!("{} [{}]", display, cli_config.name)
    };
    if cli_config.headless {
        base
    } else {
        format!("{base} (interactive only)")
    }
}

/// CB44: run the platform's registry-declared identity check against the
/// already-resolved absolute path. `Some(error)` exactly when the binary
/// does not identify as this platform (wrong program answering to the bare
/// name); `None` when no check is declared (backward compatible) or the
/// check passes. Pure diagnosis — never called on dispatch.
pub(crate) fn diagnose_cli_identity(
    cli_config: &crate::domain::cli_config::CliConfig,
    resolved: &Path,
) -> Option<crate::domain::cli_strategy::WrongBinaryError> {
    let _check = cli_config.identity_check.as_ref()?;
    crate::domain::cli_strategy::verify_identity(cli_config, resolved).err()
}

/// CB85: run the platform's registry-declared `identity_args` /
/// `identity_pattern` against the already-resolved absolute path — the
/// dispatch-enforced sibling of [`diagnose_cli_identity`]. `Some(error)`
/// exactly when the binary does not identify as this platform; `None` when
/// the platform declares neither field (backward compatible), its binary
/// does not resolve (the line above already reported that), or the check
/// passes. Cached per path+mtime, so repeated doctor runs in one process
/// check once.
pub(crate) fn diagnose_binary_identity(
    cli_config: &crate::domain::cli_config::CliConfig,
    resolved: &Path,
) -> Option<crate::domain::cli_strategy::NotInstalled> {
    let identity = crate::domain::cli_strategy::BinaryIdentity::from_cli_config(cli_config)?;
    crate::domain::cli_strategy::ensure_binary_identity_blocking(resolved, &identity).err()
}

/// Best-effort `<binary> --version` output, trimmed. `None` on any failure —
/// doctor reports a skew warning either way, just without a version string
/// to show alongside a path that couldn't be run.
pub(super) fn binary_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Report a lingering legacy/new split for the data-layout migrations in
/// `usage_stats` and `models_db`: `usage.json` next to `usage.toml`, or a
/// legacy `models_cache.json` / `models_native_<cli>.json` next to their
/// `cache/` counterparts. Both migrations defer deleting the legacy side
/// while `state_pid` shows a daemon may still be using it, so this line is
/// what lets an operator tell that expected, self-clearing wait apart from
/// a migration that's actually stuck.
pub(super) fn report_layout_split(
    canopy_dir: &Path,
    state_pid: Option<u32>,
    issues: &mut Vec<String>,
) {
    let usage_split =
        canopy_dir.join("usage.json").exists() && canopy_dir.join("usage.toml").exists();

    let legacy_native_caches: Vec<String> = std::fs::read_dir(canopy_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let name = entry.file_name().to_str()?.to_string();
                    (name.starts_with("models_native_") && name.ends_with(".json")).then_some(name)
                })
                .collect()
        })
        .unwrap_or_default();
    let legacy_catalog_cache = canopy_dir.join("models_cache.json").exists();
    let cache_split = legacy_catalog_cache || !legacy_native_caches.is_empty();

    if !usage_split && !cache_split {
        return;
    }

    println!(" \x1b[33m⚠\x1b[0m Legacy/new data layout split detected:");
    if usage_split {
        println!("     usage.json (legacy) and usage.toml (current) both exist");
    }
    if legacy_catalog_cache {
        println!(
            "     models_cache.json (legacy) and cache/models_catalog.json (current) both exist"
        );
    }
    for name in &legacy_native_caches {
        println!("     {name} (legacy) and cache/{name} (current) both exist");
    }

    if let Some(pid) = state_pid {
        println!(
            "     a canopy daemon (PID: {pid}) is running, so cleanup of the legacy file(s) is deferred until it stops"
        );
        issues.push(
            "Legacy/new data layout split detected, deferred because a daemon is running — it will clean up on a future run once no daemon is live.".to_string(),
        );
    } else {
        issues.push(
            "Legacy/new data layout split detected with no daemon running — re-run any canopy command to clean up the legacy file(s).".to_string(),
        );
    }
}

/// CB73 FR1 (I/O): the unit's *effective* environment as systemd reports
/// it (`systemctl --user show canopy.service -p Environment` merges the
/// unit file with every drop-in). `None` when `systemctl` is missing or
/// fails — the check then falls back to parsing the files. Tests never
/// call this; they inject a canned string into
/// `read_effective_unit_browser` instead.
pub(super) fn read_systemctl_show_environment() -> Option<String> {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "show", "canopy.service", "-p", "Environment"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// CB81 FR3: read `OOMPolicy` from systemd (`systemctl --user show
/// canopy.service -p OOMPolicy`), the sibling of `read_systemctl_show_environment`.
/// `None` when systemctl is missing or fails — the caller then skips the check
/// instead of false-positiving. Tests never call this; they inject a canned
/// string into `oom_policy_is_stop` / `report_service_unit` instead.
pub(super) fn read_systemctl_show_oom_policy() -> Option<String> {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "show", "canopy.service", "-p", "OOMPolicy"])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// CB81 FR3: parse the stdout of `systemctl --user show canopy.service -p
/// OOMPolicy` and return whether it reports `stop`. Pure over `&str` so tests
/// drive it without a live service manager. `false` for `continue`/`kill`, for
/// an absent property line, or for empty output; a `None` output (systemctl
/// unavailable) is handled by the caller skipping the check entirely.
pub(super) fn oom_policy_is_stop(output: &str) -> bool {
    output
        .lines()
        .find_map(|line| line.strip_prefix("OOMPolicy="))
        .is_some_and(|value| value.trim() == "stop")
}

/// CB73 FR2 (I/O): raw `/proc/<pid>/environ` bytes for the process
/// holding the daemon port, or `None` if it is gone/unreadable. Tests
/// never call this; they inject bytes into `report_service_unit`.
pub(super) fn read_proc_environ(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{pid}/environ")).ok()
}

/// CB73 FR1: BROWSER in the effective environment. The injected
/// `systemctl show` output (systemd's own merged view) is authoritative;
/// only when it is `None` (systemctl unavailable) does this fall back to
/// the unit file plus every `*.conf` drop-in under the sibling
/// `canopy.service.d/`, sorted by file name.
pub(super) fn read_effective_unit_browser(
    unit_path: &Path,
    systemctl_output: Option<&str>,
) -> bool {
    if let Some(output) = systemctl_output {
        return browser_set_in_systemctl_show(output);
    }
    let mut combined = std::fs::read_to_string(unit_path).unwrap_or_default();
    if let Some(dir) = unit_path.parent() {
        let dropin_dir = dir.join("canopy.service.d");
        let mut dropins: Vec<PathBuf> = std::fs::read_dir(&dropin_dir)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|p| p.extension() == Some(std::ffi::OsStr::new("conf")))
                    .collect()
            })
            .unwrap_or_default();
        dropins.sort();
        for dropin in dropins {
            if let Ok(content) = std::fs::read_to_string(&dropin) {
                combined.push('\n');
                combined.push_str(&content);
            }
        }
    }
    unit_defines_browser(&combined)
}

/// CB81 FR3: exact warning/issue text shared by the printed ⚠ line and the
/// pushed issue so the verbiage lives in one place. `systemctl show -p OOMPolicy`
/// reports `stop` when the unit has no drop-in/inline value (systemd's default)
/// or a drop-in that explicitly sets it — either way a node OOM-kill takes the
/// whole unit down, so the fix is to reinstall. Module-private: only
/// `report_service_unit` consumes it (same file).
const OOM_POLICY_STOP_MESSAGE: &str = "canopy service unit keeps OOMPolicy=stop: a node that runs out of memory stops the daemon — reinstall the service (canopy daemon install) to get OOMPolicy=continue";

/// Report on the systemd/launchd service unit, if any: its path, the binary
/// it names, and whether that binary is the problem. Running the daemon by
/// hand instead of via a unit is legitimate, so no unit at all is a neutral
/// informational line, not a warning.
///
/// CB73: `port_pid`/`manager_pid` are the facts `run_doctor` already
/// computed (no second /proc scan), `systemctl_output` is the injected
/// `systemctl --user show -p Environment` stdout, and `orphan_environ` is
/// the injected raw `/proc/<port_pid>/environ` bytes — present only when
/// that port holder is an orphan (not the unit's MainPID). Tests inject
/// these directly; production fills them via the thin readers below.
pub(super) fn report_service_unit(
    home: &Path,
    port_pid: Option<u32>,
    manager_pid: Option<u32>,
    systemctl_output: Option<&str>,
    orphan_environ: Option<&[u8]>,
    oom_policy_output: Option<&str>,
    issues: &mut Vec<String>,
) {
    let Some((manager, unit_path)) = service_unit_location(home) else {
        return;
    };

    let Ok(unit_content) = std::fs::read_to_string(&unit_path) else {
        println!(
            " \x1b[90m–\x1b[0m No {manager} service unit installed (running the daemon by hand is fine)"
        );
        return;
    };

    // declaration: the unit file being readable is exactly what's claimed
    // here — whether the binary it points at actually works is the
    // separate, capability-checked line below.
    success(format!("Service unit ({manager}): {}", unit_path.display()));

    let Some(unit_binary) = parse_unit_binary(manager, &unit_content) else {
        println!("     \x1b[33m⚠\x1b[0m Could not find the binary the unit points at");
        return;
    };

    let binary_exists = binary_is_executable(&unit_binary);
    let path_binary = which::which("canopy").ok();

    match diagnose_service_unit_binary(&unit_binary, binary_exists, path_binary.as_deref()) {
        ServiceUnitBinaryStatus::Missing => {
            println!(
                "     \x1b[31m✗\x1b[0m Points at a missing or non-executable binary: {}",
                unit_binary.display()
            );
            issues.push(format!(
                "Service unit's binary is gone ({}) — run 'canopy daemon install' to reinstall the service so it points at the current binary.",
                unit_binary.display()
            ));
        }
        ServiceUnitBinaryStatus::Skew(path_binary) => {
            let unit_version =
                binary_version(&unit_binary).unwrap_or_else(|| "unknown".to_string());
            let path_version =
                binary_version(&path_binary).unwrap_or_else(|| "unknown".to_string());
            println!("     \x1b[33m⚠\x1b[0m Unit binary differs from the canopy on PATH:");
            println!("         Unit: {} ({unit_version})", unit_binary.display());
            println!("         PATH: {} ({path_version})", path_binary.display());
            issues.push(
                "The service unit and the canopy on your PATH are different binaries — \
                 run 'canopy daemon install' to update the service."
                    .to_string(),
            );
        }
        // capability: reached only when `binary_is_executable` confirmed
        // the file on disk, not just that the unit names some path.
        ServiceUnitBinaryStatus::Consistent => {
            success_nested(format!("Binary: {}", unit_binary.display()));
        }
    }

    if manager == "systemd" {
        if let Some(policy) = oom_policy_output {
            if oom_policy_is_stop(policy) {
                println!("     \x1b[33m⚠\x1b[0m {}", OOM_POLICY_STOP_MESSAGE);
                issues.push(OOM_POLICY_STOP_MESSAGE.to_string());
            }
        }
        let unit_set = read_effective_unit_browser(&unit_path, systemctl_output);
        // CB73 FR2: the environ bytes only count when they came from a
        // real orphan — the port holder exists and is not the unit's
        // MainPID. Otherwise there is no second opinion to report.
        let orphan_set = match orphan_environ {
            Some(environ) if port_pid.is_some() && manager_pid != port_pid => {
                Some(browser_set_in_proc_environ(environ))
            }
            _ => None,
        };
        match diagnose_browser_env(unit_set, orphan_set) {
            BrowserEnvStatus::UnitSet => {
                success_nested(
                    "Browser env: BROWSER is set (login/connection pages can open)".to_string(),
                );
            }
            // FR2: an orphan means both labels are reported even when
            // nothing is missing — the user must see WHOSE environment
            // is actually carrying the daemon.
            BrowserEnvStatus::UnitSetOrphanSet => {
                success_nested(
                    "Browser env: unit: set, running daemon: set (orphan) — BROWSER is set (login/connection pages can open)".to_string(),
                );
            }
            BrowserEnvStatus::UnitMissing => {
                println!("     \x1b[33m⚠\x1b[0m Unit defines no Environment=BROWSER= — harnesses and MCP servers started by the daemon cannot open login or connection pages");
                println!("     Fix: mkdir -p ~/.config/systemd/user/canopy.service.d && printf '[Service]\\nEnvironment=BROWSER=<your-browser-cmd>\\n' > ~/.config/systemd/user/canopy.service.d/browser.conf && systemctl --user daemon-reload && systemctl --user restart canopy.service");
                issues.push("Daemon unit has no BROWSER — harnesses/MCP servers cannot open login or connection pages. Re-run `canopy daemon install` with BROWSER set, or add the drop-in shown above.".to_string());
            }
            // The unit already defines BROWSER (the drop-in the fix hint
            // would create exists) — so no hint; the defect is the orphan.
            BrowserEnvStatus::UnitSetOrphanMissing => {
                println!("     \x1b[33m⚠\x1b[0m unit: set, running daemon: missing (orphan) — restart the daemon through the unit so it inherits the drop-in");
                issues.push("Running daemon is an orphan without BROWSER (unit: set, running daemon: missing (orphan)) — run 'canopy daemon stop' then start through systemd.".to_string());
            }
            // The orphan's environ HAS BROWSER today, but the unit's
            // effective environment lacks it — FR2 requires reporting
            // `running daemon: set`, never "missing"; FR3 allows the
            // hint because the effective unit value IS missing.
            BrowserEnvStatus::UnitMissingOrphanSet => {
                println!("     \x1b[33m⚠\x1b[0m Unit defines no Environment=BROWSER= — harnesses and MCP servers started by the daemon cannot open login or connection pages");
                println!("     \x1b[33m⚠\x1b[0m unit: missing, running daemon: set (orphan) — the running daemon has BROWSER but the unit does not; apply the fix and restart through the unit so a plain restart does not drop it");
                println!("     Fix: mkdir -p ~/.config/systemd/user/canopy.service.d && printf '[Service]\\nEnvironment=BROWSER=<your-browser-cmd>\\n' > ~/.config/systemd/user/canopy.service.d/browser.conf && systemctl --user daemon-reload && systemctl --user restart canopy.service");
                issues.push("Daemon unit has no BROWSER — harnesses/MCP servers cannot open login or connection pages. Re-run `canopy daemon install` with BROWSER set, or add the drop-in shown above.".to_string());
            }
            // Effective unit missing ⇒ the fix hint is allowed (FR3), and
            // FR2 requires both labels on orphan reports.
            BrowserEnvStatus::UnitMissingOrphanMissing => {
                println!("     \x1b[33m⚠\x1b[0m Unit defines no Environment=BROWSER= — harnesses and MCP servers started by the daemon cannot open login or connection pages");
                println!("     \x1b[33m⚠\x1b[0m unit: missing, running daemon: missing (orphan) — restart the daemon through the unit so it inherits the drop-in");
                println!("     Fix: mkdir -p ~/.config/systemd/user/canopy.service.d && printf '[Service]\\nEnvironment=BROWSER=<your-browser-cmd>\\n' > ~/.config/systemd/user/canopy.service.d/browser.conf && systemctl --user daemon-reload && systemctl --user restart canopy.service");
                issues.push("Daemon unit has no BROWSER — harnesses/MCP servers cannot open login or connection pages. Re-run `canopy daemon install` with BROWSER set, or add the drop-in shown above.".to_string());
            }
        }
    }
}

/// CB52 FR5: list orphaned join nodes — `join`-kind nodes with no ensemble
/// row, the state databases damaged before the entry/exit FKs became
/// `RESTRICT` are left in — grouped per graph. Silent when there is no
/// database yet, when it cannot be opened, or when nothing is orphaned.
/// Never auto-repairs (C1): the issue pushed names the manual verbs.
pub(super) fn report_orphan_joins(canopy_dir: &Path, issues: &mut Vec<String>) {
    let db_path = database_path(canopy_dir);
    if !db_path.exists() {
        return;
    }
    let Ok(db) = Database::new_safe(&db_path, canopy_dir) else {
        return;
    };
    let Ok(orphans) = db.list_all_orphan_join_nodes() else {
        return;
    };
    if orphans.is_empty() {
        return;
    }
    println!(
        " \x1b[31m✗\x1b[0m Orphaned join nodes (CB52): {} join node(s) belong to no ensemble",
        orphans.len()
    );
    for node in &orphans {
        let scope = match (&node.spec_id, &node.graph_id) {
            (Some(spec_id), _) => match db.get_graph_spec(spec_id) {
                Ok(Some(spec)) => format!("spec '{}' ({spec_id})", spec.name),
                _ => format!("spec '{spec_id}'"),
            },
            (_, Some(graph_id)) => match db.get_graph(graph_id) {
                Ok(Some(graph)) => format!("graph '{}' ({graph_id})", graph.name),
                _ => format!("graph '{graph_id}'"),
            },
            _ => "no graph scope".to_string(),
        };
        println!(
            "     - '{}' ({}) in {} — belongs to no ensemble",
            node.name, node.id, scope
        );
    }
    issues.push(
        "Orphaned join nodes belong to no ensemble — run 'graph get' to see orphan_join_warnings and delete them with graph_node delete or repair via graph_ensemble update (see CB52)."
            .to_string(),
    );
}
