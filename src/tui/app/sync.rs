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
const ACTIVITY_PANEL_MAIN_MIN_WIDTH: u16 = 40;
const ACTIVITY_PANEL_MAX_USER_WIDTH: u16 = 120;
const ACTIVITY_PANEL_PERCENT: u16 = 30;
const RECENT_MESSAGE_LIMIT: usize = 18;
const MAX_RECENT_MESSAGE_LIMIT: usize = 200;
const MESSAGE_WINDOW_LINES_PER_STEP: u16 = 6;
const MESSAGE_WINDOW_ITEMS_PER_STEP: usize = 8;

impl App {
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

    pub(crate) fn activity_panel_state(&self) -> Option<SyncPanelState> {
        self.selected_activity_state()
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
    /// remote size, a remote wider than the pane, or a strip below the
    /// panel minimum.
    pub(crate) fn remote_fill_width(&self, total_width: u16) -> Option<u16> {
        let (remote_cols, _) = self.selected_remote_size()?;
        let panel_inner = self.last_panel_inner.0;
        if panel_inner == 0 || remote_cols >= panel_inner {
            return None;
        }
        let sync_w = self.last_sync_area.map(|area| area.width).unwrap_or(0);
        let panel_w = total_width.saturating_sub(sync_w);
        let chrome = panel_w.saturating_sub(panel_inner);
        let fill = total_width.saturating_sub(remote_cols.saturating_add(chrome));
        if fill < ACTIVITY_PANEL_MIN_WIDTH {
            return None;
        }
        Some(fill)
    }

    /// Clamp a dragged panel width between the panel minimum and the body
    /// minus 40 columns (capped at 120), so the main pane keeps a usable width.
    pub(crate) fn clamp_activity_panel_drag(&self, width: u16) -> u16 {
        let hi = self
            .last_body_width
            .saturating_sub(ACTIVITY_PANEL_MAIN_MIN_WIDTH)
            .clamp(ACTIVITY_PANEL_MIN_WIDTH, ACTIVITY_PANEL_MAX_USER_WIDTH);
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
        if !enabled || total_width < ACTIVITY_PANEL_MIN_WIDTH + ACTIVITY_PANEL_MAIN_MIN_WIDTH {
            return 0;
        }
        // A drag (or persisted width) outranks the proportional rule; this
        // re-clamps it against a narrower terminal or the 120-column cap.
        let hi = total_width
            .saturating_sub(ACTIVITY_PANEL_MAIN_MIN_WIDTH)
            .min(ACTIVITY_PANEL_MAX_USER_WIDTH);
        if let Some(user) = self.activity_panel_user_width {
            return user.clamp(ACTIVITY_PANEL_MIN_WIDTH, hi);
        }

        ((total_width as u32 * ACTIVITY_PANEL_PERCENT as u32 / 100) as u16)
            .clamp(ACTIVITY_PANEL_MIN_WIDTH, ACTIVITY_PANEL_MAX_WIDTH)
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
        }
    }

    pub(crate) fn activity_panel_state_for_workdir(&self, workdir: &str) -> Option<SyncPanelState> {
        // The sync board is gone: the panel keeps its place for the graph
        // cards and is available for every selected workdir.
        if workdir.is_empty() {
            return None;
        }
        Some(SyncPanelState)
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
    use tempfile::tempdir;

    fn test_db() -> Arc<Database> {
        let path = crate::test_support::temp_db_path();
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
    fn activity_panel_auto_shows_for_selected_workdir() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;

        app.activity_panel_state()
            .expect("activity panel should render");

        assert_eq!(app.selected_activity_workdir(), Some("/tmp/project"));
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
    fn activity_panel_needs_no_messages_to_show() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.selected = 0;

        app.activity_panel_state()
            .expect("activity panel should render without messages");

        assert_eq!(app.selected_activity_workdir(), Some("/tmp/project"));
    }

    #[test]
    fn message_window_grows_with_scroll_and_caps() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        assert_eq!(app.message_window_limit_for_scroll(), RECENT_MESSAGE_LIMIT);
        app.sync_scroll_offset = MESSAGE_WINDOW_LINES_PER_STEP;
        assert_eq!(
            app.message_window_limit_for_scroll(),
            RECENT_MESSAGE_LIMIT + MESSAGE_WINDOW_ITEMS_PER_STEP
        );
        app.sync_scroll_offset = u16::MAX;
        assert_eq!(
            app.message_window_limit_for_scroll(),
            MAX_RECENT_MESSAGE_LIMIT
        );
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

        app.activity_panel_state()
            .expect("projects mode should keep activity panel visible");

        assert_eq!(app.selected_activity_workdir(), Some("/tmp/project"));
    }

