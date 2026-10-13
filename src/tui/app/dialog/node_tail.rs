//! CT28: live output pane bound to the graph view's highlighted node.
//!
//! A tiling viewer over the `graph_run_output` chunks (and the per-run
//! snapshot tails) the engine streams while a node runs. Unlike the CT3
//! dialog, the pane is resolved anew on every refresh tick from the current
//! highlight — it follows the run node by node in auto-follow, and tracks the
//! user's manual selection otherwise. Opening, closing, or switching the pane
//! never writes to a run row; every read is a `SELECT` (+ one
//! `list_graph_runs_for_spec` per poll to resolve ensemble candidates).

use chrono::{DateTime, Utc};

use crate::domain::graphs::{GraphNodeRun, GraphRunStatus};
use crate::tui::app::types::App;

/// Lines retained per stream in the pane — bounds TUI memory when following a
/// very chatty node (non-functional requirement).
pub(crate) const NODE_TAIL_MAX_LINES: usize = 1000;

/// Live-follow state for the highlighted node. Resolved fresh every poll.
pub(crate) struct NodeTailDialog {
    /// Resolved this poll; `None` = no run of the highlighted node in the
    /// current spec yet (the pane shows a waiting line, not closed).
    pub run_id: Option<String>,
    pub node_name: String,
    pub stdout_lines: Vec<String>,
    pub stderr_lines: Vec<String>,
    /// `None` only when `run_id` is `None`.
    pub status: Option<GraphRunStatus>,
    /// True when the finished run's output JSON carries `"error": "timed out"`.
    pub timed_out: bool,
    pub started_at: Option<DateTime<Utc>>,
    /// `(1-based index, count)` when the resolved run is an ensemble member.
    pub member_pos: Option<(usize, usize)>,
    pub platform: Option<String>,
    pub model: Option<String>,
    /// Lines scrolled up from the live bottom edge (0 = following live).
    pub scroll: usize,
    /// Cursor into the ensemble candidate list (0 = first).
    pub member_cursor: usize,
    /// Cached candidate count so `,`/`.` can wrap without a fresh poll.
    pub member_count: usize,
    /// Set when resolution produced no run; the body shows this spec name.
    pub no_run_spec_name: Option<String>,
    /// The `(run_id, member_cursor)` last poll resolved to — lets the poll
    /// distinguish a genuine node transition (reset scroll) from a member
    /// cursor step (keep scroll).
    pub(crate) last_resolved: Option<(String, usize)>,
}

impl NodeTailDialog {
    pub(crate) fn new() -> Self {
        Self {
            run_id: None,
            node_name: String::new(),
            stdout_lines: Vec::new(),
            stderr_lines: Vec::new(),
            status: None,
            timed_out: false,
            started_at: None,
            member_pos: None,
            platform: None,
            model: None,
            scroll: 0,
            member_cursor: 0,
            member_count: 0,
            no_run_spec_name: None,
            last_resolved: None,
        }
    }
}

impl App {
    /// Open the pane on whatever node is currently highlighted. Always
    /// succeeds — finished/empty nodes open too (CT3 decision 3 superseded).
    pub fn open_node_tail_pane(&mut self) {
        self.node_tail_dialog = Some(NodeTailDialog::new());
        self.poll_node_tail_dialog();
    }

    /// Dismiss the pane. Read-only by construction.
    pub fn close_node_tail_dialog(&mut self) {
        self.node_tail_dialog = None;
    }

    pub fn node_tail_dialog_active(&self) -> bool {
        self.node_tail_dialog.is_some()
    }

    /// Scroll the pane body by `delta` lines (negative = back toward older
    /// lines, positive = toward the live edge). The renderer clamps.
    pub fn node_tail_scroll_by(&mut self, delta: isize) {
        if let Some(dialog) = self.node_tail_dialog.as_mut() {
            if delta >= 0 {
                dialog.scroll = dialog.scroll.saturating_sub(delta as usize);
            } else {
                dialog.scroll = dialog.scroll.saturating_add((-delta) as usize);
            }
        }
    }

