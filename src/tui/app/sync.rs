use crate::domain::sync::summarize_sync_context;

use super::types::{AgentEntry, App, SidebarLayer, SyncPanelState};

// Ceiling on ultrawide terminals. The panel is mostly wrapped prose (mission
// and message text), and past ~80 columns a single block of wrapped text
// gets harder to read, not more useful — so growth stops there rather than
// keeping pace with 30% of an arbitrarily wide screen. This only engages
// past ~267 columns of total width, far beyond the ~113 where the old fixed
// 34-column cap used to bind, so today's normal terminals are unaffected.
const ACTIVITY_PANEL_MAX_WIDTH: u16 = 80;
const ACTIVITY_PANEL_MIN_WIDTH: u16 = 24;
const ACTIVITY_PANEL_FORCED_MIN_WIDTH: u16 = 16;
const ACTIVITY_PANEL_PERCENT: u16 = 30;
const RECENT_MESSAGE_LIMIT: usize = 18;
const MAX_RECENT_MESSAGE_LIMIT: usize = 200;
const MESSAGE_WINDOW_LINES_PER_STEP: u16 = 6;
const MESSAGE_WINDOW_ITEMS_PER_STEP: usize = 8;
const CHATTER_LIMIT: usize = 8;

impl App {
    pub(crate) fn activity_panel_available(&self) -> bool {
        self.selected_activity_workdir().is_some()
    }

    /// Returns activity for the selected workdir without applying visibility rules.
    pub(crate) fn selected_activity_state(&self) -> Option<SyncPanelState> {
        let workdir = self.selected_activity_workdir()?;
        self.activity_panel_state_for_workdir(workdir)
    }

