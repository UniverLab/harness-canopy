//! App session lifecycle methods moved from the composition root.
//!
//! The daemon owns the PTYs and resumes them itself (see
//! [`crate::sessions::resume`]); the TUI keeps only what it still does: the
//! explicit revive of one selected orphaned session, the panel geometry, and
//! the bridge-row sweep.

use super::*;

use crate::sessions::resume::process_is_alive;

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
            session.harness_conversation_id.as_deref(),
        );
        let Some(agent) =
            self.spawn_resumed_or_fresh(session, cli_config, resume_args.as_deref(), cols, rows)
        else {
            return;
        };
        self.finalize_resumed_session(session, agent, current_boot_id);
    }

    fn spawn_resumed_or_fresh(
        &self,
        session: &crate::db::session::InteractiveSession,
        cli_config: Option<&crate::domain::cli_config::CliConfig>,
        resume_args: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Option<InteractiveAgent> {
        let cli = crate::domain::models::Cli::from_str(&session.cli);
        let existing_ids = self.interactive_agent_names();
        let since = std::time::SystemTime::now();
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
                session.harness_conversation_id.as_deref(),
                true,
            )
        };
        match spawn(resume_args) {
            Ok(agent) => Some(agent),
            Err(e) => {
                tracing::warn!(
                    "Failed to auto-resume session '{}': {e}; retrying as a fresh session",
                    session.name
                );
                let fresh_args = cli_config
                    .and_then(|config| config.interactive_args.as_deref())
                    .map(str::to_string);
                match spawn(fresh_args.as_deref()) {
                    Ok(agent) => {
                        // The fresh fallback is a new conversation: forget the
                        // stale harness id and record the new one, like a
                        // fresh launch does.
                        if cli.as_str() == "muse" {
                            let _ = self.db.clear_session_harness_conversation_id(&session.id);
                            crate::sessions::muse_conversation::record_fresh_conversation(
                                self.db.clone(),
                                session.id.clone(),
                                session.working_dir.clone(),
                                since,
                                agent.pid(),
                                crate::sessions::muse_conversation::muse_store_dir(),
                            );
                        }
                        Some(agent)
                    }
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

    /// Persist the revived session after its PTY is up.
    ///
    /// A resumed harness conversation keeps its canopy session id and name:
    /// the earlier row is reactivated in place — only its liveness fields
    /// move — instead of being superseded by a second, freshly-named row. That
    /// is what keeps hooks and scheduled sends addressed to the session's name
    /// or id delivered to the resumed process, and why there is nothing to
    /// reassign or inherit: the sends and the persisted protocol flag already
    /// point at this same id. Only a source name held by another live session
    /// changes the name (FR5), keeping the id, and the header then names both.
    /// The recorded launch args are never rewritten with the resume command
    /// line: it is rebuilt from the stored harness conversation id at every
    /// startup.
    fn finalize_resumed_session(
        &mut self,
        session: &crate::db::session::InteractiveSession,
        agent: InteractiveAgent,
        current_boot_id: Option<&str>,
    ) {
        if let Err(e) = self.db.reactivate_keep_args(
            &session.id,
            agent.pid(),
            &session.working_dir,
            current_boot_id,
        ) {
            tracing::warn!(
                "Failed to reactivate resumed session '{}': {e}",
                session.name
            );
        }
        // The hub mints a fresh name when another live session holds the
        // source name (FR5): the revive keeps the id, and the header names
        // both so the rename is never silent.
        if agent.name != session.name {
            use crate::tui::whimsg::events::{EventKind, WhimEvent};
            self.whimsg.notify_event(WhimEvent::with_detail(
                EventKind::SessionRenamed,
                session.name.clone(),
                agent.name.clone(),
            ));
        }
        self.interactive_agents.push(agent);
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
}