    /// Return the pane to the live edge (`End` key).
    pub fn node_tail_scroll_to_live(&mut self) {
        if let Some(dialog) = self.node_tail_dialog.as_mut() {
            dialog.scroll = 0;
        }
    }

    /// Step the ensemble-member cursor by `delta` (wraps within the known
    /// candidate count). Used by `,`/`.`.
    pub fn node_tail_member_step(&mut self, delta: i64) {
        let count = match self.node_tail_dialog.as_ref() {
            Some(d) => d.member_count,
            None => return,
        };
        if count == 0 {
            return;
        }
        if let Some(dialog) = self.node_tail_dialog.as_mut() {
            let next = (dialog.member_cursor as i64 + delta).rem_euclid(count as i64) as usize;
            dialog.member_cursor = next;
        }
    }

    /// Refresh the pane from the DB every tick: resolve the run, reset
    /// scroll/cursor on a genuine transition, then fetch the tail.
    pub(crate) fn poll_node_tail_dialog(&mut self) {
        // CT28: the pane adds reads to the refresh tick ONLY while it is open.
        // Guard before resolving so a closed pane costs no `list_graph_runs_for_spec`.
        if self.node_tail_dialog.is_none() {
            return;
        }
        // Phase 1 (immutable &self): resolve the highlighted node, fetch the
        // full run + tail into owned locals.
        let resolved = self.resolve_for_poll();

        // Phase 2 (&mut self): apply to the dialog.
        let Some(dialog) = self.node_tail_dialog.as_mut() else {
            return;
        };
        match resolved {
            PollData::None => {
                dialog.run_id = None;
                dialog.status = None;
                dialog.stdout_lines.clear();
                dialog.stderr_lines.clear();
                dialog.member_pos = None;
                dialog.no_run_spec_name = None;
            }
            PollData::NoRun {
                node_name,
                spec_name,
            } => {
                dialog.node_name = node_name;
                dialog.run_id = None;
                dialog.status = None;
                dialog.platform = None;
                dialog.model = None;
                dialog.started_at = None;
                dialog.timed_out = false;
                dialog.stdout_lines.clear();
                dialog.stderr_lines.clear();
                dialog.member_pos = None;
                dialog.member_cursor = 0;
                dialog.member_count = 0;
                // The resolved run id changed (to none): the next real run
                // must be treated as a transition and reset the scroll.
                dialog.scroll = 0;
                dialog.last_resolved = None;
                dialog.no_run_spec_name = Some(spec_name);
            }
            PollData::Show(s) => {
                apply_resolved_run(dialog, &s);
            }
        }
    }