    #[test]
    fn activity_panel_shown_for_terminal_and_interactive_at_64_hidden_at_63() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        let term = crate::tui::agent::InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("term-act"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn terminal");
        let inter = crate::tui::agent::InteractiveAgent::spawn(
            crate::domain::models::Cli::new("cat"),
            "/tmp",
            80,
            24,
            None,
            None,
            ratatui::style::Color::White,
            Some("inter-act"),
            None,
            &[],
            None,
            None,
            None,
            None,
            false,
        )
        .expect("spawn interactive");
        app.terminal_agents.push(term);
        app.interactive_agents.push(inter);
        app.reindex_sessions();

        for idx in 0..app.agents.len() {
            app.selected = idx;
            let enabled = app.activity_panel_state().is_some();
            assert!(enabled, "session {idx} should expose activity panel state");
            assert_eq!(app.activity_panel_layout_width(64, enabled), 24);
            assert_eq!(app.activity_panel_layout_width(63, enabled), 0);
        }

        app.terminal_agents[0].kill();
        app.interactive_agents[0].kill();
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

        assert_eq!(app.activity_panel_layout_width(63, true), 0);
        assert_eq!(app.activity_panel_layout_width(64, true), 24);
        assert_eq!(app.activity_panel_layout_width(70, true), 24);
        assert_eq!(app.activity_panel_layout_width(100, true), 30);
        assert_eq!(app.activity_panel_layout_width(267, true), 80);
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

        // Narrow: below 64 columns, panel is hidden.
        assert_eq!(app.activity_panel_layout_width(63, true), 0);
        // 64..=79 clamps up to 24.
        assert_eq!(app.activity_panel_layout_width(64, true), 24);
        assert_eq!(app.activity_panel_layout_width(79, true), 24);
        // Right at the 30% = 24 threshold.
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
    fn stored_width_clamped_on_narrow_terminal_and_above_120() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        // Clamped on a narrower terminal (70 - 40 = 30).
        app.activity_panel_user_width = Some(80);
        assert_eq!(app.activity_panel_layout_width(70, true), 30);
        assert_eq!(app.activity_panel_layout_width(64, true), 24);

        // Clamped at 120 on wide terminals, and 24 at the lower end.
        app.activity_panel_user_width = Some(150);
        assert_eq!(app.activity_panel_layout_width(300, true), 120);
        app.activity_panel_user_width = Some(10);
        assert_eq!(app.activity_panel_layout_width(120, true), 24);
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
        app.reindex_sessions();
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

        // A strip below the panel minimum leaves the layout as it is:
        // 120 - (98 + 2) = 20 < 24.
        app.terminal_agents[0].remote_size = Some((98, 20));
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

    #[test]
    fn remote_fill_shows_panel_below_64_columns() {
        let (mut app, _dir, _id) = app_with_remote_terminal(Some((30, 20)));
        app.last_panel_inner = (58, 24);
        assert_eq!(app.activity_panel_layout_width(60, true), 28);
        assert_eq!(app.activity_panel_layout_width(60, false), 28);
        app.terminal_agents[0].kill();
    }
}