    /// Returns active missions for a workdir without the ≥2 session gate.
    ///
    /// Used by the system prompt so missions are always visible, even in solo
    /// mode. This is intentional: solo agents benefit from seeing prior mission
    /// history and stale missions that need cleanup.
    pub(crate) fn active_missions_for_workdir(
        &self,
        workdir: &str,
    ) -> Vec<crate::domain::sync::ActiveIntent> {
        let Ok(mut messages) = self
            .db
            .list_activity_log_entries(workdir, RECENT_MESSAGE_LIMIT)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(crate::domain::sync::SyncMessage::from)
                    .collect::<Vec<_>>()
            })
        else {
            return Vec::new();
        };
        // The bitácora stores `source` (e.g. "sync"), not the actor's display
        // name — resolve it the same way the activity face does so intents
        // carry a real identity, not the literal source tag.
        for message in &mut messages {
            if let Ok(Some(session_name)) =
                self.db.resolve_sync_actor_name(workdir, &message.agent_id)
            {
                message.agent_name = session_name;
            }
        }
        let active_agent_ids = messages
            .iter()
            .map(|m| m.agent_id.clone())
            .collect::<std::collections::HashSet<_>>();
        summarize_sync_context(&messages, &active_agent_ids, 0).active_intents
    }

    /// True when the selected sidebar entry is a terminal session.
    fn selected_session_is_terminal(&self) -> bool {
        matches!(self.selected_agent(), Some(AgentEntry::Terminal(_)))
    }

    pub(crate) fn activity_panel_state(&self) -> Option<SyncPanelState> {
        let state = self.selected_activity_state()?;
        if self.sidebar_layer == SidebarLayer::Knowledge {
            return Some(state);
        }
        // Terminal sessions hide the sync panel by default; an explicit toggle
        // (F3 → forced) still brings it up.
        if self.selected_session_is_terminal()
            && !self.forced_activity_workdirs.contains(&state.workdir)
        {
            return None;
        }
        (!self.hidden_activity_workdirs.contains(&state.workdir)).then_some(state)
    }

    /// The remote size of the selected session, if a remote device owns it.
    pub(crate) fn selected_remote_size(&self) -> Option<(u16, u16)> {
        match self.selected_agent()? {
            AgentEntry::Interactive(idx) => self.interactive_agents.get(*idx)?.remote_size,
            AgentEntry::Terminal(idx) => self.terminal_agents.get(*idx)?.remote_size,
            _ => None,
        }
    }

    /// Width that fills the strip a remote device freed by sizing the
    /// selected session narrower than the pane: the pane's inner width then
    /// equals the remote's cols exactly. `None` when no fill applies — no
    /// remote size, a split view (which keeps its own layout), a remote
    /// wider than the pane, or a strip below the forced minimum.
    pub(crate) fn remote_fill_width(&self, total_width: u16) -> Option<u16> {
        if self.active_split_id.is_some() {
            return None;
        }
        let (remote_cols, _) = self.selected_remote_size()?;
        let panel_inner = self.last_panel_inner.0;
        if panel_inner == 0 || remote_cols >= panel_inner {
            return None;
        }
        let sync_w = self.last_sync_area.map(|area| area.width).unwrap_or(0);
        let panel_w = total_width.saturating_sub(sync_w);
        let chrome = panel_w.saturating_sub(panel_inner);
        let fill = total_width.saturating_sub(remote_cols.saturating_add(chrome));
        if fill < ACTIVITY_PANEL_FORCED_MIN_WIDTH {
            return None;
        }
        Some(fill)
    }

    /// Clamp a dragged panel width between the panel minimum and the body
    /// minus 40 columns, so the main pane keeps a usable width.
    pub(crate) fn clamp_activity_panel_drag(&self, width: u16) -> u16 {
        let hi = self
            .last_body_width
            .saturating_sub(40)
            .max(ACTIVITY_PANEL_MIN_WIDTH);
        width.clamp(ACTIVITY_PANEL_MIN_WIDTH, hi)
    }

    /// Recompute the pane size from a finished border drag, so the single
    /// release resize targets the dragged width instead of the stale layout.
    pub(crate) fn apply_activity_panel_drag_size(&mut self) {
        let Some(user) = self.activity_panel_user_width else {
            return;
        };
        let body = self.last_body_width;
        if body == 0 {
            return;
        }
        let sync_w = self.last_sync_area.map(|area| area.width).unwrap_or(0);
        let chrome = body
            .saturating_sub(sync_w)
            .saturating_sub(self.last_panel_inner.0);
        let inner = u32::from(body).saturating_sub(u32::from(user) + u32::from(chrome));
        self.last_panel_inner.0 = u16::try_from(inner).unwrap_or(u16::MAX);
    }

    pub(crate) fn activity_panel_layout_width(&self, total_width: u16, enabled: bool) -> u16 {
        // A remote-owned strip fills first, whatever the visibility flag
        // says — the fill is what keeps the strip from sitting empty.
        if let Some(fill) = self.remote_fill_width(total_width) {
            return fill;
        }
        if !enabled {
            return 0;
        }
        // A drag outranks the proportional rule; the drag already clamped
        // it, and this re-clamps it against a resized terminal.
        if let Some(user) = self.activity_panel_user_width {
            let hi = total_width.saturating_sub(40);
            if hi >= ACTIVITY_PANEL_MIN_WIDTH {
                return user.clamp(ACTIVITY_PANEL_MIN_WIDTH, hi);
            }
        }

        let proportional_width = ((total_width as u32 * ACTIVITY_PANEL_PERCENT as u32 / 100)
            as u16)
            .min(ACTIVITY_PANEL_MAX_WIDTH);

        let force_shown = self
            .selected_activity_workdir()
            .is_some_and(|workdir| self.forced_activity_workdirs.contains(workdir));

        if proportional_width < ACTIVITY_PANEL_MIN_WIDTH {
            if force_shown {
                return ACTIVITY_PANEL_FORCED_MIN_WIDTH.min(total_width);
            }
            return 0;
        }

        proportional_width
    }

    pub(crate) fn selected_activity_workdir(&self) -> Option<&str> {
        if self.sidebar_layer == SidebarLayer::Knowledge {
            return self.selected_project().map(|project| project.path.as_str());
        }
        match self.selected_agent()? {
            AgentEntry::Interactive(idx) => self
                .interactive_agents
                .get(*idx)
                .map(|agent| agent.working_dir.as_str()),
            AgentEntry::Terminal(idx) => self
                .terminal_agents
                .get(*idx)
                .map(|agent| agent.working_dir.as_str()),
            AgentEntry::Agent(agent) => agent.working_dir.as_deref(),
            AgentEntry::Corrupt(_) => None,
            AgentEntry::Orphaned(idx) => self
                .orphaned_sessions
                .get(*idx)
                .map(|s| s.working_dir.as_str()),
            AgentEntry::Group(_) => None,
        }
    }

    pub(crate) fn activity_panel_state_for_workdir(&self, workdir: &str) -> Option<SyncPanelState> {
        let recent_limit = self.message_window_limit_for_scroll();
        let mut recent_messages = self
            .db
            .list_activity_log_entries(workdir, recent_limit)
            .map(|entries| {
                entries
                    .into_iter()
                    .map(crate::domain::sync::SyncMessage::from)
                    .collect::<Vec<_>>()
            })
            .ok()?;

        for message in &mut recent_messages {
            if let Ok(Some(session_name)) =
                self.db.resolve_sync_actor_name(workdir, &message.agent_id)
            {
                message.agent_name = session_name;
            }
        }

        let active_agent_ids = self
            .db
            .list_active_sync_agent_ids(workdir)
            .unwrap_or_default()
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        if recent_messages.is_empty() {
            if self.sidebar_layer != SidebarLayer::Knowledge {
                return None;
            }
            return Some(SyncPanelState {
                workdir: workdir.to_owned(),
                participant_count: active_agent_ids.len(),
                vibe: crate::domain::sync::WorkspaceStatus::Stable,
                active_intents: Vec::new(),
                recent_messages,
            });
        }
        let summary = summarize_sync_context(&recent_messages, &active_agent_ids, CHATTER_LIMIT);
        let participant_count = active_agent_ids.len().max(1);

        Some(SyncPanelState {
            workdir: workdir.to_owned(),
            participant_count,
            vibe: summary.vibe,
            active_intents: summary.active_intents,
            recent_messages,
        })
    }

    pub(crate) fn message_window_limit_for_scroll(&self) -> usize {
        let steps = (self.sync_scroll_offset / MESSAGE_WINDOW_LINES_PER_STEP) as usize;
        (RECENT_MESSAGE_LIMIT + steps.saturating_mul(MESSAGE_WINDOW_ITEMS_PER_STEP))
            .min(MAX_RECENT_MESSAGE_LIMIT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::models::{Agent, Cli};
    use crate::domain::project::Project;
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn sample_agent(id: &str, workdir: &str) -> Agent {
        Agent {
            id: id.to_string(),
            prompt: "Track activity".to_string(),
            trigger: None,
            cli: Cli::new("opencode"),
            model: None,
            effort: None,
            working_dir: Some(workdir.to_string()),
            enabled: true,
            enable_at: None,
            created_at: Utc::now(),
            log_path: "/tmp/test.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        }
    }

    fn sample_project(path: &str) -> Project {
        Project::new(path)
    }

    #[test]
    fn activity_panel_auto_shows_for_single_agent_when_messages_exist() {
        let db = test_db();
        // The panel renders the bitácora, not `sync_messages`.
        db.insert_activity_log_entry(
            "/tmp/project",
            "sync",
            Some("agent-a"),
            "info",
            "first activity",
            None,
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;

        let state = app
            .activity_panel_state()
            .expect("activity panel should render");

        assert_eq!(state.workdir, "/tmp/project");
        assert_eq!(state.participant_count, 1);
        assert_eq!(state.recent_messages.len(), 1);
    }

    #[test]
    fn activity_panel_reads_from_activity_log() {
        let db = test_db();
        // Seed ONLY the bitácora — nothing in sync_messages.
        db.insert_activity_log_entry(
            "/tmp/project",
            "graph",
            Some("graph-7"),
            "info",
            "bitacora-only event",
            None,
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;

        let state = app
            .activity_panel_state()
            .expect("activity panel should render from the bitácora");
        assert_eq!(state.workdir, "/tmp/project");
        assert_eq!(state.recent_messages.len(), 1);
        assert_eq!(state.recent_messages[0].message, "bitacora-only event");
        // Panel and direct read return the same entries.
        let direct = db.list_activity_log_entries("/tmp/project", 10).unwrap();
        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].message, state.recent_messages[0].message);
    }

    #[test]
    fn active_missions_resolves_actor_name_from_bitacora() {
        let db = test_db();
        db.insert_interactive_session(
            "agent-x",
            "test-display",
            "copilot",
            "/tmp/project",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let intent_payload = serde_json::to_string(&crate::domain::sync::IntentPayload {
            mission: "deploy".to_string(),
            impact: crate::domain::sync::MissionImpact::High,
            description: "ship it".to_string(),
        })
        .unwrap();
        db.insert_activity_log_entry(
            "/tmp/project",
            "sync",
            Some("agent-x"),
            "intent",
            "intent",
            Some(intent_payload.as_str()),
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let intents = app.active_missions_for_workdir("/tmp/project");
        assert_eq!(intents.len(), 1);
        assert_ne!(intents[0].agent_name, "sync");
        assert_eq!(intents[0].agent_name, "test-display · copilot");
    }

    #[test]
    fn activity_panel_hides_intents_for_inactive_agents() {
        let db = test_db();
        let intent_payload = serde_json::to_string(&crate::domain::sync::IntentPayload {
            mission: "Old mission".to_string(),
            impact: crate::domain::sync::MissionImpact::High,
            description: "should not show when inactive".to_string(),
        })
        .expect("serialize intent payload");
        // The panel renders the bitácora, not `sync_messages`.
        db.insert_activity_log_entry(
            "/tmp/project",
            "sync",
            Some("agent-a"),
            "intent",
            "intent",
            Some(intent_payload.as_str()),
        )
        .expect("insert intent");

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;

        let state = app
            .activity_panel_state()
            .expect("activity panel should render");

        assert!(state.active_intents.is_empty());
    }

    #[test]
    fn activity_panel_expands_message_window_with_scroll() {
        let db = test_db();
        for index in 0..(RECENT_MESSAGE_LIMIT + 4) {
            // The panel renders the bitácora, not `sync_messages`.
            db.insert_activity_log_entry(
                "/tmp/project",
                "sync",
                Some(&format!("agent-{index}")),
                "info",
                &format!("message-{index}"),
                None,
            )
            .expect("insert sync message");
        }

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;

        let compact = app
            .activity_panel_state()
            .expect("activity panel should render");
        assert_eq!(compact.recent_messages.len(), RECENT_MESSAGE_LIMIT);

        app.sync_scroll_offset = MESSAGE_WINDOW_LINES_PER_STEP;
        let expanded = app
            .activity_panel_state()
            .expect("activity panel should render");
        assert!(expanded.recent_messages.len() > RECENT_MESSAGE_LIMIT);
    }

    #[test]
    fn selected_activity_workdir_uses_selected_project_in_projects_mode() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let project = sample_project("/tmp/project");
        app.projects = vec![project];
        app.sidebar_layer = SidebarLayer::Knowledge;

        assert_eq!(app.selected_activity_workdir(), Some("/tmp/project"));
    }

    #[test]
    fn activity_panel_stays_visible_in_projects_mode_without_messages() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let project = sample_project("/tmp/project");
        app.projects = vec![project];
        app.sidebar_layer = SidebarLayer::Knowledge;

        let state = app
            .activity_panel_state()
            .expect("projects mode should keep activity panel visible");

        assert_eq!(state.workdir, "/tmp/project");
        assert!(state.recent_messages.is_empty());
        assert_eq!(state.participant_count, 0);
    }

    #[test]
    fn toggle_activity_panel_is_ignored_in_projects_mode() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let project = sample_project("/tmp/project");
        app.projects = vec![project];
        app.sidebar_layer = SidebarLayer::Knowledge;

        assert!(app.activity_panel_state().is_some());
        app.toggle_activity_panel();
        assert!(app.activity_panel_state().is_some());
    }

    #[test]
    fn explicit_activity_toggle_forces_width_on_narrow_screens() {
        let db = test_db();
        db.insert_activity_log_entry(
            "/tmp/project",
            "sync",
            Some("agent-a"),
            "info",
            "first activity",
            None,
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;
        app.hidden_activity_workdirs
            .insert("/tmp/project".to_string());

        assert_eq!(app.activity_panel_layout_width(60, true), 0);

        app.toggle_activity_panel();

        assert!(app.forced_activity_workdirs.contains("/tmp/project"));
        assert!(app.activity_panel_layout_width(60, true) > 0);
    }

    #[test]
    fn toggle_activity_panel_shows_when_auto_panel_is_suppressed_by_width() {
        let db = test_db();
        // The panel renders the bitácora, not `sync_messages`.
        db.insert_activity_log_entry(
            "/tmp/project",
            "sync",
            Some("agent-a"),
            "info",
            "first activity",
            None,
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;
        app.term_width = 60;

        assert_eq!(
            app.activity_panel_layout_width(app.term_width, app.activity_panel_state().is_some()),
            0
        );

        app.toggle_activity_panel();

        assert!(app.forced_activity_workdirs.contains("/tmp/project"));
        assert!(!app.hidden_activity_workdirs.contains("/tmp/project"));
        assert!(
            app.activity_panel_layout_width(app.term_width, app.activity_panel_state().is_some())
                > 0
        );
    }

    #[test]
    fn activity_panel_width_is_30_percent_clamped_between_24_and_80() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        assert_eq!(app.activity_panel_layout_width(100, true), 30);
        assert_eq!(app.activity_panel_layout_width(70, true), 0);
    }

    #[test]
    fn activity_panel_width_grows_proportionally_past_the_old_fixed_cap() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        // Narrow: below the minimum, panel is hidden.
        assert_eq!(app.activity_panel_layout_width(70, true), 0);
        // Right at the minimum threshold.
        assert_eq!(app.activity_panel_layout_width(80, true), 24);
        // Mid range: follows the 30% proportion.
        assert_eq!(app.activity_panel_layout_width(100, true), 30);
        // Where the old fixed 34-column cap used to bind — no longer capped.
        assert_eq!(app.activity_panel_layout_width(114, true), 34);
        assert_eq!(app.activity_panel_layout_width(150, true), 45);
        assert_eq!(app.activity_panel_layout_width(200, true), 60);
        // Approaching the new ceiling: still proportional, no jump.
        assert_eq!(app.activity_panel_layout_width(266, true), 79);
        assert_eq!(app.activity_panel_layout_width(267, true), 80);
        // Past the ceiling: bounded, even on an ultrawide terminal.
        assert_eq!(app.activity_panel_layout_width(300, true), 80);
        assert_eq!(app.activity_panel_layout_width(1000, true), 80);
    }

    #[test]
    fn activity_panel_width_uses_forced_minimum_when_narrow() {
        let db = test_db();
        db.insert_activity_log_entry(
            "/tmp/project",
            "sync",
            Some("agent-a"),
            "info",
            "first activity",
            None,
        )
        .unwrap();

        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;
        app.hidden_activity_workdirs
            .insert("/tmp/project".to_string());

        app.toggle_activity_panel();
        assert!(app.forced_activity_workdirs.contains("/tmp/project"));

        assert_eq!(app.activity_panel_layout_width(70, true), 16);
    }

    fn app_with_remote_terminal(remote: Option<(u16, u16)>) -> (App, tempfile::TempDir, String) {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let mut agent = crate::tui::agent::InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("remote-fill"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn terminal");
        agent.remote_size = remote;
        let id = agent.id.clone();
        app.terminal_agents.push(agent);
        app.agents = vec![AgentEntry::Terminal(0)];
        app.selected = 0;
        (app, data_dir, id)
    }

    #[test]
    fn remote_fill_makes_pane_inner_equal_remote_cols() {
        let (mut app, _dir, _id) = app_with_remote_terminal(Some((60, 20)));
        // Body 120, chrome 2: the fill leaves exactly the remote's 60.
        app.last_panel_inner = (118, 24);
        assert_eq!(app.remote_fill_width(120), Some(58));
        assert_eq!(app.activity_panel_layout_width(120, true), 58);
        assert_eq!(120 - 58 - 2, 60);
        app.terminal_agents[0].kill();
    }

    #[test]
    fn remote_fill_accounts_for_a_visible_sync_panel() {
        let (mut app, _dir, _id) = app_with_remote_terminal(Some((60, 20)));
        // The sync panel takes 30 of the 120 body columns; the pane keeps 90
        // with a 2-column chrome, so the fill is still 58.
        app.last_sync_area = Some(ratatui::layout::Rect::new(90, 0, 30, 24));
        app.last_panel_inner = (88, 24);
        assert_eq!(app.remote_fill_width(120), Some(58));
        app.terminal_agents[0].kill();
    }

    #[test]
    fn remote_fill_falls_through_when_nothing_to_fill() {
        let (mut app, _dir, _id) = app_with_remote_terminal(Some((130, 20)));
        app.last_panel_inner = (118, 24);
        // A remote wider than the pane changes nothing here.
        assert_eq!(app.remote_fill_width(120), None);
        assert_eq!(app.activity_panel_layout_width(120, true), 36);

        // A strip below the forced minimum leaves the layout as it is:
        // 120 - (110 + 2) = 8 < 16.
        app.terminal_agents[0].remote_size = Some((110, 20));
        assert_eq!(app.remote_fill_width(120), None);
        assert_eq!(app.activity_panel_layout_width(120, true), 36);

        // A split view keeps its own layout.
        app.terminal_agents[0].remote_size = Some((60, 20));
        app.active_split_id = Some("split".to_string());
        assert_eq!(app.remote_fill_width(120), None);
        assert_eq!(app.activity_panel_layout_width(120, true), 36);
        app.terminal_agents[0].kill();
    }

    #[test]
    fn user_width_beats_proportional_but_loses_to_remote_fill() {
        let (mut app, _dir, _id) = app_with_remote_terminal(None);
        app.last_panel_inner = (118, 24);
        app.activity_panel_user_width = Some(40);
        assert_eq!(app.activity_panel_layout_width(120, true), 40);

        // The remote fill outranks the drag.
        app.terminal_agents[0].remote_size = Some((60, 20));
        assert_eq!(app.activity_panel_layout_width(120, true), 58);

        // Without a remote size the drag clamps at both ends: body - 40
        // above, the panel minimum below.
        app.terminal_agents[0].remote_size = None;
        app.activity_panel_user_width = Some(200);
        assert_eq!(app.activity_panel_layout_width(120, true), 80);
        app.activity_panel_user_width = Some(10);
        assert_eq!(app.activity_panel_layout_width(120, true), 24);
        app.terminal_agents[0].kill();
    }

    #[test]
    fn remote_fill_forces_the_panel_shown() {
        let (mut app, _dir, _id) = app_with_remote_terminal(Some((60, 20)));
        app.last_panel_inner = (118, 24);
        assert_eq!(app.activity_panel_layout_width(120, false), 58);

        app.terminal_agents[0].remote_size = None;
        assert_eq!(app.activity_panel_layout_width(120, false), 0);
        app.terminal_agents[0].kill();
    }
}