    /// Resolve the highlighted node to display data (immutable borrow only).
    fn resolve_for_poll(&self) -> PollData {
        let Some(highlighted) = self.graph_live_highlighted_node_id() else {
            return PollData::None;
        };
        let Some(state) = self.graph_live_state.as_ref() else {
            return PollData::None;
        };
        let Some(spec_id) = state.current_spec_id.as_deref() else {
            return PollData::None;
        };
        let node_name_of = |node_id: &str| -> String {
            state
                .effective_nodes
                .iter()
                .find(|n| n.id == node_id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| node_id.to_string())
        };
        let spec_name_of = || -> String {
            state
                .spec_queue
                .iter()
                .find(|q| Some(q.spec_id.as_str()) == Some(spec_id))
                .map(|q| q.spec_name.clone())
                .unwrap_or_else(|| spec_id.to_string())
        };

        let ensemble = state.ensembles.iter().find(|e| {
            e.join_node_id == highlighted || e.members.iter().any(|m| m.node_id == highlighted)
        });

        match ensemble {
            Some(ens) => {
                let member_ids: Vec<&str> =
                    ens.members.iter().map(|m| m.node_id.as_str()).collect();
                let mut runs = self
                    .db
                    .list_graph_runs_for_spec(spec_id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|r| member_ids.contains(&r.node_id.as_str()))
                    .collect::<Vec<GraphNodeRun>>();
                runs.sort_by(|a, b| match (a.status, b.status) {
                    (GraphRunStatus::Running, GraphRunStatus::Running) => {
                        a.started_at.cmp(&b.started_at)
                    }
                    (GraphRunStatus::Running, _) => std::cmp::Ordering::Less,
                    (_, GraphRunStatus::Running) => std::cmp::Ordering::Greater,
                    _ => b.started_at.cmp(&a.started_at),
                });
                if runs.is_empty() {
                    // The ensemble box (join or member) with no member runs in
                    // this spec still names the spec in the waiting line.
                    return PollData::NoRun {
                        node_name: node_name_of(highlighted),
                        spec_name: spec_name_of(),
                    };
                }
                let cursor = self
                    .node_tail_dialog
                    .as_ref()
                    .map(|d| d.member_cursor.min(runs.len().saturating_sub(1)))
                    .unwrap_or(0);
                let run = &runs[cursor];
                // `runs` is the candidate list, which can be longer than the
                // member list: a retried member contributes one run per
                // attempt (node_attempt inserts a fresh row), and `,`/`.`
                // must be able to reach every candidate.
                self.make_show(
                    run,
                    node_name_of(&run.node_id),
                    Some((cursor + 1, runs.len())),
                )
            }
            None => {
                let runs = self
                    .db
                    .list_graph_runs_for_spec(spec_id)
                    .unwrap_or_default();
                match runs.iter().rfind(|r| r.node_id == highlighted) {
                    Some(run) => self.make_show(run, node_name_of(highlighted), None),
                    None => PollData::NoRun {
                        node_name: node_name_of(highlighted),
                        spec_name: spec_name_of(),
                    },
                }
            }
        }
    }

    /// Build a [`PollData::Show`] from a resolved run (fetches tail + status).
    fn make_show(
        &self,
        run: &GraphNodeRun,
        node_name: String,
        member_pos: Option<(usize, usize)>,
    ) -> PollData {
        let stdout_lines = self
            .db
            .get_graph_run_tail(&run.id, NODE_TAIL_MAX_LINES)
            .ok()
            .map(|(out, err)| {
                (
                    out.lines().map(str::to_string).collect(),
                    err.lines().map(str::to_string).collect(),
                )
            });

        let (stdout_lines, stderr_lines) = stdout_lines.unwrap_or_default();
        let latest = self.db.get_graph_run(&run.id).ok().flatten();
        PollData::Show(ShowData {
            run_id: run.id.clone(),
            executed_platform: run.executed_platform.clone(),
            executed_model: run.executed_model.clone(),
            stdout_lines,
            stderr_lines,
            status: latest.as_ref().map(|r| r.status),
            started_at: latest.as_ref().map(|r| r.started_at),
            timed_out: latest.as_ref().is_some_and(|r| {
                r.output
                    .as_ref()
                    .and_then(|v| v.get("error"))
                    .and_then(|v| v.as_str())
                    == Some("timed out")
            }),
            node_name,
            member_pos,
        })
    }
}

/// Fully-resolved display data for a run (owned; produced in the immutable
/// phase, applied to the dialog in the mutable phase).
struct ShowData {
    run_id: String,
    executed_platform: Option<String>,
    executed_model: Option<String>,
    stdout_lines: Vec<String>,
    stderr_lines: Vec<String>,
    status: Option<GraphRunStatus>,
    started_at: Option<DateTime<Utc>>,
    timed_out: bool,
    node_name: String,
    member_pos: Option<(usize, usize)>,
}

enum PollData {
    None,
    NoRun {
        node_name: String,
        spec_name: String,
    },
    Show(ShowData),
}

