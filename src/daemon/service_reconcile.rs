//! CB75: keep the installed service unit pointed at the binary that is
//! actually asking to start it, and say why a start failed.
//!
//! Measured 2026-09-27 on a second machine: a unit left behind by a past
//! `cargo install` still had `ExecStart=/home/lina/.cargo/bin/canopy serve`
//! after the installer moved the binary to `~/.local/bin/canopy`.
//! `canopy daemon start` dutifully ran `systemctl --user start`, systemd
//! looped `status=203/EXEC`, and the only message the user saw was "the port
//! never opened". This module closes both halves of that gap.

use anyhow::Result;
use std::path::{Path, PathBuf};

use super::process::{CommandRunner, RealRunner, SYSTEMD_UNIT_NAME};
use super::service_install;

/// CB75 FR1: the binary named by a systemd unit's `ExecStart=` line — its
/// first whitespace-separated token, before the arguments (`serve --port`).
/// `None` when the unit has no `ExecStart=` line.
pub(crate) fn unit_exec_start_binary(unit_content: &str) -> Option<PathBuf> {
    unit_content
        .lines()
        .find_map(|line| line.strip_prefix("ExecStart="))
        .and_then(|rest| rest.split_whitespace().next())
        .map(PathBuf::from)
}

/// CB75 FR1: does the unit need to be re-rendered for `current_exe`? `true`
/// when the unit names no binary at all, or names one that is either missing
/// from disk or a different file.
///
/// Both paths go through `canonicalize` when both resolve, so a unit pointing
/// at a symlink to the same binary is never rewritten on every start.
pub(crate) fn exec_start_needs_update(unit_binary: Option<&Path>, current_exe: &Path) -> bool {
    let Some(old) = unit_binary else {
        return true;
    };
    if old == current_exe {
        return false;
    }
    match (old.canonicalize(), current_exe.canonicalize()) {
        (Ok(old), Ok(new)) => old != new,
        // The old binary is gone (the reported bug) or the current one cannot
        // be resolved: both are drift.
        _ => true,
    }
}

/// What the reconcile changed, for the caller and for tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnitReconcile {
    /// What the unit named before the rewrite; `None` when it had no
    /// `ExecStart=` line to compare.
    pub(crate) old: Option<PathBuf>,
    pub(crate) new: PathBuf,
}

/// CB75 FR1's user-visible line. Pure so the exact wording the spec asks for
/// is testable — the `println!` in [`reconcile_unit_binary`] prints this and
/// nothing else.
pub(crate) fn reconcile_message(old: Option<&Path>, new: &Path) -> String {
    let old_label = match old {
        Some(old) => old.display().to_string(),
        None => "no ExecStart line".to_string(),
    };
    format!(
        "service unit pointed at {old_label}; updated to {}",
        new.display()
    )
}

/// CB75 FR1: make the unit at `unit_path` point at `exe`.
///
/// `Some(..)` when the unit was rewritten (and the manager reloaded and its
/// failed state cleared), `None` when there is no unit file, when it already
/// pointed at `exe`, or when it names no binary to compare.
///
/// The rewrite goes through `service_install::write_systemd_unit`, so the
/// PATH/BROWSER/DISPLAY/WAYLAND_DISPLAY reconciliation is exactly the one
/// `daemon install-service` uses — there is no second renderer to drift.
pub(crate) fn reconcile_unit_binary(
    runner: &dyn CommandRunner,
    unit_path: &Path,
    exe: &Path,
    port: u16,
) -> Result<Option<UnitReconcile>> {
    let Ok(unit_content) = std::fs::read_to_string(unit_path) else {
        return Ok(None);
    };
    let named = unit_exec_start_binary(&unit_content);
    if !exec_start_needs_update(named.as_deref(), exe) {
        return Ok(None);
    }

    service_install::write_systemd_unit(exe, port, unit_path)?;

    // A unit that has already hit its start limit stays `failed` until it is
    // cleared, and a rewritten `ExecStart` is only re-read after a reload —
    // without both, the very next `start` fails again for the old reason.
    let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
    let _ = runner.run("systemctl", &["--user", "reset-failed", SYSTEMD_UNIT_NAME]);

    let outcome = UnitReconcile {
        old: named,
        new: exe.to_path_buf(),
    };
    // Both fields are read here in production code, not only by tests: a
    // field only `#[cfg(test)]` code reads trips `dead_code` in the
    // non-test target `clippy --all-targets` also compiles.
    println!(
        "{}",
        reconcile_message(outcome.old.as_deref(), &outcome.new)
    );
    Ok(Some(outcome))
}

