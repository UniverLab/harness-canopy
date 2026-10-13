use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::daemon::process;
use crate::db::Database;
use crate::domain::clean::{self, CleanPlan};

use super::hard_cascade::run_hard_cascade;
use super::scans::format_bytes;
use super::{build_clean_plan, execute_plan, print_summary};

/// After a clean run, reclaim the database space its own row deletions
/// freed — but only when that's actually warranted. Skips automatically
/// (spec: "skipped automatically when the deletion was trivial") unless
/// `rows_deleted` clears [`clean::RECLAIM_ROW_THRESHOLD`], skips entirely
/// when `no_reclaim` opts out for a fast run, and never touches the file
/// under `--dry-run` — it only prints what a real run would do.
///
/// Reclaiming (`VACUUM` + WAL checkpoint) takes an exclusive lock on the
/// database, so it must not run while the daemon could be mid-write. The
/// daemon's own singleton lock (`daemon::process::acquire_daemon_lock`) is
/// a `daemon.pid`-backed flock; this reuses the same pid file (rather than
/// re-acquiring the flock, which would race the daemon's own re-acquire on
/// restart) to decide whether a daemon is up before ever calling `VACUUM`.
pub(super) fn reclaim_if_warranted(
    db: &Database,
    data_dir: &Path,
    db_path: &Path,
    dry_run: bool,
    no_reclaim: bool,
    rows_deleted: u64,
) {
    if no_reclaim || !clean::should_reclaim(rows_deleted as usize) {
        return;
    }

    let size_before = std::fs::metadata(db_path).ok().map(|m| m.len());

    if dry_run {
        if let Some(before) = size_before {
            println!(
                "\n Database file: {} — would reclaim space ({rows_deleted} row(s) deleted, ≥ {} threshold; run without --dry-run to apply).",
                format_bytes(before),
                clean::RECLAIM_ROW_THRESHOLD,
            );
        }
        return;
    }

    let daemon_running = crate::daemon::process::read_pid(data_dir)
        .map(crate::daemon::process::is_process_running)
        .unwrap_or(false);
    if daemon_running {
        println!(
            "\n \x1b[33m⚠\x1b[0m  Skipped space reclamation: the canopy daemon is running and holds a write connection that a VACUUM's exclusive lock would conflict with. Stop it (`canopy daemon stop`) and re-run `canopy clean` to shrink the database file."
        );
        return;
    }

    match db.reclaim_space() {
        Ok(()) => {
            let size_after = std::fs::metadata(db_path).ok().map(|m| m.len());
            match (size_before, size_after) {
                (Some(before), Some(after)) => println!(
                    "\n Database file: {} -> {} ({rows_deleted} row(s) reclaimed via VACUUM + WAL checkpoint)",
                    format_bytes(before),
                    format_bytes(after),
                ),
                _ => println!(
                    "\n Reclaimed database space ({rows_deleted} row(s) via VACUUM + WAL checkpoint)."
                ),
            }
        }
        Err(err) => {
            eprintln!("\n \x1b[33m⚠\x1b[0m  Could not reclaim database space: {err}");
        }
    }
}

// ── B2: the stop-daemon reclaim window ───────────────────────────────────
//
// `reclaim_if_warranted` above only ever fires when the daemon happens to
// already be down — normally it never is, so that path is effectively
// dead. The functions below let `canopy clean` create the exclusive window
// itself: refuse if busy, stop the daemon, `quick_check` gate, run the
// existing cleanup, `VACUUM` + WAL checkpoint, and restore the daemon on
// every exit path — including a panic or Ctrl-C.

/// Interactive consent to stop the daemon, mirroring the `--hard`/`--yes`
/// precedent in this same command: refuses (rather than hangs or
/// accidentally confirms) on a non-tty or a stray Enter.
pub(super) fn confirm_stop_daemon() -> Result<bool> {
    use inquire::Confirm;
    Confirm::new(
        "Reclaiming this much space requires stopping the canopy daemon. Stop it, reclaim, then restart it?",
    )
    .with_default(false)
    .with_help_message("y: stop/reclaim/restart, n/Esc: skip reclamation")
    .prompt()
    .or(Ok(false))
}

