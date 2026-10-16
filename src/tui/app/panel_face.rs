//! CT1 multi-face right panel: the switching rule.
//!
//! One panel, three faces — **activity**, **knowledge**, **graph** — with a
//! strict priority: a pinned face always wins; otherwise a running graph is
//! the resting face (a STATE); knowledge insertion and backlog change are
//! EVENTS that take the panel for a 10-second dwell and then fall back.
//! A newer event replaces the current dwell and restarts it; events never
//! queue. While the user is interacting with the panel (scrolling it, or
//! with focus inside it) no automatic switch happens and the pending switch
//! is dropped, not deferred.
//!
//! The tick only ever writes `App::panel_face` and its bookkeeping — it
//! never touches `App::focus`, so an automatic switch can never steal
//! keyboard focus — and it only assigns when the computed face differs, so
//! rendering never flickers or forces a full-screen redraw.

use std::time::{Duration, Instant};

use super::types::{App, PanelFace};
use crate::application::ports::StateRepository;
use crate::domain::graphs::GraphStatus;

impl App {
    /// STATE input: true while any graph is running, regardless of workdir.
    pub(crate) fn panel_graph_running(&self) -> bool {
        self.graphs
            .iter()
            .any(|lp| lp.status == GraphStatus::Running)
    }

    /// The resting face: Graph while a graph is running, Activity otherwise.
    pub(crate) fn panel_resting_face(&self) -> PanelFace {
        if self.panel_graph_running() {
            PanelFace::Graph
        } else {
            PanelFace::Activity
        }
    }

    /// True while the user is interacting with the panel: scrolled recently,
    /// scrolled over the panel this tick, or focused inside it. While true,
    /// automatic switches are dropped, not deferred.
    pub(crate) fn panel_is_interacting(&self) -> bool {
        self.panel_focused || self.panel_interacting
    }

    /// Record a knowledge/backlog event: it takes the panel for a dwell of
    /// `tui.panel_dwell_seconds` (default 10; 0 disables the dwell). A newer
    /// event during a dwell replaces it and restarts the dwell — events never
    /// queue. While pinned or interacting the event is dropped outright.
    pub(crate) fn fire_panel_event(&mut self, face: PanelFace, reason: &str) {
        if self.panel_pinned.is_some() || self.panel_is_interacting() {
            return;
        }
        self.panel_dwell_face = Some(face);
        self.panel_dwell_until =
            Some(Instant::now() + Duration::from_secs(self.tui_settings.panel_dwell_seconds));
        self.panel_dwell_reason = Some(reason.to_string());
    }

    fn panel_dwell_valid(&self) -> bool {
        match (self.panel_dwell_face, self.panel_dwell_until) {
            (Some(_), Some(until)) => Instant::now() < until,
            _ => false,
        }
    }

    fn clear_panel_dwell(&mut self) {
        self.panel_dwell_face = None;
        self.panel_dwell_until = None;
        self.panel_dwell_reason = None;
    }

    /// Recompute the visible face from the switching rule. Called once per
    /// refresh tick, after the data refreshes it reads have run.
    pub(crate) fn tick_panel_face(&mut self) {
        let graph_running = self.panel_graph_running();
        let project = self
            .selected_project()
            .map(|project| (project.hash.clone(), project.path.clone()));
        let knowledge_updated = project.as_ref().and_then(|(hash, _)| {
            self.db
                .max_project_knowledge_updated_at(hash)
                .ok()
                .flatten()
        });
        let backlog_updated = self
            .db
            .max_backlog_updated_at(project.as_ref().map(|(_, path)| path.as_str()))
            .ok()
            .flatten();

        if !self.panel_baselines_init {
            self.panel_last_knowledge_updated = knowledge_updated;
            self.panel_last_backlog_updated = backlog_updated;
            self.panel_last_graph_running = graph_running;
            self.panel_baselines_init = true;
            self.panel_face = self
                .panel_pinned
                .unwrap_or_else(|| self.panel_resting_face());
            self.panel_last_reason = None;
            self.panel_interacting = false;
            return;
        }

        // EVENT inputs. Knowledge insertion and backlog change both take the
        // Knowledge face; when both change on the same tick the backlog
        // change is the newer event and wins the dwell.
        let knowledge_changed = match (self.panel_last_knowledge_updated, knowledge_updated) {
            (None, Some(_)) => true,
            (Some(previous), Some(current)) => current > previous,
            _ => false,
        };
        if knowledge_changed {
            self.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        }
        let backlog_changed = match (self.panel_last_backlog_updated, backlog_updated) {
            (None, Some(_)) => true,
            (Some(previous), Some(current)) => current > previous,
            _ => false,
        };
        if backlog_changed {
            self.fire_panel_event(PanelFace::Knowledge, "backlog changed");
        }
        self.panel_last_knowledge_updated = knowledge_updated;
        self.panel_last_backlog_updated = backlog_updated;
        self.panel_last_graph_running = graph_running;

        if self
            .panel_dwell_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.clear_panel_dwell();
        }

