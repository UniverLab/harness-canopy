//! App session lifecycle methods moved from the composition root.

use super::*;

impl App {
    pub(crate) fn session_panel_size() -> (u16, u16) {
        let (tw, th) = ratatui::crossterm::terminal::size().unwrap_or((120, 40));
        (tw.saturating_sub(28), th.saturating_sub(4))
    }

    pub(crate) fn interactive_agent_names(&self) -> Vec<&str> {
        self.interactive_agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect()
    }

    pub(crate) fn terminal_agent_names(&self) -> Vec<&str> {
        self.terminal_agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect()
    }

    pub(crate) fn resume_session_accent(
        cli_config: Option<&crate::domain::cli_config::CliConfig>,
    ) -> ratatui::style::Color {
        cli_config
            .and_then(|config| config.accent_color)
            .map(|[r, g, b]| ratatui::style::Color::Rgb(r, g, b))
            .unwrap_or(crate::tui::ui::INTERACTIVE_COLOR)
    }

    pub(crate) fn resume_interactive_session(
        &mut self,
        session: &crate::db::session::InteractiveSession,
        canopy_config: &crate::domain::canopy_config::CanopyConfig,
        cols: u16,
        rows: u16,
        current_boot_id: Option<&str>,
    ) {
        let cli = crate::domain::models::Cli::from_str(&session.cli);
        let cli_config = canopy_config.get_cli(cli.as_str());
        let resume_args = build_resumed_session_args(
            session.args.as_deref(),
            cli_config.and_then(|config| config.interactive_args.as_deref()),
            cli_config.and_then(|config| config.resume_args.as_deref()),
            cli_config.and_then(|config| config.session_resume_cmd.as_deref()),
            cli_config.and_then(|config| config.yolo_flag.as_deref()),
        );
        let Some((agent, used_args)) =
            self.spawn_resumed_or_fresh(session, cli_config, resume_args, cols, rows)
        else {
            return;
        };
        self.finalize_resumed_session(session, used_args.as_ref(), agent, current_boot_id);
    }

    fn spawn_resumed_or_fresh(
        &self,
        session: &crate::db::session::InteractiveSession,
        cli_config: Option<&crate::domain::cli_config::CliConfig>,
        resume_args: Option<String>,
        cols: u16,
        rows: u16,
    ) -> Option<(InteractiveAgent, Option<String>)> {
        let cli = crate::domain::models::Cli::from_str(&session.cli);
        let existing_ids = self.interactive_agent_names();
        let spawn = |args: Option<&str>| {
            InteractiveAgent::spawn(
                cli.clone(),
                &session.working_dir,
                cols,
                rows,
                args,
                cli_config.and_then(|config| config.fallback_interactive_args.as_deref()),
                Self::resume_session_accent(cli_config),
                Some(&session.name),
                Some(&session.id),
                &existing_ids,
                None,
                cli_config.and_then(|config| config.model_flag.as_deref()),
                None,
            )
        };
        match spawn(resume_args.as_deref()) {
            Ok(agent) => Some((agent, resume_args)),
            Err(e) => {
                tracing::warn!(
                    "Failed to auto-resume session '{}': {e}; retrying as a fresh session",
                    session.name
                );
                let fresh_args = cli_config
                    .and_then(|config| config.interactive_args.as_deref())
                    .map(str::to_string);
                match spawn(fresh_args.as_deref()) {
                    Ok(agent) => Some((agent, fresh_args)),
                    Err(e2) => {
                        tracing::warn!(
                            "Fresh-session fallback also failed for '{}': {e2}; closing session",
                            session.name
                        );
                        let _ = self.db.mark_session_closed(&session.id);
                        None
                    }
                }
            }
        }
    }

    /// Persist the resumed session after its PTY is up.
    ///
    /// A resumed harness conversation keeps its canopy session id and name:
    /// the earlier row is reactivated in place — only its liveness fields
    /// move — instead of being superseded by a second, freshly-named row. That
    /// is what keeps hooks and scheduled sends addressed to the session's name
    /// or id delivered to the resumed process (FR2, FR4, FR5), and why there is
    /// nothing to reassign or inherit: the sends and the persisted
    /// protocol flag already point at this same id.
    fn finalize_resumed_session(
        &mut self,
        session: &crate::db::session::InteractiveSession,
        used_args: Option<&String>,
        agent: InteractiveAgent,
        current_boot_id: Option<&str>,
    ) {
        if let Err(e) = self.db.reactivate_interactive_session(
            &session.id,
            agent.pid(),
            &session.working_dir,
            used_args.map(String::as_str),
            current_boot_id,
        ) {
            tracing::warn!(
                "Failed to reactivate resumed session '{}': {e}",
                session.name
            );
        }
        self.interactive_agents.push(agent);
    }