pub(super) fn print_dry_run_reclaim_window(rows_deleted: u64, db_path: &Path) {
    let size_before = std::fs::metadata(db_path).ok().map(|m| m.len());
    println!(
        "\n Database file: {} — would reclaim space ({rows_deleted} row(s) deleted, ≥ {} threshold).",
        size_before.map(format_bytes).unwrap_or_default(),
        clean::RECLAIM_ROW_THRESHOLD,
    );
    println!(
        " Would stop the canopy daemon (if running), run `PRAGMA quick_check`, reclaim space \
         (VACUUM + WAL checkpoint), then restart the daemon exactly as it was running."
    );
}

/// Who currently owns the running daemon process — decides how it must be
/// stopped and restored (B-decision 4). `Managed` carries the service
/// manager's name (`"systemd"` on Linux, `"launchd"` on macOS) purely for
/// logging/reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DaemonOwnerKind {
    Managed(&'static str),
    Detached,
}

impl DaemonOwnerKind {
    fn describe(&self) -> &'static str {
        match self {
            DaemonOwnerKind::Managed("systemd") => "systemd",
            DaemonOwnerKind::Managed("launchd") => "launchd",
            DaemonOwnerKind::Managed(_) => "the service manager",
            DaemonOwnerKind::Detached => "a detached process (no owning service unit)",
        }
    }
}

/// Reuses [`process::service_manager_facts`] (systemd/launchd cgroup or
/// `launchctl` fact-checking already built for `daemon status`) rather than
/// inventing a second ownership check: the daemon is service-manager-owned
/// only if that manager's own live-PID fact names exactly this PID.
fn detect_daemon_owner(daemon_pid: u32) -> DaemonOwnerKind {
    match process::service_manager_facts() {
        Some(facts) if facts.pid == Some(daemon_pid) => DaemonOwnerKind::Managed(facts.name),
        _ => DaemonOwnerKind::Detached,
    }
}

/// B-decision 5: detect *before* stopping anything whether restoration is
/// even possible. A missing/disabled systemd unit must not be discovered
/// only after the daemon is already down.
fn restoration_feasible(owner: DaemonOwnerKind) -> std::result::Result<(), String> {
    let unit_installed = process::service_unit_installed();
    #[cfg(target_os = "linux")]
    let (systemd_available, service_enabled) = (
        process::is_systemd_available(),
        process::is_service_enabled(),
    );
    #[cfg(not(target_os = "linux"))]
    let (systemd_available, service_enabled) = (true, true);
    restoration_feasible_from_facts(owner, unit_installed, systemd_available, service_enabled)
}

/// Pure decision half of [`restoration_feasible`] (B-decision 5), fed
/// booleans instead of shelling out itself — `systemctl`/the unit file are
/// absent in some CI containers (see [`process::is_systemd_available`]),
/// so a test asserting a specific refusal must not depend on the machine
/// it happens to run on actually having (or lacking) a real canopy unit.
pub(super) fn restoration_feasible_from_facts(
    owner: DaemonOwnerKind,
    unit_installed: bool,
    systemd_available: bool,
    service_enabled: bool,
) -> std::result::Result<(), String> {
    match owner {
        DaemonOwnerKind::Managed(name) => {
            if !unit_installed {
                return Err(format!(
                    "{name} unit is not installed — restoring through {name} is impossible"
                ));
            }
            if name == "systemd" {
                if !systemd_available {
                    return Err("the systemd user session is unavailable".to_string());
                }
                if !service_enabled {
                    return Err(format!(
                        "the {name} unit is disabled — a restart cannot be trusted to bring the daemon back"
                    ));
                }
            }
            Ok(())
        }
        DaemonOwnerKind::Detached => std::env::current_exe()
            .map(|_| ())
            .map_err(|err| format!("cannot resolve the canopy executable to relaunch it: {err}")),
    }
}

/// The stop/restart mechanics, behind a trait so [`run_reclaim_window`]'s
/// decision logic — the part the spec requires tests for — is exercisable
/// with a fake instead of a real subprocess/systemctl dance.
pub(super) trait DaemonOps {
    fn stop(&self, owner: DaemonOwnerKind) -> Result<()>;
    fn restart(&self, owner: DaemonOwnerKind) -> Result<()>;
}

struct RealDaemonOps {
    data_dir: std::path::PathBuf,
    port: u16,
}

