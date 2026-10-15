//! CB72: the single shared daemon-start path.
//!
//! Every auto-start call site (TUI, setup wizard, `canopy daemon start`)
//! goes through [`start_daemon_live`], which prefers the installed user
//! unit (`systemctl --user start canopy.service`, `launchctl load` on
//! macOS) and waits for the port to answer, and only falls back to today's
//! detached `canopy serve` spawn when no unit is installed or no service
//! manager is available. Anything the unit defines (PATH, drop-ins,
//! `Restart=on-failure`) therefore applies to the daemon actually running —
//! a detached spawn no longer bypasses it and become an orphan.

use super::process::{CommandRunner, RealRunner};
use super::service_reconcile::{self, StartFailureInfo};

/// Why this start was requested. Passed for readability/audit at the call
/// sites: `start_daemon` itself behaves identically for both intents — it
/// never kills; the kill rules live in the callers (`handle_start` kills a
/// proven orphan before starting; the TUI/setup Auto path never kills).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StartIntent {
    /// TUI (`auto_start_daemon`) and setup (`start_daemon_if_needed`):
    /// never kills anything, only starts when nothing listens.
    Auto,
    /// `canopy daemon start`: the caller has already killed a proven
    /// orphan, so this call is allowed to take the port.
    Explicit,
}

/// Everything [`start_daemon`] touches, injected so tests are hermetic and
/// never execute a real `systemctl`/`launchctl` or spawn a real process.
pub(crate) struct StartDeps<'a> {
    /// Receives every manager invocation the start path makes.
    pub(crate) runner: &'a dyn CommandRunner,
    /// Is the user unit/agent installed? (real: `process::service_unit_installed()`)
    pub(crate) unit_installed: bool,
    /// Is the manager binary usable? (real: Linux `process::is_systemd_available()`,
    /// macOS `true` — `launchctl` always exists — elsewhere `false`)
    pub(crate) manager_available: bool,
    /// Does something listen on `port` right now?
    /// (real: `|p| process::resolve_port_pid(p).is_some()`)
    pub(crate) port_open: &'a dyn Fn(u16) -> bool,
    /// Today's detached `canopy serve` spawn, preconfigured by the caller.
    pub(crate) spawn_direct: &'a dyn Fn() -> anyhow::Result<()>,
    /// Manager start command, precomputed per-platform by
    /// [`start_daemon_live`] so [`start_daemon`] has no cfg branches.
    pub(crate) manager_start_prog: String,
    /// See `manager_start_prog` (macOS carries the plist path here).
    pub(crate) manager_start_args: Vec<String>,
    /// How many times to poll the port while waiting for the manager start
    /// to answer (real: 40 × 250 ms = 10 s).
    pub(crate) polls: usize,
    /// One wait between polls (real: `std::thread::sleep(250ms)`;
    /// tests: no-op so the timeout path is instant).
    pub(crate) tick: &'a dyn Fn(),
    /// FR3: read the unit's state. Called only when the manager start never
    /// opened the port, never on the happy path.
    /// (real: `service_reconcile::read_start_failure_info_live`)
    pub(crate) diagnose: &'a dyn Fn() -> StartFailureInfo,
}

/// The one start function all three auto-start paths share (FR1).
///
/// 1. Port already answering → nothing to do (an orphan listener included:
///    this is why the TUI path never kills and never starts over one).
/// 2. Unit installed and manager available → ask the manager to start the
///    unit, then poll the port for up to ~10 s. **Never** falls through to
///    a direct spawn: a direct spawn here is exactly the CB72 orphan.
/// 3. Otherwise (no unit, or no manager) → today's direct detached spawn.
///
/// `intent` never alters this behavior; it documents the caller's kill
/// rule (the shared function itself never kills).
pub(crate) fn start_daemon(
    port: u16,
    intent: StartIntent,
    deps: &StartDeps<'_>,
) -> anyhow::Result<()> {
    let _ = intent; // audited by tests: both intents take the identical path

    // (a) Something already listens — managed daemon or orphan alike.
    if (deps.port_open)(port) {
        return Ok(());
    }

    // (b) Unit installed + manager available → start via the manager and
    // wait for the port; never spawn directly on this branch.
    if deps.unit_installed && deps.manager_available {
        let args: Vec<&str> = deps.manager_start_args.iter().map(String::as_str).collect();
        let _ = deps.runner.run(&deps.manager_start_prog, &args);
        for _ in 0..deps.polls {
            if (deps.port_open)(port) {
                return Ok(());
            }
            (deps.tick)();
        }
        if (deps.port_open)(port) {
            return Ok(());
        }
        let info = (deps.diagnose)();
        anyhow::bail!("{}", service_reconcile::start_failure_message(port, &info));
    }

    // (c) No unit installed, or no usable manager → direct spawn as today.
    (deps.spawn_direct)()
}