    /// Finalize scheduled-send restore after auto-resume: any pending schedule
    /// whose target session was not resumed (its session no longer exists) is
    /// dropped silently, then the delivery gate opens so due schedules — past
    /// due ones included — fire on the next tick. Idempotent; safe to call once
    /// on startup even when there are no sessions.
    pub fn restore_scheduled_sends(&mut self) {
        let live_ids: Vec<String> = self
            .interactive_agents
            .iter()
            .map(|agent| agent.id.clone())
            .collect();
        if let Err(e) = self.db.drop_scheduled_sends_missing_targets(&live_ids) {
            tracing::warn!("Failed to drop scheduled sends for missing sessions: {e}");
        }
        self.scheduled_sends_restored = true;
    }

    pub(crate) fn resume_terminal_session(
        &mut self,
        session: &crate::db::session::TerminalSession,
        cols: u16,
        rows: u16,
    ) {
        let existing_refs = self.terminal_agent_names();
        let agent = match InteractiveAgent::spawn_terminal(
            &session.shell,
            &session.working_dir,
            cols,
            rows,
            Some(&session.name),
            &existing_refs,
            self.theme.header_color,
        ) {
            Ok(agent) => agent,
            Err(e) => {
                tracing::warn!(
                    "Failed to auto-resume terminal session '{}': {e}",
                    session.name
                );
                return;
            }
        };

        let _ = self.db.insert_terminal_session(
            &agent.id,
            &agent.name,
            &session.shell,
            &session.working_dir,
        );
        let hist = crate::tui::terminal_history::load_history(&self.data_dir, &agent.name);
        agent.replay_scrollback_lines(&hist.scrollback);
        self.terminal_histories.insert(agent.name.clone(), hist);
        self.terminal_agents.push(agent);
    }

    /// Reap bridge sidecar rows left `active` by a process that died without
    /// calling `finish_standalone_session` (daemon restart, MCP client
    /// killed). Bridge sessions are never auto-resumed (see
    /// `get_active_sessions`), so without this their rows accumulate as
    /// `active` forever instead of just going stale.
    pub fn reconcile_bridge_sessions(&self) {
        let Ok(sessions) = self.db.get_active_sessions_by_type("bridge") else {
            return;
        };
        for session in &sessions {
            // Bridges are internal daemon processes — a live PID always means
            // the bridge is running, regardless of boot_id (unlike user CLI
            // sessions where PID recycling after reboot makes PIDs unreliable).
            let dead = match session.pid {
                Some(pid) => !process_is_alive(pid),
                None => true,
            };
            if dead {
                let _ = self.db.finish_interactive_session(&session.id, 1);
            }
        }
    }

    pub fn auto_resume_sessions(&mut self) {
        self.sweep_orphaned_sessions();

        let Ok(sessions) = self.db.get_active_sessions() else {
            return;
        };
        if sessions.is_empty() {
            tracing::info!("No active sessions to resume");
            return;
        }
        tracing::info!("Resuming {} active session(s)", sessions.len());

        let current_boot_id = crate::system::boot_id();
        let home = dirs::home_dir().unwrap_or_default();
        let canopy_config = crate::domain::canopy_config::CanopyConfig::load(&home.join(".canopy"));
        let (cols, rows) = Self::session_panel_size();

        for session in &sessions {
            self.resume_scanned_session(
                session,
                &canopy_config,
                cols,
                rows,
                current_boot_id.as_deref(),
            );
        }

        if !self.interactive_agents.is_empty() {
            let _ = self.refresh_agents();
        }
    }