impl DaemonOps for RealDaemonOps {
    fn stop(&self, owner: DaemonOwnerKind) -> Result<()> {
        match owner {
            DaemonOwnerKind::Managed(name) => stop_managed_daemon(name),
            DaemonOwnerKind::Detached => stop_detached_daemon(&self.data_dir, self.port),
        }
    }

    fn restart(&self, owner: DaemonOwnerKind) -> Result<()> {
        match owner {
            DaemonOwnerKind::Managed(name) => {
                if !process::service_manager_start() {
                    anyhow::bail!("failed to restart the {name}-managed daemon");
                }
                Ok(())
            }
            DaemonOwnerKind::Detached => spawn_detached_daemon(&self.data_dir, self.port),
        }
    }
}

/// Stops a service-manager-owned daemon through the manager's own stop
/// verb. Never a bare SIGTERM here: the installed unit has
/// `Restart=on-failure`, so signalling the PID directly reads to the
/// manager as an unclean exit and it respawns the daemon out from under
/// the exclusive VACUUM this stop is for. Going through the manager is a
/// stop it won't immediately undo.
fn stop_managed_daemon(name: &'static str) -> Result<()> {
    if !process::service_manager_stop() {
        anyhow::bail!("failed to stop the {name}-managed daemon");
    }
    Ok(())
}

/// Stops a detached daemon by PID (PID file first, then the port), waits
/// for it to actually exit, and always clears the PID file afterwards.
fn stop_detached_daemon(data_dir: &Path, port: u16) -> Result<()> {
    let pid = process::read_pid(data_dir)
        .or_else(|| process::resolve_port_pid(port))
        .filter(|&p| process::is_process_running(p));
    if let Some(pid) = pid {
        process::send_signal(pid);
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            if !process::is_process_running(pid) {
                break;
            }
        }
    }
    process::remove_pid_file(data_dir);
    Ok(())
}

/// Relaunch a detached `canopy serve`, mirroring `canopy daemon start`'s
/// spawn (setsid, logs appended, stdin null) but deliberately without its
/// `install_service_if_needed` side effect — restoring a daemon that wasn't
/// service-manager-owned must not silently switch it to being one.
fn spawn_detached_daemon(data_dir: &Path, port: u16) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg("serve").arg("--port").arg(port.to_string());

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
    let child_pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(500));
    if !process::is_process_running(child_pid) {
        anyhow::bail!("daemon process exited immediately after restart");
    }
    Ok(())
}

/// The operator-facing choices for one reclaim window: how much history to
/// keep, the timestamp the cleanup plan is computed against, whether to also
/// run the hard cascade, and whether confirmation prompts are skipped.
#[derive(Debug, Clone, Copy)]
pub(super) struct ReclaimOptions {
    pub retention_days: u64,
    /// CM46: days a graph's event log is kept (independent of `retention_days`).
    pub graph_event_retention_days: u64,
    pub now_ts: i64,
    pub hard: bool,
    pub yes: bool,
}

/// What happened inside the reclaim window — every branch reports exactly
/// what the spec requires (busy names what's busy; a `quick_check` failure
/// says nothing was modified; the happy path reports every step
/// distinctly).
pub(super) enum ReclaimWindowOutcome {
    /// Refused before touching anything: either busy (names what) or
    /// restoration was judged infeasible (names why).
    Refused(String),
    /// `quick_check` found a problem — VACUUM was never attempted and the
    /// existing cleanup never ran, so nothing was modified.
    QuickCheckFailed {
        verdict: String,
        daemon_restored: bool,
    },
    Completed(Box<ReclaimWindowReport>),
}

pub(super) struct ReclaimWindowReport {
    pub(super) daemon_was_running: bool,
    pub(super) daemon_owner: Option<DaemonOwnerKind>,
    pub(super) quick_check: String,
    pub(super) plan: CleanPlan,
    pub(super) retention_days: u64,
    pub(super) hard_rows: u64,
    pub(super) size_before: Option<u64>,
    pub(super) size_after: Option<u64>,
    pub(super) daemon_restored: bool,
}