        // A pinned face always wins. While pinned, nothing switches — clear
        // any dwell so unpinning returns to a clean automatic state.
        if let Some(pinned) = self.panel_pinned {
            self.clear_panel_dwell();
            self.panel_interacting = false;
            if self.panel_face != pinned {
                self.panel_face = pinned;
            }
            self.panel_last_reason = None;
            return;
        }

        // No automatic switch while the user is interacting with the panel —
        // scrolling it, or with focus inside it. The pending switch is
        // dropped (the dwell is cleared), not deferred.
        if self.panel_is_interacting() {
            self.clear_panel_dwell();
            self.panel_interacting = false;
            return;
        }
        self.panel_interacting = false;

        // States beat events: while a graph runs, Graph is the resting face
        // and any event dwell waits underneath it. The dwell keeps counting
        // down meanwhile, so a stale event never outlives its 10 seconds.
        let (target, reason) = if graph_running {
            (PanelFace::Graph, Some("graph running".to_string()))
        } else if self.panel_dwell_valid() {
            let reason = self.panel_dwell_reason.clone();
            (
                self.panel_dwell_face.unwrap_or(PanelFace::Knowledge),
                reason,
            )
        } else {
            (PanelFace::Activity, None)
        };

        if self.panel_face != target {
            self.panel_face = target;
            self.panel_last_reason = reason;
        } else if !self.panel_dwell_valid() && !graph_running {
            // Settled on the resting face with nothing driving it: no badge.
            self.panel_last_reason = None;
        }
    }

    /// Seconds left on the current dwell, if any.
    pub(crate) fn panel_dwell_remaining_secs(&self) -> Option<u64> {
        let until = self.panel_dwell_until?;
        let remaining = until.saturating_duration_since(Instant::now());
        if self.panel_dwell_face.is_none() || remaining.is_zero() {
            return None;
        }
        Some(remaining.as_secs().max(1))
    }

    /// Badge explaining why the current face is showing, so a face never
    /// appears unexplained. `None` on the default resting face.
    pub(crate) fn panel_face_badge(&self) -> Option<String> {
        if self.panel_pinned.is_some() {
            return Some("pinned".to_string());
        }
        if let Some(reason) = self.panel_dwell_reason.clone() {
            if self.panel_dwell_valid() {
                if let Some(secs) = self.panel_dwell_remaining_secs() {
                    return Some(format!("{reason} · {secs}s"));
                }
                return Some(reason);
            }
        }
        self.panel_last_reason.clone()
    }

    /// Whether the right panel has anything to show for the current face.
    /// The Activity face keeps its existing visibility rule; Knowledge
    /// needs a selected workdir; Graph needs a graph to look at.
    pub(crate) fn panel_face_visible(&self) -> bool {
        if self.panel_pinned.is_some() {
            return true;
        }
        match self.panel_face {
            PanelFace::Activity => self.activity_panel_state().is_some(),
            PanelFace::Knowledge => {
                self.selected_activity_workdir().is_some() || self.activity_panel_state().is_some()
            }
            PanelFace::Graph => {
                self.graph_live_state.is_some()
                    || !self.graphs.is_empty()
                    || self.activity_panel_state().is_some()
            }
            // Crews is pin-only: it is always visible once pinned, so this
            // arm only exists for exhaustiveness (the pin check above
            // already returned true).
            PanelFace::Crews => true,
        }
    }

    /// Pin a face (`None` = back to automatic) and persist the choice so it
    /// survives restarts. Applies immediately. Production pins go through
    /// the daemon's `config_set` and the config reload (`refresh.rs`); this
    /// direct-apply path is exercised by the persistence tests.
    #[allow(dead_code)]
    pub(crate) fn pin_panel_face(&mut self, pin: Option<PanelFace>) {
        self.panel_pinned = pin;
        if let Some(face) = pin {
            self.clear_panel_dwell();
            self.panel_face = face;
            self.panel_last_reason = None;
            if face == PanelFace::Crews {
                // The Crews face refreshes only while it is the visible
                // face; pinning it is that moment.
                self.request_crews_refresh();
            }
        } else {
            self.tick_panel_face();
        }
        self.save_panel_pinned_face();
    }

    /// CM51 (D5): persist the pin through the daemon's `config_set`, the one
    /// writer of `config.toml` — the TUI never writes the file itself, so a
    /// pin can no longer drop the user's comments or race another writer.
    /// With the daemon down the pin still applies in memory for this session
    /// and the status banner says so. CT31 keeps this path unchanged: the
    /// hub's Settings tab writes through the same `config_set`.
    #[allow(dead_code)]
    fn save_panel_pinned_face(&mut self) {
        let value = self
            .panel_pinned
            .map(|face| face.label().to_string())
            .unwrap_or_else(|| "auto".to_string());
        let port = self
            .db
            .get_state("port")
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::resolve_port(None).to_string());
        let outcome = crate::tui::mcp_client::call_daemon_tool(
            &port,
            "config_set",
            &serde_json::json!({ "key": "pinned_panel_face", "value": value }),
        );
        match outcome {
            Ok(outcome) if !outcome.is_error => {}
            Ok(outcome) => self.set_status_message(true, outcome.text),
            Err(_) => self.set_status_message(true, "pin not saved: daemon not running"),
        }
    }

    /// Show a daemon response on the app's status banner (same shape and TTL
    /// as the graph-control banner).
    #[allow(dead_code)]
    fn set_status_message(&mut self, is_error: bool, text: impl Into<String>) {
        self.graph_action_message = Some(crate::tui::app::dialog::GraphActionMessage {
            is_error,
            text: text.into(),
        });
        self.graph_action_message_at = std::time::Instant::now();
    }

    /// Read the persisted pin back into a face, ignoring unknown values.
    pub(crate) fn load_panel_pinned_face(value: &Option<String>) -> Option<PanelFace> {
        value.as_deref().and_then(PanelFace::from_str)
    }

    /// A mouse-wheel scroll over the panel: marks the interaction and drops
    /// any pending automatic switch.
    pub(crate) fn on_panel_scrolled(&mut self) {
        self.panel_interacting = true;
        self.clear_panel_dwell();
    }

    /// A click inside the panel claims its focus; a click outside releases
    /// it. While focused, automatic switches are dropped, not deferred.
    pub(crate) fn on_panel_clicked(&mut self, inside: bool) {
        self.panel_focused = inside;
        if inside {
            self.clear_panel_dwell();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::intelligence::IntelligenceNodeInput;
    use crate::db::Database;
    use crate::domain::graphs::{Graph, GraphStatus};
    use crate::domain::models::{Agent, Cli};
    use crate::domain::project::Project;
    use crate::tui::app::types::{AgentEntry, Focus};
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn test_app() -> App {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");
        // App::new already ran one tick (seeding baselines); keep the db
        // handle alive via the app itself.
        let _ = &mut app;
        app
    }

    /// Pin every signal timestamp the panel reads to a fixed ancient value so
    /// the baseline a tick seeds is deterministically older than any later
    /// write. `tick_panel_face` only fires on a *strictly* newer mark, and
    /// wall clocks are not monotonic: backward NTP/VM steps of hundreds of
    /// ms make "write, tick, write, tick" flaky (PR #51 CI failure), and no
    /// sleep can cover a backward jump. Call this before seeding a baseline,
    /// and again (followed by a tick) before each subsequent write that must
    /// read as newer.
    fn pin_panel_clocks(db: &Database) {
        db.exec_test_sql(
            "UPDATE intelligence_nodes SET content_touched_at = 1 WHERE project_hash = 'panel-project';
             UPDATE graph_specs SET updated_at = 1;",
        )
        .unwrap();
    }

    #[test]
    fn app_new_honors_pinned_panel_face_from_config_parameter() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let config = crate::domain::canopy_config::CanopyConfig {
            pinned_panel_face: Some("knowledge".to_string()),
            ..Default::default()
        };
        let app = App::new(Arc::clone(&db), data_dir.path(), &config).expect("create app");
        assert_eq!(
            app.panel_pinned,
            Some(PanelFace::Knowledge),
            "an explicit config value passed as a parameter must be honoured, independent of ~/.canopy/config.toml"
        );
    }

    fn knowledge_input(id: &str, body: &str) -> IntelligenceNodeInput {
        IntelligenceNodeInput {
            id: Some(id.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some(id.to_string()),
            body: Some(body.to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some("panel-project".to_string())),
            session_id: None,
            relations: None,
        }
    }

    #[test]
    fn knowledge_signal_is_independent_of_display_cap_and_length() {
        let db = test_db();
        for index in 0..51 {
            db.upsert_intelligence_node(knowledge_input(&format!("knowledge-{index}"), "initial"))
                .unwrap();
        }
        pin_panel_clocks(&db);

        let data_dir = tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.projects = vec![Project {
            hash: "panel-project".to_string(),
            path: "/tmp/panel-project".to_string(),
            name: "panel-project".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: Utc::now().timestamp(),
        }];
        app.panel_baselines_init = false;
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        assert_eq!(app.project_knowledge.len(), 50);

        db.upsert_intelligence_node(knowledge_input("knowledge-51", "added"))
            .unwrap();
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason.as_deref(), Some("new knowledge"));

        app.panel_dwell_until = None;
        // Re-anchor the baseline: the add above stamped wall-clock time, so
        // the edit below must not depend on the clock moving forward.
        pin_panel_clocks(&db);
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        db.upsert_intelligence_node(knowledge_input("knowledge-0", "edited"))
            .unwrap();
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason.as_deref(), Some("new knowledge"));
    }

    #[test]
    fn knowledge_delete_does_not_fire_event() {
        let db = test_db();
        for index in 0..5 {
            db.upsert_intelligence_node(knowledge_input(&format!("knowledge-{index}"), "initial"))
                .unwrap();
        }
        let data_dir = tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.projects = vec![Project {
            hash: "panel-project".to_string(),
            path: "/tmp/panel-project".to_string(),
            name: "panel-project".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: Utc::now().timestamp(),
        }];
        app.panel_baselines_init = false;
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        app.clear_panel_dwell();

        db.delete_intelligence_node("knowledge-0").unwrap();
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason, None);
    }

    #[test]
    fn knowledge_add_and_delete_in_same_tick_fires_once() {
        let db = test_db();
        for index in 0..5 {
            db.upsert_intelligence_node(knowledge_input(&format!("knowledge-{index}"), "initial"))
                .unwrap();
        }
        pin_panel_clocks(&db);
        let data_dir = tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.projects = vec![Project {
            hash: "panel-project".to_string(),
            path: "/tmp/panel-project".to_string(),
            name: "panel-project".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: Utc::now().timestamp(),
        }];
        app.panel_baselines_init = false;
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        app.clear_panel_dwell();

        db.upsert_intelligence_node(knowledge_input("knowledge-new", "added"))
            .unwrap();
        db.delete_intelligence_node("knowledge-1").unwrap();
        app.refresh_project_knowledge().unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason.as_deref(), Some("new knowledge"));
    }

    fn standalone_spec(id: &str, workdir: &str) -> crate::domain::graphs::GraphSpec {
        crate::domain::graphs::GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: format!("Backlog {id}"),
            description: Some("stub".to_string()),
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: Some(workdir.to_string()),
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        }
    }

    #[test]
    fn backlog_signal_add_edit_delete() {
        let db = test_db();
        let data_dir = tempdir().unwrap();
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.projects = vec![Project {
            hash: "panel-project".to_string(),
            path: "/tmp/panel-project".to_string(),
            name: "panel-project".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: Utc::now().timestamp(),
        }];
        app.panel_baselines_init = false;
        app.tick_panel_face();
        app.clear_panel_dwell();

        db.insert_graph_spec(&standalone_spec("backlog-1", "/tmp/panel-project"))
            .unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason.as_deref(), Some("backlog changed"));
        app.clear_panel_dwell();

        // Re-anchor the baseline: the add above stamped wall-clock time, so
        // the edit below must not depend on the clock moving forward.
        pin_panel_clocks(&db);
        app.tick_panel_face();
        db.update_spec_tag_details("backlog-1", Some("Renamed"), None, None)
            .unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason.as_deref(), Some("backlog changed"));
        app.clear_panel_dwell();

        db.delete_graph_spec("backlog-1").unwrap();
        app.tick_panel_face();
        assert_eq!(app.panel_dwell_reason, None);
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

    fn make_graph(id: &str, status: GraphStatus) -> Graph {
        Graph {
            id: id.to_string(),
            name: format!("graph {id}"),
            description: None,
            workdir: "/tmp/project".to_string(),
            status,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
        }
    }

    fn expire_dwell(app: &mut App) {
        app.panel_dwell_until = Some(Instant::now() - Duration::from_secs(1));
    }

    #[test]
    fn panel_face_priority_pinned_over_state() {
        let mut app = test_app();
        app.graphs = vec![make_graph("l1", GraphStatus::Running)];
        app.panel_pinned = Some(PanelFace::Knowledge);
        app.panel_face = PanelFace::Knowledge;
        app.tick_panel_face();

        assert_eq!(app.panel_face, PanelFace::Knowledge);
        assert_eq!(app.panel_face_badge().as_deref(), Some("pinned"));
    }

    #[test]
    fn panel_face_priority_state_over_event() {
        let mut app = test_app();
        app.graphs = vec![make_graph("l1", GraphStatus::Running)];
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Graph);

        // A knowledge event fires while the graph runs: the graph (state)
        // keeps the panel; the event dwell is recorded underneath.
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Graph);

        // When the graph ends, the pending event dwell takes the panel.
        app.graphs = vec![make_graph("l1", GraphStatus::Completed)];
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Knowledge);
    }

    #[test]
    fn panel_face_dwell_10s_then_returns_to_resting() {
        let mut app = test_app();
        assert_eq!(app.panel_face, PanelFace::Activity);

        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Knowledge);
        let badge = app.panel_face_badge().expect("event face shows a reason");
        assert!(badge.contains("new knowledge"), "badge was {badge:?}");

        // The dwell lasts `tui.panel_dwell_seconds` (default 10).
        let dwell = crate::domain::settings::default_int("tui.panel_dwell_seconds") as u64;
        let wait = app
            .panel_dwell_until
            .expect("dwell deadline is set")
            .saturating_duration_since(Instant::now());
        assert!(wait <= Duration::from_secs(dwell));
        assert!(wait > Duration::from_secs(dwell - 2));

        expire_dwell(&mut app);
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Activity);
        assert_eq!(app.panel_face_badge(), None);
    }

    /// CM52: `tui.panel_dwell_seconds = 0` disables the dwell — the event
    /// never holds the panel.
    #[test]
    fn panel_dwell_zero_expires_immediately() {
        let mut app = test_app();
        app.tui_settings.panel_dwell_seconds = 0;
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        assert!(!app.panel_dwell_valid(), "dwell 0 must be expired at once");
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Activity);
    }

    #[test]
    fn panel_face_dwell_restart_replaces_and_restarts() {
        let mut app = test_app();
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        let first_until = app.panel_dwell_until.expect("first dwell set");

        std::thread::sleep(Duration::from_millis(5));
        app.fire_panel_event(PanelFace::Knowledge, "backlog changed");

        let second_until = app.panel_dwell_until.expect("second dwell set");
        assert!(
            second_until > first_until,
            "a newer event restarts the dwell"
        );
        assert_eq!(
            app.panel_dwell_reason.as_deref(),
            Some("backlog changed"),
            "a newer event replaces the old one"
        );
        assert!(app.panel_dwell_face == Some(PanelFace::Knowledge));
    }

    #[test]
    fn panel_face_drop_on_interaction_is_not_deferred() {
        let mut app = test_app();
        app.agents = vec![AgentEntry::Agent(sample_agent("bg-1", "/tmp/project"))];
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");

        // The user starts scrolling the panel: the pending switch is
        // dropped, not deferred — the panel stays put…
        app.panel_interacting = true;
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Activity);
        assert!(app.panel_dwell_face.is_none(), "dwell was dropped");

        // …and it does not fire later once the interaction ends.
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Activity);
    }

    #[test]
    fn panel_face_drop_on_focus_inside_panel() {
        let mut app = test_app();
        app.on_panel_clicked(true);
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Activity);
        assert!(app.panel_dwell_face.is_none());

        // Clicking outside releases the focus; a fresh event then applies.
        app.on_panel_clicked(false);
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.tick_panel_face();
        assert_eq!(app.panel_face, PanelFace::Knowledge);
    }

    #[test]
    fn scrolling_the_main_panel_does_not_drop_panel_events() {
        let mut app = test_app();
        app.last_scroll_at = Instant::now();
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.tick_panel_face();

        assert_eq!(app.panel_face, PanelFace::Knowledge);
        assert!(app.panel_dwell_face.is_some());
    }

    #[test]
    fn panel_face_persistence_round_trips_through_config_strings() {
        for face in PanelFace::ALL {
            let stored = face.label().to_string();
            assert_eq!(
                App::load_panel_pinned_face(&Some(stored)),
                Some(face),
                "pinned face survives a config reload"
            );
        }
        assert_eq!(App::load_panel_pinned_face(&None), None);
        assert_eq!(
            App::load_panel_pinned_face(&Some("sideways".to_string())),
            None,
            "unknown values fall back to automatic mode"
        );
    }

    #[test]
    fn panel_face_no_focus_steal() {
        let mut app = test_app();
        app.focus = Focus::Agent;
        app.graphs = vec![make_graph("l1", GraphStatus::Running)];
        app.tick_panel_face();

        assert_eq!(app.panel_face, PanelFace::Graph);
        assert!(matches!(app.focus, Focus::Agent));

        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.graphs = vec![make_graph("l1", GraphStatus::Completed)];
        app.tick_panel_face();
        assert!(matches!(app.focus, Focus::Agent));
    }

    /// Crews is a management view: no combination of resting state, graph
    /// state or knowledge/backlog event may ever select it automatically.
    #[test]
    fn crews_is_never_chosen_automatically() {
        let mut app = test_app();
        assert_ne!(app.panel_face, PanelFace::Crews);

        // (a) no graphs: resting face is Activity.
        app.tick_panel_face();
        assert_ne!(app.panel_face, PanelFace::Crews);

        // (b) a running graph: resting face is Graph.
        app.graphs = vec![make_graph("l1", GraphStatus::Running)];
        app.tick_panel_face();
        assert_ne!(app.panel_face, PanelFace::Crews);

        // (c) a knowledge event: dwell face is Knowledge.
        app.graphs.clear();
        app.fire_panel_event(PanelFace::Knowledge, "new knowledge");
        app.tick_panel_face();
        assert_ne!(app.panel_face, PanelFace::Crews);
    }

    #[test]
    fn crews_from_str() {
        assert_eq!(PanelFace::from_str("crews"), Some(PanelFace::Crews));
        assert_eq!(PanelFace::Crews.label(), "crews");
    }

    /// CT31 requirement 5: the pin must go through the daemon's `config_set`,
    /// never a direct `CanopyConfig::save` — the TUI must not write
    /// `config.toml` itself.
    #[test]
    fn save_panel_pinned_face_calls_config_set() {
        use crate::application::ports::StateRepository;
        use crate::tui::mcp_client::test_support::spawn_fake_daemon;

        let fake = spawn_fake_daemon(serde_json::json!({
            "content": [{ "type": "text", "text": "{}" }],
            "isError": false
        }));
        let mut app = test_app();
        app.db.set_state("port", &fake.port).unwrap();

        app.pin_panel_face(Some(PanelFace::Knowledge));

        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1, "exactly one daemon call: {calls:?}");
        assert_eq!(calls[0]["name"], "config_set");
        assert_eq!(
            calls[0]["arguments"],
            serde_json::json!({ "key": "pinned_panel_face", "value": "knowledge" })
        );
    }
}