/// CB75 FR1: the production entry point. No unit, or a platform whose unit is
/// not a systemd unit, yields `None`.
pub(crate) fn reconcile_unit_binary_live(exe: &Path, port: u16) -> Option<UnitReconcile> {
    let unit_path = live_unit_path()?;
    reconcile_unit_binary(&RealRunner, &unit_path, exe, port)
        .ok()
        .flatten()
}

#[cfg(target_os = "linux")]
fn live_unit_path() -> Option<PathBuf> {
    service_install::systemd_unit_path().ok()
}

#[cfg(not(target_os = "linux"))]
fn live_unit_path() -> Option<PathBuf> {
    // macOS plists are rewritten whole by `daemon install-service`; launchd
    // re-reads a plist on load, so there is nothing to reconcile here.
    None
}

/// What a failed start is diagnosed from. Both fields are `None` when the
/// facts could not be gathered — the message then says so instead of guessing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StartFailureInfo {
    /// Raw stdout of `systemctl --user show canopy.service -p
    /// ActiveState,SubState,ExecMainStatus,Result`.
    pub(crate) show: Option<String>,
    /// The binary the unit's `ExecStart=` names, read from the unit file.
    pub(crate) unit_binary: Option<PathBuf>,
}

/// The four properties FR3 names. `None` per field when absent, so the
/// printed message can show `?` rather than an empty value.
#[derive(Default)]
struct ShowProperties {
    active_state: Option<String>,
    sub_state: Option<String>,
    exec_main_status: Option<String>,
    result: Option<String>,
}

fn parse_show_properties(output: &str) -> ShowProperties {
    let mut props = ShowProperties::default();
    for line in output.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let slot = match key.trim() {
            "ActiveState" => &mut props.active_state,
            "SubState" => &mut props.sub_state,
            "ExecMainStatus" => &mut props.exec_main_status,
            "Result" => &mut props.result,
            _ => continue,
        };
        *slot = Some(value.trim().to_string());
    }
    props
}

/// CB75 FR3: name the cause of a start that never opened the port, given the
/// unit's `systemctl show` output and the binary its `ExecStart=` names.
/// Pure, so every branch is reachable without a service manager.
pub(crate) fn describe_start_failure(show: Option<&str>, unit_binary: Option<&Path>) -> String {
    let Some(show) = show else {
        return "could not read the unit's state (systemctl show failed)".to_string();
    };
    let props = parse_show_properties(show);
    if props.exec_main_status.as_deref() == Some("203") {
        return match unit_binary {
            Some(path) => {
                format!(
                    "ExecStart binary not executable or missing: {}",
                    path.display()
                )
            }
            None => "ExecStart binary not executable or missing".to_string(),
        };
    }
    format!(
        "unit state: ActiveState={} SubState={} ExecMainStatus={} Result={}",
        props.active_state.as_deref().unwrap_or("?"),
        props.sub_state.as_deref().unwrap_or("?"),
        props.exec_main_status.as_deref().unwrap_or("?"),
        props.result.as_deref().unwrap_or("?"),
    )
}

/// CB75 FR3: the full error text for a start that never opened the port —
/// the named cause, then the journalctl hint that used to be all there was.
pub(crate) fn start_failure_message(port: u16, info: &StartFailureInfo) -> String {
    format!(
        "service manager start did not open port {port} within 10s — {}; \
         check logs: journalctl --user -u {SYSTEMD_UNIT_NAME} (or ~/Library logs on macOS)",
        describe_start_failure(info.show.as_deref(), info.unit_binary.as_deref())
    )
}