    /// Startup sweep (B32): retire any row still in the removed `orphaned`
    /// status to `completed` so historic red orphan cards disappear. Runs
    /// before `auto_resume_sessions`'s own resume attempts and before
    /// `restore_scheduled_sends` (see `tui/mod.rs` startup order): a swept
    /// session is not resumed, so it never joins `interactive_agents`, and
    /// `restore_scheduled_sends`'s missing-target drop then discards any
    /// `scheduled_sends` that still pointed at it.
    pub(crate) fn sweep_orphaned_sessions(&self) {
        match self.db.close_orphaned_interactive_sessions() {
            Ok(n) if n > 0 => {
                tracing::info!("Closed {n} orphaned interactive session(s) on startup");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("Failed to sweep orphaned interactive sessions: {e}"),
        }
    }

    pub(crate) fn resume_scanned_session(
        &mut self,
        session: &crate::db::session::InteractiveSession,
        canopy_config: &crate::domain::canopy_config::CanopyConfig,
        cols: u16,
        rows: u16,
        current_boot_id: Option<&str>,
    ) {
        if should_resume_session(session.pid, session.boot_id.as_deref(), current_boot_id) {
            self.resume_interactive_session(session, canopy_config, cols, rows, current_boot_id);
            return;
        }
        self.resume_or_defer_conflicting_session(
            session,
            canopy_config,
            cols,
            rows,
            current_boot_id,
        );
    }

    pub(crate) fn resume_or_defer_conflicting_session(
        &mut self,
        session: &crate::db::session::InteractiveSession,
        canopy_config: &crate::domain::canopy_config::CanopyConfig,
        cols: u16,
        rows: u16,
        current_boot_id: Option<&str>,
    ) {
        // The stored PID is alive on the same boot — but that alone is
        // NOT proof of a session-lock conflict. Two benign cases used
        // to permanently orphan healthy sessions here:
        //  * quick TUI close→reopen: the old CLI got its HUP and is
        //    still in the middle of dying;
        //  * PID recycling: the number now belongs to an unrelated
        //    process.
        // Only a live process that actually IS this CLI, and that
        // survives a short grace period, is a genuine conflict.
        let pid = session.pid.unwrap_or(0);
        if matches!(
            resume_decision(true, process_outlives_grace(pid, &session.cli)),
            ResumeDecision::Resume
        ) {
            tracing::info!(
                "Auto-resuming session '{}': stored pid {pid} was recycled or exited during grace",
                session.name
            );
            self.resume_interactive_session(session, canopy_config, cols, rows, current_boot_id);
            return;
        }
        tracing::warn!(
            "Skipping auto-resume of session '{}' for this start: pid {pid} is still alive and holds it; will retry on the next start",
            session.name
        );
        // A genuine session-lock conflict: the old CLI is still alive
        // and holding the session, which makes it unreachable right
        // now, not dead. Leave the row exactly as it is — still
        // `active`, still holding its pid and boot_id — so it's
        // retried on every subsequent start and comes back on its own
        // once the holder exits. There's nothing to clean up in the
        // meantime either: only resumed sessions join
        // `interactive_agents`, so a skipped row is simply absent
        // from this run's sidebar.
    }

    pub fn auto_resume_terminal_sessions(&mut self) {
        let Ok(sessions) = self.db.get_active_terminal_sessions() else {
            return;
        };
        if sessions.is_empty() {
            tracing::info!("No active terminal sessions to resume");
            return;
        }
        tracing::info!("Resuming {} terminal session(s)", sessions.len());
        let _ = self.db.mark_orphaned_terminal_sessions();

        let (cols, rows) = Self::session_panel_size();

        for session in &sessions {
            self.resume_terminal_session(session, cols, rows);
        }

        if !self.terminal_agents.is_empty() {
            let _ = self.refresh_agents();
        }
    }
}

/// Check whether a process with the given pid is still alive.
///
/// Uses `kill(pid, 0)`: a `0` return means the process exists and is ours;
/// `EPERM` means it exists but is owned by someone else (still alive from our
/// point of view); `ESRCH` means it's gone.
#[cfg(unix)]
pub(crate) fn process_is_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    ret == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
pub(crate) fn process_is_alive(_pid: i64) -> bool {
    false
}

/// Whether the live process behind `pid` is actually an instance of `cli`.
/// `/proc/<pid>/comm` holds the executable's basename truncated to 15 bytes —
/// if it doesn't match, the PID was recycled by an unrelated process and the
/// session it came from is long gone.
#[cfg(target_os = "linux")]
pub(crate) fn process_matches_cli(pid: i64, cli: &str) -> bool {
    let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) else {
        return false;
    };
    let want: String = cli.chars().take(15).collect();
    comm.trim() == want
}

/// Without procfs there is no cheap identity check — assume the PID is the
/// CLI so the conservative (grace-then-orphan) path handles it.
#[cfg(not(target_os = "linux"))]
pub(crate) fn process_matches_cli(_pid: i64, _cli: &str) -> bool {
    true
}

/// A stored-PID conflict is genuine only if the process is really this CLI
/// and it outlives a short grace window. A quick TUI close→reopen leaves the
/// old CLI mid-death for well under a second — waiting briefly turns what
/// used to be a permanent orphaning into a normal resume.
pub(crate) fn process_outlives_grace(pid: i64, cli: &str) -> bool {
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
pub(crate) fn should_resume_session(
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
pub(crate) enum ResumeDecision {
    Resume,
    SkipHeldByLiveProcess,
}

/// Pure decision for a session whose stored PID is alive on the current boot
/// (i.e. `should_resume_session` returned `false`). A PID that doesn't
/// outlive the grace window in `process_outlives_grace` was mid-death or
/// recycled, so resuming is safe. A PID that survives the full window is a
/// genuine session-lock conflict and must be skipped for this start.
pub(crate) fn resume_decision(pid_alive_same_boot: bool, outlives_grace: bool) -> ResumeDecision {
    if pid_alive_same_boot && outlives_grace {
        ResumeDecision::SkipHeldByLiveProcess
    } else {
        ResumeDecision::Resume
    }
}