/// The detached `canopy serve` spawn, moved verbatim (minus readiness
/// checks — callers keep their own) out of the three duplicated copies in
/// `daemon/cli.rs`, `tui/mod.rs`, and `setup_module/daemon_service.rs`.
/// `port_arg` preserves each caller's argv: `Some(p)` for `daemon start
/// --port`, `None` for the TUI/setup bare `serve`.
pub(crate) fn spawn_direct_daemon(
    exe: &std::path::Path,
    port_arg: Option<u16>,
    data_dir: &std::path::Path,
) -> anyhow::Result<u32> {
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve");
    if let Some(p) = port_arg {
        cmd.arg("--port").arg(p.to_string());
    }

    let log_path = data_dir.join("daemon.log");
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_file_err = log_file.try_clone()?;

    cmd.stdout(log_file)
        .stderr(log_file_err)
        .stdin(std::process::Stdio::null());

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    let child = cmd.spawn()?;
    Ok(child.id())
}

/// Is the platform's service manager usable? (Linux: `systemctl --user
/// is-system-running` answers; macOS: `launchctl` always exists; elsewhere
/// `false`.)
pub(crate) fn manager_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        super::process::is_systemd_available()
    }
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// The production entry point all three callers use: builds [`StartDeps`]
/// from live platform values and delegates to [`start_daemon`].
pub(crate) fn start_daemon_live(
    port: u16,
    intent: StartIntent,
    exe: &std::path::Path,
    port_arg: Option<u16>,
    data_dir: &std::path::Path,
) -> anyhow::Result<()> {
    // Manager start command, precomputed per-cfg (FR4): `start_daemon`
    // itself stays cfg-free.
    #[cfg(target_os = "linux")]
    let (manager_start_prog, manager_start_args, manager_available) = (
        "systemctl".to_string(),
        vec![
            "--user".to_string(),
            "start".to_string(),
            super::process::SYSTEMD_UNIT_NAME.to_string(),
        ],
        manager_available(),
    );
    #[cfg(target_os = "macos")]
    let (manager_start_prog, manager_start_args, manager_available) = (
        "launchctl".to_string(),
        vec![
            "load".to_string(),
            super::process::launchd_plist_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ],
        true,
    );
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let (manager_start_prog, manager_start_args, manager_available) =
        (String::new(), Vec::<String>::new(), false);

    // Direct-spawn fallback, with the same 500 ms liveness check `daemon
    // start` did inline after spawning.
    let spawn_direct = || -> anyhow::Result<()> {
        let pid = spawn_direct_daemon(exe, port_arg, data_dir)?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        if !super::process::is_process_running(pid) {
            anyhow::bail!(
                "Daemon failed to start — check logs at {}",
                data_dir.join("daemon.log").display()
            );
        }
        Ok(())
    };

    let deps = StartDeps {
        runner: &RealRunner,
        unit_installed: super::process::service_unit_installed(),
        manager_available,
        port_open: &|p| super::process::resolve_port_pid(p).is_some(),
        spawn_direct: &spawn_direct,
        manager_start_prog,
        manager_start_args,
        polls: 40,
        tick: &|| std::thread::sleep(std::time::Duration::from_millis(250)),
        diagnose: &|| service_reconcile::read_start_failure_info_live(),
    };
    start_daemon(port, intent, &deps)
}