/// Restores the daemon exactly once, no matter how many callers race to
/// call it (the sync happy/error paths below, and — in the real CLI glue —
/// a concurrent Ctrl-C handler). `restored` only latches `true` on an
/// actual success so a failed attempt can still be retried by whichever
/// caller asks next (idempotent restoration per the spec).
fn restore_daemon_once(
    restored: &Mutex<bool>,
    ops: &dyn DaemonOps,
    owner: Option<DaemonOwnerKind>,
) -> bool {
    let mut done = restored.lock().unwrap_or_else(|e| e.into_inner());
    if *done {
        return true;
    }
    let ok = match owner {
        None => true,
        Some(owner) => ops.restart(owner).is_ok(),
    };
    *done = ok;
    ok
}

/// The orchestrated window itself (B-decision 1-6, functional
/// requirements): refuse-if-busy, stop, `quick_check`, existing cleanup,
/// `VACUUM` + WAL checkpoint, restore — in that order, with the daemon
/// restored on every exit path via `restored`/[`restore_daemon_once`].
///
/// Deliberately synchronous and injected with `ops` rather than calling the
/// real subprocess/systemctl mechanics directly: this is the part the spec
/// requires tests for (busy refusal, `quick_check`-gates-VACUUM,
/// restore-on-failure, infeasible-restoration refusal), and none of that
/// needs a real daemon process to exercise.
pub(super) fn run_reclaim_window(
    db: &Database,
    data_dir: &Path,
    db_path: &Path,
    options: ReclaimOptions,
    daemon_pid: Option<u32>,
    ops: &dyn DaemonOps,
    restored: &Mutex<bool>,
) -> Result<ReclaimWindowOutcome> {
    let ReclaimOptions {
        retention_days,
        graph_event_retention_days,
        now_ts,
        hard,
        yes,
    } = options;
    let busy = db.busy_reasons()?;
    if !busy.is_empty() {
        return Ok(ReclaimWindowOutcome::Refused(format!(
            "refusing to stop the daemon — busy: {}",
            busy.join(", ")
        )));
    }

    let owner = daemon_pid.map(detect_daemon_owner);
    if let Some(owner) = owner {
        if let Err(reason) = restoration_feasible(owner) {
            return Ok(ReclaimWindowOutcome::Refused(format!(
                "refusing to stop the daemon — cannot guarantee it can be restored afterward: {reason}"
            )));
        }
    }

    // Daemon wasn't running at all: nothing to stop, so `restored` starts
    // already-true and every subsequent restore attempt is a no-op.
    if owner.is_none() {
        *restored.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }

    /// Panic safety net: if anything below unwinds, restore on drop rather
    /// than leaving the daemon down. A no-op once `restore_daemon_once` has
    /// already succeeded from the normal control flow.
    struct PanicGuard<'a> {
        ops: &'a dyn DaemonOps,
        owner: Option<DaemonOwnerKind>,
        restored: &'a Mutex<bool>,
    }
    impl Drop for PanicGuard<'_> {
        fn drop(&mut self) {
            restore_daemon_once(self.restored, self.ops, self.owner);
        }
    }
    let _panic_guard = PanicGuard {
        ops,
        owner,
        restored,
    };

    if let Some(owner) = owner {
        ops.stop(owner)?;
    }

    let quick_check = db.quick_check()?;
    if quick_check != "ok" {
        let daemon_restored = restore_daemon_once(restored, ops, owner);
        return Ok(ReclaimWindowOutcome::QuickCheckFailed {
            verdict: quick_check,
            daemon_restored,
        });
    }

    let plan = build_clean_plan(
        data_dir,
        db,
        retention_days,
        graph_event_retention_days,
        now_ts,
    )?;
    execute_plan(db, &plan)?;
    let hard_rows = if hard {
        run_hard_cascade(db, false, yes)?
    } else {
        0
    };

    let size_before = std::fs::metadata(db_path).ok().map(|m| m.len());
    db.reclaim_space()?;
    let size_after = std::fs::metadata(db_path).ok().map(|m| m.len());

    let daemon_restored = restore_daemon_once(restored, ops, owner);

    Ok(ReclaimWindowOutcome::Completed(Box::new(
        ReclaimWindowReport {
            daemon_was_running: daemon_pid.is_some(),
            daemon_owner: owner,
            quick_check,
            plan,
            retention_days,
            hard_rows,
            size_before,
            size_after,
            daemon_restored,
        },
    )))
}