/// CB75 FR3 (I/O): read the unit's state for a failed start. `runner` and
/// `unit_path` are injected so tests never touch a real `systemctl` or the
/// developer's real unit file.
pub(crate) fn read_start_failure_info(
    runner: &dyn CommandRunner,
    unit_path: Option<&Path>,
) -> StartFailureInfo {
    let show = runner.run_capture(
        "systemctl",
        &[
            "--user",
            "show",
            SYSTEMD_UNIT_NAME,
            "-p",
            "ActiveState,SubState,ExecMainStatus,Result",
        ],
    );
    let unit_content = unit_path.and_then(|path| std::fs::read_to_string(path).ok());
    StartFailureInfo {
        show,
        unit_binary: unit_content.as_deref().and_then(unit_exec_start_binary),
    }
}

/// CB75 FR3: the production reader behind `StartDeps::diagnose`.
pub(crate) fn read_start_failure_info_live() -> StartFailureInfo {
    read_start_failure_info(&RealRunner, live_unit_path().as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct RecordingRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        show: Option<String>,
    }
    impl RecordingRunner {
        fn new(show: Option<String>) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                show,
            }
        }
        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }
        fn record(&self, prog: &str, args: &[&str]) {
            self.calls.borrow_mut().push((
                prog.to_string(),
                args.iter().map(|s| (*s).to_string()).collect(),
            ));
        }
    }
    impl CommandRunner for RecordingRunner {
        fn run(&self, prog: &str, args: &[&str]) -> bool {
            self.record(prog, args);
            true
        }
        fn run_capture(&self, prog: &str, args: &[&str]) -> Option<String> {
            self.record(prog, args);
            self.show.clone()
        }
    }

    #[test]
    fn unit_exec_start_binary_takes_the_first_token() {
        assert_eq!(
            unit_exec_start_binary("[Service]\nExecStart=/opt/canopy serve --port 4177\n"),
            Some(PathBuf::from("/opt/canopy"))
        );
        assert_eq!(unit_exec_start_binary("[Service]\nExecStart=\n"), None);
        assert_eq!(
            unit_exec_start_binary("ExecStart=/gone/canopy serve"),
            Some(PathBuf::from("/gone/canopy"))
        );
    }

    #[test]
    fn exec_start_needs_update_is_true_without_a_binary() {
        assert!(exec_start_needs_update(None, Path::new("/x/canopy")));
    }

    #[test]
    fn exec_start_needs_update_is_false_for_the_same_path() {
        assert!(!exec_start_needs_update(
            Some(Path::new("/x/canopy")),
            Path::new("/x/canopy")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn exec_start_needs_update_is_false_for_a_symlink_to_the_same_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let real_dir = tmp.path().join("real");
        let link_dir = tmp.path().join("link");
        std::fs::create_dir_all(&real_dir).unwrap();
        std::fs::create_dir_all(&link_dir).unwrap();
        let real = real_dir.join("canopy");
        std::fs::write(&real, "x").unwrap();
        let link = link_dir.join("canopy");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(!exec_start_needs_update(Some(&link), &real));
    }

    #[test]
    fn exec_start_needs_update_is_true_when_the_binary_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        let current = tmp.path().join("canopy");
        std::fs::write(&current, "x").unwrap();
        assert!(exec_start_needs_update(
            Some(Path::new("/gone/canopy")),
            &current
        ));
    }

    #[test]
    fn reconcile_message_names_the_old_and_new_binaries() {
        assert_eq!(
            reconcile_message(
                Some(Path::new("/home/lina/.cargo/bin/canopy")),
                Path::new("/home/lina/.local/bin/canopy")
            ),
            "service unit pointed at /home/lina/.cargo/bin/canopy; updated to /home/lina/.local/bin/canopy"
        );
        assert_eq!(
            reconcile_message(None, Path::new("/home/lina/.local/bin/canopy")),
            "service unit pointed at no ExecStart line; updated to /home/lina/.local/bin/canopy"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reconcile_rewrites_a_stale_unit_then_reloads_and_starts() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        std::fs::write(
            &unit_path,
            "[Unit]\nDescription=canopy daemon\n\n[Service]\nType=simple\nExecStart=/gone/canopy serve --port 7755\nRestart=on-failure\nEnvironment=PATH=/home/u/.opencode/bin:/usr/bin:/bin\nEnvironment=BROWSER=/x/wsl-browser\nEnvironment=DISPLAY=:0\n\n[Install]\nWantedBy=default.target\n",
        )
        .unwrap();
        let runner = RecordingRunner::new(None);
        let outcome = reconcile_unit_binary(
            &runner,
            &unit_path,
            Path::new("/home/u/.local/bin/canopy"),
            7755,
        )
        .unwrap();
        assert_eq!(
            outcome,
            Some(UnitReconcile {
                old: Some(PathBuf::from("/gone/canopy")),
                new: PathBuf::from("/home/u/.local/bin/canopy"),
            })
        );
        super::super::process::service_manager_start_with(&runner);
        assert_eq!(
            runner.calls(),
            vec![
                (
                    "systemctl".to_string(),
                    vec!["--user".to_string(), "daemon-reload".to_string()]
                ),
                (
                    "systemctl".to_string(),
                    vec![
                        "--user".to_string(),
                        "reset-failed".to_string(),
                        "canopy.service".to_string()
                    ]
                ),
                (
                    "systemctl".to_string(),
                    vec![
                        "--user".to_string(),
                        "start".to_string(),
                        "canopy.service".to_string()
                    ]
                ),
            ]
        );
        let rewritten = std::fs::read_to_string(&unit_path).unwrap();
        assert!(rewritten.contains("ExecStart=/home/u/.local/bin/canopy serve --port 7755"));
        assert!(rewritten.contains("/home/u/.opencode/bin"));
        // Browser env goes through the same reconciliation `install` uses:
        // the live process env wins when set, otherwise the old unit value
        // is kept. Assert whichever the current environment dictates.
        match std::env::var("BROWSER").ok().filter(|v| !v.is_empty()) {
            Some(live) => assert!(rewritten.contains(&format!("Environment=BROWSER={live}"))),
            None => assert!(rewritten.contains("Environment=BROWSER=/x/wsl-browser")),
        }
        match std::env::var("DISPLAY").ok().filter(|v| !v.is_empty()) {
            Some(live) => assert!(rewritten.contains(&format!("Environment=DISPLAY={live}"))),
            None => assert!(rewritten.contains("Environment=DISPLAY=:0")),
        }
        assert!(rewritten.contains("OOMPolicy=continue"));
        assert!(rewritten.contains("MemoryMax=80%"));
    }

    #[test]
    fn reconcile_leaves_a_current_unit_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("canopy");
        std::fs::write(&exe, "x").unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let content = format!("[Service]\nExecStart={} serve --port 7755\n", exe.display());
        std::fs::write(&unit_path, &content).unwrap();
        let runner = RecordingRunner::new(None);
        let outcome = reconcile_unit_binary(&runner, &unit_path, &exe, 7755).unwrap();
        assert_eq!(outcome, None);
        assert!(runner.calls().is_empty());
        assert_eq!(std::fs::read_to_string(&unit_path).unwrap(), content);
    }

    #[test]
    fn reconcile_is_a_noop_without_a_unit_file() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        let runner = RecordingRunner::new(None);
        let outcome = reconcile_unit_binary(
            &runner,
            &unit_path,
            Path::new("/home/u/.local/bin/canopy"),
            7755,
        )
        .unwrap();
        assert_eq!(outcome, None);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn reconcile_rewrites_a_unit_with_no_exec_start_line() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        std::fs::write(&unit_path, "[Service]\nEnvironment=PATH=/usr/bin:/bin\n").unwrap();
        let runner = RecordingRunner::new(None);
        let exe = Path::new("/home/u/.local/bin/canopy");
        let outcome = reconcile_unit_binary(&runner, &unit_path, exe, 7755).unwrap();
        assert_eq!(
            outcome,
            Some(UnitReconcile {
                old: None,
                new: exe.to_path_buf(),
            })
        );
        let rewritten = std::fs::read_to_string(&unit_path).unwrap();
        assert!(rewritten.contains("ExecStart=/home/u/.local/bin/canopy"));
        assert_eq!(runner.calls().len(), 2);
    }

    #[test]
    fn describe_start_failure_203_names_the_binary() {
        assert_eq!(
            describe_start_failure(
                Some("ActiveState=failed\nSubState=exited\nExecMainStatus=203\nResult=exit-code\n"),
                Some(Path::new("/gone/canopy"))
            ),
            "ExecStart binary not executable or missing: /gone/canopy"
        );
    }

    #[test]
    fn describe_start_failure_203_without_a_binary_still_names_the_cause() {
        assert_eq!(
            describe_start_failure(
                Some("ActiveState=failed\nSubState=exited\nExecMainStatus=203\nResult=exit-code\n"),
                None
            ),
            "ExecStart binary not executable or missing"
        );
    }

    #[test]
    fn describe_start_failure_reports_raw_values_for_other_statuses() {
        let msg = describe_start_failure(
            Some("ActiveState=failed\nSubState=start-limit\nExecMainStatus=1\nResult=exit-code\n"),
            Some(Path::new("/gone/canopy")),
        );
        assert!(msg.contains("ActiveState=failed"));
        assert!(msg.contains("SubState=start-limit"));
        assert!(msg.contains("ExecMainStatus=1"));
        assert!(msg.contains("Result=exit-code"));
        assert!(msg.contains("unit state:"));
        assert!(!msg.contains("not executable or missing"));
    }

    #[test]
    fn describe_start_failure_without_show_output_says_so() {
        assert!(describe_start_failure(None, None).contains("could not read the unit's state"));
    }

    #[test]
    fn read_start_failure_info_reads_show_and_the_unit_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let unit_path = tmp.path().join("canopy.service");
        std::fs::write(
            &unit_path,
            "[Service]\nExecStart=/gone/canopy serve --port 7755\n",
        )
        .unwrap();
        let runner = RecordingRunner::new(Some(
            "ActiveState=failed\nSubState=exited\nExecMainStatus=203\nResult=exit-code\n"
                .to_string(),
        ));
        let info = read_start_failure_info(&runner, Some(&unit_path));
        assert_eq!(
            runner.calls(),
            vec![(
                "systemctl".to_string(),
                vec![
                    "--user".to_string(),
                    "show".to_string(),
                    "canopy.service".to_string(),
                    "-p".to_string(),
                    "ActiveState,SubState,ExecMainStatus,Result".to_string(),
                ]
            )]
        );
        assert_eq!(info.unit_binary, Some(PathBuf::from("/gone/canopy")));
        assert_eq!(
            describe_start_failure(info.show.as_deref(), info.unit_binary.as_deref()),
            "ExecStart binary not executable or missing: /gone/canopy"
        );
    }

    #[test]
    fn start_failure_message_carries_port_cause_and_journal_hint() {
        let info = StartFailureInfo {
            show: Some(
                "ActiveState=failed\nSubState=exited\nExecMainStatus=203\nResult=exit-code\n"
                    .to_string(),
            ),
            unit_binary: Some(PathBuf::from("/gone/canopy")),
        };
        let msg = start_failure_message(7755, &info);
        assert!(msg.contains("did not open port 7755 within 10s"));
        assert!(msg.contains("not executable or missing: /gone/canopy"));
        assert!(msg.contains("journalctl --user -u canopy.service"));
    }
}
