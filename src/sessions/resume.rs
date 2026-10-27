//! Daemon-side reconciliation at startup: bring back the sessions whose PTYs
//! died with the previous daemon (a restart, an upgrade, a reboot).
//!
//! Moved here from `tui::app::sessions`, decisions unchanged. The hub owns
//! every PTY, so the daemon runs this before its session endpoint accepts
//! clients; the TUI no longer resumes anything.

use std::path::Path;

use crate::db::session::{InteractiveSession, TerminalSession};
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::models::Cli;
use crate::sessions::{HubSession, SessionHub, SessionKind, SpawnRequest};
use crate::tui::build_resumed_session_args;

use std::sync::Arc;

/// Reconcile the hub with the database after a daemon start: sweep orphaned
/// rows, then respawn every `active` interactive session (and every terminal
/// session with saved history) that no live PTY in the hub owns.
pub fn reconcile_at_startup(hub: &SessionHub, data_dir: &Path) {
    sweep_orphaned_sessions(hub.db());

    let current_boot_id = crate::system::boot_id();
    let canopy_config = CanopyConfig::load(data_dir);
    resume_agent_sessions(hub, &canopy_config, current_boot_id.as_deref());
    resume_terminal_sessions(hub, data_dir);
}

/// Startup sweep (B32): retire any row still in the removed `orphaned`
/// status to `completed` so historic red orphan cards disappear. A swept
/// session is not resumed, so it never joins the hub and a pending schedule
/// aimed at it is preserved as a dead target on the next scheduler tick.
fn sweep_orphaned_sessions(db: &Database) {
    match db.close_orphaned_interactive_sessions() {
        Ok(n) if n > 0 => {
            tracing::info!("Closed {n} orphaned interactive session(s) on startup");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("Failed to sweep orphaned interactive sessions: {e}"),
    }
}

/// Resume every `active` interactive session row the hub does not already
/// run. Bridge sidecars are excluded by `get_active_sessions` itself.
fn resume_agent_sessions(
    hub: &SessionHub,
    canopy_config: &CanopyConfig,
    current_boot_id: Option<&str>,
) {
    let Ok(sessions) = hub.db().get_active_sessions() else {
        return;
    };
    if sessions.is_empty() {
        tracing::info!("No active sessions to resume");
        return;
    }
    tracing::info!("Resuming {} active session(s)", sessions.len());

    for session in &sessions {
        // A live PTY in the hub already owns this row: resuming it again
        // would mint a second process fighting the first for the session.
        if hub.get(&session.id).is_some() {
            continue;
        }
        resume_scanned_session(hub, session, canopy_config, current_boot_id);
    }
}

fn resume_scanned_session(
    hub: &SessionHub,
    session: &InteractiveSession,
    canopy_config: &CanopyConfig,
    current_boot_id: Option<&str>,
) {
    if should_resume_session(session.pid, session.boot_id.as_deref(), current_boot_id) {
        resume_interactive_session(hub, session, canopy_config, current_boot_id);
        return;
    }
    resume_or_defer_conflicting_session(hub, session, canopy_config, current_boot_id);
}

/// The stored PID is alive on the same boot — but that alone is NOT proof of
/// a session-lock conflict. Two benign cases used to permanently orphan
/// healthy sessions here: a quick TUI close→reopen, where the old CLI got its
/// HUP and is still in the middle of dying; and PID recycling, where the
/// number now belongs to an unrelated process. Only a live process that
/// actually IS this CLI, and that survives a short grace period, is a genuine
/// conflict.
fn resume_or_defer_conflicting_session(
    hub: &SessionHub,
    session: &InteractiveSession,
    canopy_config: &CanopyConfig,
    current_boot_id: Option<&str>,
) {
    let pid = session.pid.unwrap_or(0);
    if matches!(
        resume_decision(true, process_outlives_grace(pid, &session.cli)),
        ResumeDecision::Resume
    ) {
        tracing::info!(
            "Auto-resuming session '{}': stored pid {pid} was recycled or exited during grace",
            session.name
        );
        resume_interactive_session(hub, session, canopy_config, current_boot_id);
        return;
    }
    tracing::warn!(
        "Skipping auto-resume of session '{}' for this start: pid {pid} is still alive and holds it; will retry on the next start",
        session.name
    );
    // A genuine session-lock conflict: the old CLI is still alive and holding
    // the session, which makes it unreachable right now, not dead. Leave the
    // row exactly as it is — still `active`, still holding its pid and
    // boot_id — so it's retried on every subsequent start and comes back on
    // its own once the holder exits.
}

/// Relaunch one interactive session under its own id and name, with the
/// harness's resume args (and its configured fresh args as a fallback).
fn resume_interactive_session(
    hub: &SessionHub,
    session: &InteractiveSession,
    canopy_config: &CanopyConfig,
    current_boot_id: Option<&str>,
) {
    let cli = Cli::from_str(&session.cli);
    let cli_config = canopy_config.get_cli(cli.as_str());
    let resume_args = build_resumed_session_args(
        session.args.as_deref(),
        cli_config.and_then(|config| config.interactive_args.as_deref()),
        cli_config.and_then(|config| config.resume_args.as_deref()),
        cli_config.and_then(|config| config.session_resume_cmd.as_deref()),
        cli_config.and_then(|config| config.yolo_flag.as_deref()),
    );
    let Some((spawned, used_args)) = spawn_resumed_or_fresh(hub, session, cli_config, resume_args)
    else {
        return;
    };
    finalize_resumed_session(
        hub,
        session,
        used_args.as_deref(),
        spawned.pid,
        current_boot_id,
    );
}

/// Spawn the resumed session, falling back to the platform's fresh
/// interactive args when the resume launch fails.
fn spawn_resumed_or_fresh(
    hub: &SessionHub,
    session: &InteractiveSession,
    cli_config: Option<&crate::domain::cli_config::CliConfig>,
    resume_args: Option<String>,
) -> Option<(Arc<HubSession>, Option<String>)> {
    let spawn = |args: Option<&str>| {
        hub.spawn(&SpawnRequest {
            kind: SessionKind::Agent,
            cli: session.cli.clone(),
            model: None,
            workdir: session.working_dir.clone(),
            name: Some(session.name.clone()),
            prompt: None,
            interactive_args: args.map(str::to_string),
            fallback_args: cli_config.and_then(|c| c.fallback_interactive_args.clone()),
            model_flag: cli_config.and_then(|c| c.model_flag.clone()),
            id: Some(session.id.clone()),
            seed_id: None,
            accent: resume_accent(cli_config),
        })
    };
    match spawn(resume_args.as_deref()) {
        Ok(agent) => Some((agent, resume_args)),
        Err(e) => {
            tracing::warn!(
                "Failed to auto-resume session '{}': {e}; retrying as a fresh session",
                session.name
            );
            let fresh_args = cli_config.and_then(|config| config.interactive_args.clone());
            match spawn(fresh_args.as_deref()) {
                Ok(agent) => Some((agent, fresh_args)),
                Err(e2) => {
                    tracing::warn!(
                        "Fresh-session fallback also failed for '{}': {e2}; closing session",
                        session.name
                    );
                    if let Err(e) = hub.db().mark_session_closed(&session.id) {
                        tracing::warn!("Failed to close session '{}': {e}", session.name);
                    }
                    None
                }
            }
        }
    }
}

/// Persist the resumed session once its PTY is up.
///
/// A resumed conversation keeps its canopy session id and name: the earlier
/// row is reactivated in place — only its liveness fields move — instead of
/// being superseded by a second, freshly-named row. That is what keeps hooks
/// and scheduled sends addressed to the session's name or id delivered to the
/// resumed process.
fn finalize_resumed_session(
    hub: &SessionHub,
    session: &InteractiveSession,
    used_args: Option<&str>,
    pid: Option<i64>,
    current_boot_id: Option<&str>,
) {
    if let Err(e) = hub.db().reactivate_interactive_session(
        &session.id,
        pid,
        &session.working_dir,
        used_args,
        current_boot_id,
    ) {
        tracing::warn!(
            "Failed to reactivate resumed session '{}': {e}",
            session.name
        );
    }
}

/// The accent a resumed agent advertises to its child CLI: the platform's
/// configured accent, else canopy's interactive color.
fn resume_accent(cli_config: Option<&crate::domain::cli_config::CliConfig>) -> Option<String> {
    if let Some([r, g, b]) = cli_config.and_then(|config| config.accent_color) {
        return Some(crate::sessions::format_accent_rgb((r, g, b)));
    }
    match crate::tui::ui::INTERACTIVE_COLOR {
        ratatui::style::Color::Rgb(r, g, b) => Some(crate::sessions::format_accent_rgb((r, g, b))),
        _ => None,
    }
}

/// Respawn every terminal session left `idle` by a previous run, replaying
/// the scrollback saved for it.
fn resume_terminal_sessions(hub: &SessionHub, data_dir: &Path) {
    let Ok(sessions) = hub.db().get_active_terminal_sessions() else {
        return;
    };
    if sessions.is_empty() {
        tracing::info!("No active terminal sessions to resume");
        return;
    }
    tracing::info!("Resuming {} terminal session(s)", sessions.len());
    if let Err(e) = hub.db().mark_orphaned_terminal_sessions() {
        tracing::warn!("Failed to mark orphaned terminal sessions: {e}");
    }

    for session in &sessions {
        resume_terminal_session(hub, data_dir, session);
    }
}

/// Respawn one terminal in its workdir with its shell, keeping its id and
/// name, and replay the history stored for that name.
fn resume_terminal_session(hub: &SessionHub, data_dir: &Path, session: &TerminalSession) {
    let request = SpawnRequest {
        kind: SessionKind::Terminal,
        cli: session.shell.clone(),
        model: None,
        workdir: session.working_dir.clone(),
        name: Some(session.name.clone()),
        prompt: None,
        interactive_args: None,
        fallback_args: None,
        model_flag: None,
        id: Some(session.id.clone()),
        seed_id: None,
        accent: None,
    };
    match hub.spawn(&request) {
        Ok(spawned) => {
            let history = crate::tui::terminal_history::load_history(data_dir, &spawned.name);
            spawned.replay_scrollback_lines(&history.scrollback);
        }
        Err(e) => tracing::warn!(
            "Failed to auto-resume terminal session '{}': {e}",
            session.name
        ),
    }
}

/// Check whether a process with the given pid is still alive.
///
/// Uses `kill(pid, 0)`: a `0` return means the process exists and is ours;
/// `EPERM` means it exists but is owned by someone else (still alive from our
/// point of view); `ESRCH` means it's gone.
#[cfg(unix)]
pub fn process_is_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    ret == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub fn process_is_alive(_pid: i64) -> bool {
    false
}

/// Whether the live process behind `pid` is actually an instance of `cli`.
/// `/proc/<pid>/comm` holds the executable's basename truncated to 15 bytes —
/// if it doesn't match, the PID was recycled by an unrelated process and the
/// session it came from is long gone.
#[cfg(target_os = "linux")]
pub fn process_matches_cli(pid: i64, cli: &str) -> bool {
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return false;
    };
    let want: String = cli.chars().take(15).collect();
    comm.trim() == want
}