fn print_reclaim_window_outcome(outcome: &ReclaimWindowOutcome) {
    match outcome {
        ReclaimWindowOutcome::Refused(reason) => {
            println!("\n \x1b[33m⚠\x1b[0m  {reason}");
            println!(
                "   Skipped space reclamation and left the daemon exactly as it was — retry once that clears."
            );
        }
        ReclaimWindowOutcome::QuickCheckFailed {
            verdict,
            daemon_restored,
        } => {
            println!(
                "\n \x1b[31m✗\x1b[0m  PRAGMA quick_check reported a problem with the database file: {verdict}"
            );
            println!(
                "   No reclamation was attempted and nothing was modified — the file was left exactly as found."
            );
            println!(
                "   Daemon restored: {}",
                if *daemon_restored {
                    "yes"
                } else {
                    "\x1b[31mno — check `canopy daemon status`\x1b[0m"
                }
            );
        }
        ReclaimWindowOutcome::Completed(report) => {
            print_summary(&report.plan, report.retention_days, false);
            println!(
                " Daemon stopped: {}",
                if report.daemon_was_running {
                    format!(
                        "yes ({})",
                        report
                            .daemon_owner
                            .map(|o| o.describe())
                            .unwrap_or("unknown")
                    )
                } else {
                    "no (was not running)".to_string()
                }
            );
            println!(" quick_check: {}", report.quick_check);
            match (report.size_before, report.size_after) {
                (Some(before), Some(after)) => println!(
                    " Database file: {} -> {} ({} row(s) reclaimed via VACUUM + WAL checkpoint)",
                    format_bytes(before),
                    format_bytes(after),
                    report.plan.deleted_row_count() as u64 + report.hard_rows,
                ),
                _ => println!(
                    " Reclaimed database space ({} row(s) via VACUUM + WAL checkpoint).",
                    report.plan.deleted_row_count() as u64 + report.hard_rows
                ),
            }
            println!(
                " Daemon restored: {}",
                if report.daemon_restored {
                    "yes"
                } else {
                    "\x1b[31mno — check `canopy daemon status`\x1b[0m"
                }
            );
        }
    }
}

/// Real-CLI glue around [`run_reclaim_window`]: runs it on a blocking
/// thread (it does real subprocess/flock/VACUUM work) while a concurrent
/// task watches for Ctrl-C. Both sides restore through the same
/// `restored` flag, so whichever notices first — the window finishing on
/// its own, or the operator interrupting — is the one that actually
/// restarts the daemon; the other is a no-op.
pub(super) async fn run_reclaim_window_and_report(
    data_dir: &Path,
    db_path: &Path,
    options: ReclaimOptions,
    daemon_pid: Option<u32>,
) -> Result<()> {
    let port = crate::daemon::cli::configured_port(data_dir);
    let owner = daemon_pid.map(detect_daemon_owner);
    let ops = Arc::new(RealDaemonOps {
        data_dir: data_dir.to_path_buf(),
        port,
    });
    let restored = Arc::new(Mutex::new(daemon_pid.is_none()));

    let watcher_ops = ops.clone();
    let watcher_restored = restored.clone();
    let watcher = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n\x1b[33m⚠\x1b[0m  Interrupted — restoring the daemon before exiting...");
            let ok = restore_daemon_once(&watcher_restored, watcher_ops.as_ref(), owner);
            eprintln!(
                "{}",
                if ok {
                    "Daemon restored."
                } else {
                    "Failed to restore the daemon — check `canopy daemon status`."
                }
            );
            std::process::exit(130);
        }
    });

    let db_owned = db_path.to_path_buf();
    let db_dir_owned = data_dir.to_path_buf();
    let blocking_ops = ops.clone();
    let blocking_restored = restored.clone();
    let outcome = tokio::task::spawn_blocking({
        let db = Database::new_safe(&db_owned, &db_dir_owned)?;
        move || {
            run_reclaim_window(
                &db,
                &db_dir_owned,
                &db_owned,
                options,
                daemon_pid,
                blocking_ops.as_ref(),
                &blocking_restored,
            )
        }
    })
    .await;

    watcher.abort();

    let outcome = outcome.map_err(|e| anyhow::anyhow!("reclaim window task panicked: {e}"))??;
    print_reclaim_window_outcome(&outcome);
    Ok(())
}