/// CB75 FR2: restart the installed unit as one manager operation and wait
/// for the port the way `start_daemon` does. Returns `Ok(false)` when there
/// is no installed unit / no usable manager, so the caller falls back to its
/// own stop-then-start path.
///
/// `canopy daemon restart`'s old stop-then-start pair could never clear a
/// unit that was already failed: `stop` says nothing about a unit whose
/// `ExecStart` is gone, and the `start` that follows re-hits `203/EXEC`.
pub(crate) fn restart_daemon(port: u16, deps: &RestartDeps<'_>) -> anyhow::Result<bool> {
    if !(deps.unit_installed && deps.manager_available) {
        return Ok(false);
    }
    reload_before_restart(deps.runner);
    let _ = super::process::service_manager_restart_with(deps.runner);
    for _ in 0..deps.polls {
        if (deps.port_open)(port) {
            return Ok(true);
        }
        (deps.tick)();
    }
    if (deps.port_open)(port) {
        return Ok(true);
    }
    let info = (deps.diagnose)();
    anyhow::bail!("{}", service_reconcile::start_failure_message(port, &info));
}

/// FR2: the reload that must run before a restart so a unit file rewritten
/// moments ago is actually re-read. Linux: `systemctl --user daemon-reload`;
/// launchd re-reads a plist on load, so there is nothing to reload.
#[cfg(target_os = "linux")]
fn reload_before_restart(runner: &dyn CommandRunner) {
    let _ = runner.run("systemctl", &["--user", "daemon-reload"]);
}

#[cfg(not(target_os = "linux"))]
fn reload_before_restart(_runner: &dyn CommandRunner) {}

/// Everything [`restart_daemon`] touches, injected the same way
/// [`StartDeps`] is.
pub(crate) struct RestartDeps<'a> {
    pub(crate) runner: &'a dyn CommandRunner,
    pub(crate) unit_installed: bool,
    pub(crate) manager_available: bool,
    pub(crate) port_open: &'a dyn Fn(u16) -> bool,
    pub(crate) diagnose: &'a dyn Fn() -> StartFailureInfo,
    pub(crate) polls: usize,
    pub(crate) tick: &'a dyn Fn(),
}