/// Apply resolved display data to the dialog: detect transitions, fill fields.
fn apply_resolved_run(dialog: &mut NodeTailDialog, data: &ShowData) {
    let new_id = data.run_id.clone();
    let new_cursor = dialog.member_cursor;
    let transitioned = match &dialog.last_resolved {
        Some((id, cursor)) if *cursor == new_cursor && *id != new_id => true,
        None => true,
        _ => false,
    };
    if transitioned {
        dialog.scroll = 0;
        dialog.member_cursor = 0;
    }
    dialog.last_resolved = Some((new_id.clone(), dialog.member_cursor));

    dialog.run_id = Some(new_id);
    dialog.platform = data.executed_platform.clone();
    dialog.model = data.executed_model.clone();
    dialog.stdout_lines = data.stdout_lines.clone();
    dialog.stderr_lines = data.stderr_lines.clone();
    dialog.status = data.status;
    dialog.started_at = data.started_at;
    dialog.timed_out = data.timed_out;
    dialog.node_name = data.node_name.clone();
    dialog.member_pos = data.member_pos;
    if let Some((_, count)) = data.member_pos {
        dialog.member_count = count;
    } else {
        dialog.member_cursor = 0;
        dialog.member_count = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus,
        GraphStatus,
    };
    use chrono::Utc;
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Arc<Database> {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn seed_running_node(db: &Database) {
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp-tail".to_string(),
            name: "Graph lp-tail".to_string(),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "spec-tail".to_string(),
            graph_id: Some("lp-tail".to_string()),
            name: "Spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "node-tail".to_string(),
            spec_id: Some("spec-tail".to_string()),
            graph_id: None,
            name: "Tail node".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 0,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_run(&GraphNodeRun {
            id: "run-tail".to_string(),
            graph_id: "lp-tail".to_string(),
            spec_id: "spec-tail".to_string(),
            node_id: "node-tail".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    fn test_app(db: Arc<Database>) -> App {
        let data_dir = tempdir().expect("create data dir");
        let dir_path = data_dir.path().to_path_buf();
        std::mem::forget(data_dir);
        App::new(
            db,
            &dir_path,
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app")
    }

    #[test]
    fn open_pane_always_succeeds() {
        let db = test_db();
        seed_running_node(&db);
        let mut app = test_app(Arc::clone(&db));

        app.open_node_tail_pane();
        assert!(app.node_tail_dialog_active());
        let dialog = app.node_tail_dialog.as_ref().unwrap();
        assert_eq!(dialog.node_name, "Tail node");
    }

    #[test]
    fn close_does_not_affect_run() {
        let db = test_db();
        seed_running_node(&db);
        let mut app = test_app(Arc::clone(&db));

        app.open_node_tail_pane();
        app.close_node_tail_dialog();
        assert!(!app.node_tail_dialog_active());
        let run = db.get_graph_run("run-tail").unwrap().unwrap();
        assert_eq!(run.status, GraphRunStatus::Running);
    }

    #[test]
    fn scroll_by_moves_away_from_and_back_to_live() {
        let db = test_db();
        seed_running_node(&db);
        let mut app = test_app(Arc::clone(&db));

        app.open_node_tail_pane();
        app.node_tail_scroll_by(-3);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().scroll, 3);
        app.node_tail_scroll_by(3);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().scroll, 0);
        app.node_tail_scroll_to_live();
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().scroll, 0);
        app.close_node_tail_dialog();
        app.node_tail_scroll_by(-3);
    }

    #[test]
    fn member_step_wraps_within_count() {
        let db = test_db();
        let mut app = test_app(Arc::clone(&db));
        let mut dialog = NodeTailDialog::new();
        dialog.member_count = 3;
        app.node_tail_dialog = Some(dialog);
        app.node_tail_member_step(1);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().member_cursor, 1);
        app.node_tail_member_step(1);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().member_cursor, 2);
        app.node_tail_member_step(1);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().member_cursor, 0);
        app.node_tail_member_step(-1);
        assert_eq!(app.node_tail_dialog.as_ref().unwrap().member_cursor, 2);
    }

    /// Seed a Running graph `lp-pane` with a Running spec `spec-pane` and the
    /// given Agent nodes.
    fn seed_pane_graph(db: &Database, spec_id: &str, node_ids: &[&str]) {
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "lp-pane".to_string(),
            name: "Pane graph".to_string(),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: spec_id.to_string(),
            graph_id: Some("lp-pane".to_string()),
            name: "Pane spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
        for (i, id) in node_ids.iter().enumerate() {
            db.insert_graph_node(&GraphNode {
                id: id.to_string(),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: format!("Node {id}"),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: i as i64,
                created_at: Utc::now(),
            })
            .unwrap();
        }
    }

    fn insert_pane_run(
        db: &Database,
        spec_id: &str,
        run_id: &str,
        node_id: &str,
        status: GraphRunStatus,
        started_at: DateTime<Utc>,
    ) {
        db.insert_graph_run(&GraphNodeRun {
            id: run_id.to_string(),
            graph_id: "lp-pane".to_string(),
            spec_id: spec_id.to_string(),
            node_id: node_id.to_string(),
            status,
            input: None,
            output: None,
            started_at,
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    fn pane_state(
        spec_id: &str,
        highlighted: &str,
    ) -> crate::tui::app::graph_live_state::GraphLiveState {
        crate::tui::app::graph_live_state::GraphLiveState {
            graph_id: "lp-pane".to_string(),
            graph_name: "Pane graph".to_string(),
            graph_status: GraphStatus::Running,
            workdir: "/tmp/test".to_string(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            spec_queue: vec![crate::tui::app::graph_live_state::SpecQueueEntry {
                spec_id: spec_id.to_string(),
                spec_name: "Pane spec".to_string(),
                status: GraphSpecStatus::Running,
                failure_reason: None,
            }],
            done_count: 0,
            total_count: 1,
            current_spec_id: Some(spec_id.to_string()),
            effective_nodes: Vec::new(),
            effective_edges: Vec::new(),
            ensembles: Vec::new(),
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: Some(highlighted.to_string()),
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        }
    }

    fn member(node_id: &str) -> crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
        crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
            node_id: node_id.to_string(),
            label: node_id.to_string(),
            status: None,
        }
    }

    #[test]
    fn resolution_picks_latest_run_for_plain_node() {
        let db = test_db();
        seed_pane_graph(&db, "spec-pane", &["n1"]);
        let base = Utc::now();
        insert_pane_run(
            &db,
            "spec-pane",
            "run-old",
            "n1",
            GraphRunStatus::Pass,
            base,
        );
        insert_pane_run(
            &db,
            "spec-pane",
            "run-new",
            "n1",
            GraphRunStatus::Running,
            base + chrono::Duration::seconds(5),
        );

        let mut app = test_app(Arc::clone(&db));
        app.graph_live_state = Some(pane_state("spec-pane", "n1"));
        app.graph_live_follow = true;

        app.open_node_tail_pane();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-new"),
            "the pane must resolve the latest run of a plain node"
        );
    }

    #[test]
    fn ensemble_candidates_order_running_first_then_finished_newest() {
        let db = test_db();
        seed_pane_graph(&db, "spec-pane", &["m1", "m2", "m3", "j1"]);
        let base = Utc::now();
        // One running member, two finished: the newest finished must follow
        // the running one, the older after that.
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m1",
            "m1",
            GraphRunStatus::Running,
            base,
        );
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m2",
            "m2",
            GraphRunStatus::Pass,
            base + chrono::Duration::seconds(9),
        );
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m3",
            "m3",
            GraphRunStatus::Pass,
            base + chrono::Duration::seconds(2),
        );

        let mut app = test_app(Arc::clone(&db));
        let mut state = pane_state("spec-pane", "j1");
        state.ensembles = vec![crate::tui::app::graph_live_state::EnsembleLiveInfo {
            ensemble_id: "e1".to_string(),
            name: "Ensemble".to_string(),
            join_node_id: "j1".to_string(),
            members: vec![member("m1"), member("m2"), member("m3")],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }];
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;

        app.open_node_tail_pane();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m1"),
            "candidate 0 is the running member"
        );
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().member_pos,
            Some((1, 3))
        );

        app.node_tail_member_step(1);
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m2"),
            "candidate 1 is the newest finished member"
        );

        app.node_tail_member_step(1);
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m3"),
            "candidate 2 is the older finished member"
        );

        app.node_tail_member_step(1);
        app.poll_node_tail_dialog();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m1"),
            "the member cursor wraps back to the running member"
        );
    }

    /// CT28: a retried member contributes one run per attempt, so the
    /// candidate list can be longer than the member list — `,`/`.` must wrap
    /// within the *candidates*, not the member count, or the older attempts
    /// are unreachable.
    #[test]
    fn member_step_wraps_within_candidates_not_members() {
        let db = test_db();
        seed_pane_graph(&db, "spec-pane", &["m1", "m2", "m3", "j1"]);
        let base = Utc::now();
        // m1 ran once (finished, oldest), then was retried and is running now.
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m1-old",
            "m1",
            GraphRunStatus::Fail,
            base,
        );
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m1-new",
            "m1",
            GraphRunStatus::Running,
            base + chrono::Duration::seconds(5),
        );
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m2",
            "m2",
            GraphRunStatus::Pass,
            base + chrono::Duration::seconds(2),
        );
        insert_pane_run(
            &db,
            "spec-pane",
            "run-m3",
            "m3",
            GraphRunStatus::Pass,
            base + chrono::Duration::seconds(1),
        );

        let mut app = test_app(Arc::clone(&db));
        let mut state = pane_state("spec-pane", "j1");
        state.ensembles = vec![crate::tui::app::graph_live_state::EnsembleLiveInfo {
            ensemble_id: "e1".to_string(),
            name: "Ensemble".to_string(),
            join_node_id: "j1".to_string(),
            members: vec![member("m1"), member("m2"), member("m3")],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }];
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;

        app.open_node_tail_pane();
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
            Some("run-m1-new"),
            "candidate 0 is the running retry"
        );
        assert_eq!(
            app.node_tail_dialog.as_ref().unwrap().member_pos,
            Some((1, 4)),
            "the count is the candidate count (4 runs), not the member count (3)"
        );

        // Step through every candidate: m1-new → m2 → m3 → m1-old → wrap.
        for expected in ["run-m2", "run-m3", "run-m1-old", "run-m1-new"] {
            app.node_tail_member_step(1);
            app.poll_node_tail_dialog();
            assert_eq!(
                app.node_tail_dialog.as_ref().unwrap().run_id.as_deref(),
                Some(expected),
                "`.` must reach every candidate, got {}",
                app.node_tail_dialog
                    .as_ref()
                    .unwrap()
                    .run_id
                    .as_deref()
                    .unwrap()
            );
        }
    }

    /// CT28 req 3: a highlighted ensemble node (join or member) with no member
    /// run in the current spec still names the spec in the waiting line.
    #[test]
    fn ensemble_without_runs_names_the_current_spec() {
        let db = test_db();
        seed_pane_graph(&db, "spec-pane", &["m1", "m2", "j1"]);
        let mut app = test_app(Arc::clone(&db));
        let mut state = pane_state("spec-pane", "j1");
        state.ensembles = vec![crate::tui::app::graph_live_state::EnsembleLiveInfo {
            ensemble_id: "e1".to_string(),
            name: "Ensemble".to_string(),
            join_node_id: "j1".to_string(),
            members: vec![member("m1"), member("m2")],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }];
        app.graph_live_state = Some(state);
        app.graph_live_follow = true;

        // The ensemble box (join) highlighted.
        app.open_node_tail_pane();
        let dialog = app.node_tail_dialog.as_ref().unwrap();
        assert_eq!(dialog.run_id, None);
        assert_eq!(
            dialog.no_run_spec_name.as_deref(),
            Some("Pane spec"),
            "an empty ensemble must name the spec, not fall back to 'this spec'"
        );

        // A member of the same empty ensemble.
        if let Some(state) = app.graph_live_state.as_mut() {
            state.current_node_id = Some("m2".to_string());
        }
        app.poll_node_tail_dialog();
        let dialog = app.node_tail_dialog.as_ref().unwrap();
        assert_eq!(dialog.run_id, None);
        assert_eq!(
            dialog.no_run_spec_name.as_deref(),
            Some("Pane spec"),
            "an empty ensemble member must name the spec too"
        );
    }
}