/// Without procfs there is no cheap identity check — assume the PID is the
/// CLI so the conservative (grace-then-orphan) path handles it.
#[cfg(not(target_os = "linux"))]
pub fn process_matches_cli(_pid: i64, _cli: &str) -> bool {
    true
}

/// A stored-PID conflict is genuine only if the process is really this CLI
/// and it outlives a short grace window. A quick TUI close→reopen leaves the
/// old CLI mid-death for well under a second — waiting briefly turns what
/// used to be a permanent orphaning into a normal resume.
pub fn process_outlives_grace(pid: i64, cli: &str) -> bool {
    if pid <= 0 || !process_matches_cli(pid, cli) {
        return false;
    }
    for _ in 0..10 {
        std::thread::sleep(std::time::Duration::from_millis(150));
        if !process_is_alive(pid) {
            return false;
        }
    }
    true
}

/// Whether an interactive session should be auto-resumed on startup.
///
/// Boot-id rule: if the stored boot_id is `None` (legacy row) or differs
/// from the current machine boot_id the PID is meaningless — after a reboot
/// the OS recycles PIDs from the bottom, so a stored PID matching a live
/// process is a coincidence, not evidence the original process survived.
/// In that case we always resume.
///
/// Only when the stored boot_id matches the current one do we fall back to
/// the PID-aliveness check: a live PID means the CLI is still running and
/// resuming would fight it for the session lock.
pub fn should_resume_session(
    pid: Option<i64>,
    session_boot_id: Option<&str>,
    current_boot_id: Option<&str>,
) -> bool {
    // Different boot (or legacy NULL) → PID is meaningless, always resume.
    match (session_boot_id, current_boot_id) {
        (Some(s), Some(c)) if s == c => {}
        _ => return true,
    }
    // Same boot → trust the PID-aliveness check.
    match pid {
        Some(pid) => !process_is_alive(pid),
        None => true,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeDecision {
    Resume,
    SkipHeldByLiveProcess,
}

/// Pure decision for a session whose stored PID is alive on the current boot
/// (i.e. `should_resume_session` returned `false`). A PID that doesn't
/// outlive the grace window in `process_outlives_grace` was mid-death or
/// recycled, so resuming is safe. A PID that survives the full window is a
/// genuine session-lock conflict and must be skipped for this start.
pub fn resume_decision(pid_alive_same_boot: bool, outlives_grace: bool) -> ResumeDecision {
    if pid_alive_same_boot && outlives_grace {
        ResumeDecision::SkipHeldByLiveProcess
    } else {
        ResumeDecision::Resume
    }
}