/// FR2: the production entry point `cli.rs` uses.
pub(crate) fn restart_daemon_live(port: u16) -> anyhow::Result<bool> {
    let deps = RestartDeps {
        runner: &RealRunner,
        unit_installed: super::process::service_unit_installed(),
        manager_available: manager_available(),
        port_open: &|p| super::process::resolve_port_pid(p).is_some(),
        diagnose: &|| service_reconcile::read_start_failure_info_live(),
        polls: 40,
        tick: &|| std::thread::sleep(std::time::Duration::from_millis(250)),
    };
    restart_daemon(port, &deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::path::PathBuf;

    /// Fake manager: records every `(prog, args)` it is asked to run and
    /// reports `start_ok`. On a successful start/load command it flips
    /// `opened`, simulating the unit actually bringing the port up (the
    /// test's `port_open` closure reads it). Any production path that
    /// shells out to `systemctl`/`launchctl` or spawns directly instead of
    /// going through `runner.run`/`spawn_direct` records nothing here and
    /// fails the `calls`/`spawned` assertions — the intended tripwire for
    /// "tests never call the real systemctl".
    struct FakeRunner {
        calls: RefCell<Vec<(String, Vec<String>)>>,
        start_ok: bool,
        opened: Cell<bool>,
    }

    impl FakeRunner {
        fn new(start_ok: bool) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                start_ok,
                opened: Cell::new(false),
            }
        }

        fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, prog: &str, args: &[&str]) -> bool {
            self.calls.borrow_mut().push((
                prog.to_string(),
                args.iter().map(|s| (*s).to_string()).collect(),
            ));
            if self.start_ok && args.iter().any(|a| *a == "start" || *a == "load") {
                self.opened.set(true);
            }
            self.start_ok
        }
        fn run_capture(&self, _prog: &str, _args: &[&str]) -> Option<String> {
            // No test drives the capture path through this runner: the
            // `systemctl show` seam is exercised by `RecordingRunner` in
            // `service_reconcile`'s own tests (T14), and the `start_daemon`
            // diagnosis seam is `StartDeps::diagnose` (T19).
            None
        }
    }

    const LINUX_UNIT_START: (&str, &[&str]) = ("systemctl", &["--user", "start", "canopy.service"]);

    /// Never sleeps, so the 40-poll timeout path runs instantly in tests.
    fn noop_tick() {}

    /// Build deps for a scenario. `port_open` and `spawn_direct` are passed
    /// in (rather than built here) because `StartDeps` borrows them for its
    /// whole lifetime — a returned struct cannot hold references to
    /// closures created inside this function.
    fn deps_for<'a>(
        runner: &'a FakeRunner,
        unit_installed: bool,
        manager_available: bool,
        port_open: &'a dyn Fn(u16) -> bool,
        spawn_direct: &'a dyn Fn() -> anyhow::Result<()>,
        polls: usize,
    ) -> StartDeps<'a> {
        StartDeps {
            runner,
            unit_installed,
            manager_available,
            port_open,
            spawn_direct,
            manager_start_prog: LINUX_UNIT_START.0.to_string(),
            manager_start_args: LINUX_UNIT_START.1.iter().map(|s| s.to_string()).collect(),
            polls,
            tick: &noop_tick,
            diagnose: &no_failure_info,
        }
    }

    /// The four existing `start_daemon` tests never exercise the failure
    /// diagnosis, so their deps carry an empty one.
    fn no_failure_info() -> StartFailureInfo {
        StartFailureInfo::default()
    }

    /// The direct-spawn probe every scenario shares: flips `spawned`
    /// instead of actually spawning a process.
    fn spawn_probe(spawned: &Cell<bool>) -> impl Fn() -> anyhow::Result<()> + '_ {
        move || {
            spawned.set(true);
            Ok(())
        }
    }

    #[test]
    fn unit_installed_plus_nothing_listening_starts_unit_without_direct_spawn() {
        for intent in [StartIntent::Auto, StartIntent::Explicit] {
            let runner = FakeRunner::new(true);
            let spawned = Cell::new(false);
            let port_open = |_: u16| runner.opened.get();
            let spawn = spawn_probe(&spawned);
            let deps = deps_for(&runner, true, true, &port_open, &spawn, 5);

            start_daemon(7755, intent, &deps)
                .unwrap_or_else(|e| panic!("intent {intent:?} must start via the unit: {e}"));

            let expected = vec![(
                LINUX_UNIT_START.0.to_string(),
                LINUX_UNIT_START
                    .1
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
            )];
            assert_eq!(
                runner.calls(),
                expected,
                "intent {intent:?}: exactly one manager start, nothing else"
            );
            assert!(
                !spawned.get(),
                "intent {intent:?}: unit path must never direct-spawn an orphan"
            );
        }
    }

    #[test]
    fn no_unit_falls_back_to_direct_spawn() {
        // No unit installed, and unit-installed-but-manager-unavailable:
        // both must take today's direct path and never touch the manager.
        for (unit_installed, manager_available) in [(false, true), (true, false)] {
            for intent in [StartIntent::Auto, StartIntent::Explicit] {
                let runner = FakeRunner::new(true);
                let spawned = Cell::new(false);
                let closed = |_: u16| false;
                let spawn = spawn_probe(&spawned);
                let deps = deps_for(
                    &runner,
                    unit_installed,
                    manager_available,
                    &closed,
                    &spawn,
                    5,
                );

                start_daemon(7755, intent, &deps).unwrap_or_else(|e| {
                    panic!(
                        "installed={unit_installed} available={manager_available} intent={intent:?}: \
                         direct spawn fallback must succeed: {e}"
                    )
                });

                assert!(
                    runner.calls().is_empty(),
                    "installed={unit_installed} available={manager_available}: \
                     no manager may be invoked on the fallback path"
                );
                assert!(
                    spawned.get(),
                    "installed={unit_installed} available={manager_available}: \
                     must fall back to the direct spawn"
                );
            }
        }
    }

    #[test]
    fn tui_auto_start_with_orphan_listening_does_nothing() {
        for intent in [StartIntent::Auto, StartIntent::Explicit] {
            let runner = FakeRunner::new(true);
            let spawned = Cell::new(false);
            let occupied = |_: u16| true;
            let spawn = spawn_probe(&spawned);
            let deps = deps_for(&runner, true, true, &occupied, &spawn, 5);

            start_daemon(7755, intent, &deps).unwrap_or_else(|e| {
                panic!("intent {intent:?}: an occupied port is already-running, not an error: {e}")
            });

            assert!(
                runner.calls().is_empty(),
                "intent {intent:?}: an occupied port must not trigger a manager start"
            );
            assert!(
                !spawned.get(),
                "intent {intent:?}: an occupied port must not trigger a direct spawn (and never a kill)"
            );
        }
    }

    #[test]
    fn manager_start_wait_timeout_errors_without_spawn() {
        // The manager command "succeeds" but the daemon never binds —
        // papering that over with a direct spawn would recreate the
        // very orphan CB72 is about.
        let runner = FakeRunner::new(true);
        let spawned = Cell::new(false);
        let never_opens = |_: u16| false;
        let spawn = spawn_probe(&spawned);
        let deps = deps_for(&runner, true, true, &never_opens, &spawn, 40);

        let err = start_daemon(7755, StartIntent::Auto, &deps)
            .expect_err("a unit start that never opens the port must error");
        assert!(
            err.to_string().contains("did not open port"),
            "unexpected error: {err}"
        );
        assert!(
            !spawned.get(),
            "a failed unit start must never be papered over with a direct spawn"
        );
        assert_eq!(
            runner.calls().len(),
            1,
            "the manager start must have been attempted exactly once"
        );
    }

    /// The runner seam in `process.rs` (CB72/plan §3.2): the real manager
    /// verbs route through the injected runner with exactly the argv the
    /// spec names, never a hardwired `std::process::Command`.
    #[cfg(target_os = "linux")]
    #[test]
    fn service_manager_start_and_stop_route_through_the_injected_runner() {
        let runner = FakeRunner::new(true);
        assert!(super::super::process::service_manager_start_with(&runner));
        assert!(super::super::process::service_manager_stop_with(&runner));
        assert_eq!(
            runner.calls(),
            vec![
                (
                    "systemctl".to_string(),
                    vec![
                        "--user".to_string(),
                        "start".to_string(),
                        "canopy.service".to_string()
                    ]
                ),
                (
                    "systemctl".to_string(),
                    vec![
                        "--user".to_string(),
                        "stop".to_string(),
                        "canopy.service".to_string()
                    ]
                ),
            ]
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn service_manager_restart_routes_through_the_injected_runner() {
        let runner = FakeRunner::new(true);
        assert!(super::super::process::service_manager_restart_with(&runner));
        assert_eq!(
            runner.calls(),
            vec![(
                "systemctl".to_string(),
                vec![
                    "--user".to_string(),
                    "restart".to_string(),
                    "canopy.service".to_string()
                ]
            )]
        );
    }

    #[test]
    fn unit_start_timeout_names_the_203_exec_start_binary() {
        let runner = FakeRunner::new(true);
        let spawned = Cell::new(false);
        let never_opens = |_: u16| false;
        let spawn = spawn_probe(&spawned);
        let diagnose = || StartFailureInfo {
            show: Some(
                "ActiveState=failed\nSubState=exited\nExecMainStatus=203\nResult=exit-code\n"
                    .to_string(),
            ),
            unit_binary: Some(PathBuf::from("/gone/canopy")),
        };
        let deps = StartDeps {
            runner: &runner,
            unit_installed: true,
            manager_available: true,
            port_open: &never_opens,
            spawn_direct: &spawn,
            manager_start_prog: LINUX_UNIT_START.0.to_string(),
            manager_start_args: LINUX_UNIT_START.1.iter().map(|s| s.to_string()).collect(),
            polls: 2,
            tick: &noop_tick,
            diagnose: &diagnose,
        };
        let err = start_daemon(7755, StartIntent::Auto, &deps)
            .expect_err("a 203/EXEC unit start must error");
        let msg = err.to_string();
        assert!(
            msg.contains("ExecStart binary not executable or missing: /gone/canopy"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("journalctl --user -u canopy.service"),
            "unexpected error: {msg}"
        );
        assert!(!spawned.get(), "a failed unit start must never spawn");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn restart_runs_reload_then_restart_once() {
        let runner = FakeRunner::new(true);
        let open = |_: u16| true;
        let deps = RestartDeps {
            runner: &runner,
            unit_installed: true,
            manager_available: true,
            port_open: &open,
            diagnose: &no_failure_info,
            polls: 2,
            tick: &noop_tick,
        };
        let ok = restart_daemon(7755, &deps).expect("restart must succeed");
        assert!(ok);
        let calls = runner.calls();
        assert_eq!(
            calls,
            vec![
                (
                    "systemctl".to_string(),
                    vec!["--user".to_string(), "daemon-reload".to_string()]
                ),
                (
                    "systemctl".to_string(),
                    vec![
                        "--user".to_string(),
                        "restart".to_string(),
                        "canopy.service".to_string()
                    ]
                ),
            ]
        );
        for (_, args) in &calls {
            assert!(
                !args.iter().any(|a| a == "stop" || a == "start"),
                "restart must not contain a separate stop/start: {calls:?}"
            );
        }
    }

    #[test]
    fn restart_falls_back_when_no_unit_is_installed() {
        for (unit_installed, manager_available) in [(false, true), (true, false), (false, false)] {
            let runner = FakeRunner::new(true);
            let open = |_: u16| true;
            let deps = RestartDeps {
                runner: &runner,
                unit_installed,
                manager_available,
                port_open: &open,
                diagnose: &no_failure_info,
                polls: 2,
                tick: &noop_tick,
            };
            let ok = restart_daemon(7755, &deps).expect("fallback must be Ok(false)");
            assert!(
                !ok,
                "installed={unit_installed} available={manager_available}: must fall back"
            );
            assert!(
                runner.calls().is_empty(),
                "installed={unit_installed} available={manager_available}: no manager call"
            );
        }
    }

    #[test]
    fn restart_timeout_reports_the_unit_state_and_never_spawns() {
        let runner = FakeRunner::new(true);
        let never_opens = |_: u16| false;
        let diagnose = || StartFailureInfo {
            show: Some(
                "ActiveState=failed\nSubState=exited\nExecMainStatus=203\nResult=exit-code\n"
                    .to_string(),
            ),
            unit_binary: Some(PathBuf::from("/gone/canopy")),
        };
        let deps = RestartDeps {
            runner: &runner,
            unit_installed: true,
            manager_available: true,
            port_open: &never_opens,
            diagnose: &diagnose,
            polls: 2,
            tick: &noop_tick,
        };
        let err = restart_daemon(7755, &deps).expect_err("a restart that never opens must error");
        assert!(
            err.to_string()
                .contains("not executable or missing: /gone/canopy"),
            "unexpected error: {err}"
        );
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
                        "restart".to_string(),
                        "canopy.service".to_string()
                    ]
                ),
            ]
        );
    }

    /// (f) FR3's stop predicate: manager-stop only when the port occupant
    /// IS the unit's MainPID — every other combination keeps the plain
    /// signal path (orphan) or has nothing to stop at all.
    #[test]
    fn stop_goes_via_manager_only_for_managed_main_pid() {
        use crate::daemon::process::{should_stop_via_manager, ServiceManagerFacts};

        let managed_9 = ServiceManagerFacts {
            name: "systemd",
            pid: Some(9),
        };
        let no_live_main = ServiceManagerFacts {
            name: "systemd",
            pid: None,
        };

        // Occupant is the unit's MainPID → go through the manager.
        assert!(should_stop_via_manager(Some(&managed_9), Some(9)));
        // Orphan holds the port (≠ MainPID) → signal path, not manager stop.
        assert!(!should_stop_via_manager(Some(&managed_9), Some(7)));
        // MainPID exists but nothing holds the port → nothing to manage-stop.
        assert!(!should_stop_via_manager(Some(&managed_9), None));
        // No manager at all → signal path.
        assert!(!should_stop_via_manager(None, Some(7)));
        // Unit installed but no live MainPID while an orphan listens → signal path.
        assert!(!should_stop_via_manager(Some(&no_live_main), Some(7)));
    }
}
