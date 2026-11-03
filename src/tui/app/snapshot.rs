//! Background snapshot for the once-per-second refresh: every database read
//! `App::refresh_slow` used to do on the UI thread now runs here, on a worker
//! thread that owns its own [`Database`] connection, and hands an immutable
//! [`SlowSnapshot`] (plain owned data, no `rusqlite` types) to the UI over a
//! channel. The UI tick only swaps the latest snapshot in.
//!
//! Follows the playground-search worker-plus-poll pattern (B23): the worker
//! never blocks the UI — [`SnapshotWorker::kick`] sends the current UI
//! selections (project, graph, agent, workdirs, limits) when no build is in
//! flight, and [`SnapshotWorker::poll`] non-blockingly drains the latest
//! finished snapshot.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Instant;

use crate::application::ports::StateRepository;
use crate::db::project::{ProjectHistoryEntry, RagInfoSummary, RagPerFileStatus, RagQueueItem};
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphDetails, GraphNodeRun, GraphSpec};
use crate::domain::models::{Agent, CorruptAgent, RunLog};
use crate::domain::project::Project;
use crate::tui::app::graph_live_state::GraphLiveState;
use crate::tui::app::types::{
    AgentEntry, App, GraphLiveFocus, GraphSidebarMeta, ProjectPreviewSummary, ProjectTab,
};
use crate::tui::gamification::MissionEvent;

/// Everything the slow tick needs from the database, built off the UI thread.
#[derive(Default)]
pub(crate) struct SlowSnapshot {
    pub daemon_version: String,
    pub daemon_port: u16,
    pub settings_revision: String,
    pub agents: Vec<Agent>,
    pub corrupt_agents: Vec<CorruptAgent>,
    pub active_runs: HashMap<String, RunLog>,
    pub recent_runs: Vec<RunLog>,
    pub finished_runs: Vec<FinishedRunInfo>,
    pub projects: Vec<Project>,
    pub project_last_activity: HashMap<String, i64>,
    pub project_knowledge: Vec<crate::db::intelligence::IntelligenceNodeRecord>,
    pub backlog_specs: Vec<GraphSpec>,
    pub project_preview_cache: HashMap<String, ProjectPreviewSummary>,
    pub project_history: Vec<ProjectHistoryEntry>,
    pub project_history_hash: Option<String>,
    pub graphs: Vec<Graph>,
    pub archived_graphs: Vec<Graph>,
    pub archived_graph_count: usize,
    pub graph_sidebar_meta: HashMap<String, GraphSidebarMeta>,
    pub graph_details: Option<GraphDetails>,
    pub graph_runs: Vec<GraphNodeRun>,
    pub graph_live_state: Option<GraphLiveState>,
    pub graph_whim: HashMap<String, GraphWhimInfo>,
    pub project_graph_edges: Vec<crate::tui::app::types::ProjectGraphEdge>,
    pub project_graph_trees: Vec<Vec<String>>,
    pub rag_queue: Vec<RagQueueItem>,
    pub rag_info: RagInfoSummary,
    pub rag_paused: bool,
    pub rag_model_loaded: bool,
    pub rag_acquisition_state: Option<crate::rag::status::AcquisitionState>,
    pub rag_file_status: Vec<RagPerFileStatus>,
    pub panel_knowledge_updated: Option<i64>,
    pub panel_backlog_updated: Option<i64>,
    pub mission: MissionDbCounters,
    pub mission_events: Vec<MissionEvent>,
    pub confirmed_unlocks: Vec<(crate::domain::gamification::MissionId, i64)>,
    pub activity_digest: u64,
    pub overview_sessions_digest: u64,
    pub hub_counts_digest: (i64, i64, i64, i64),
    pub pending_sends_digest: u64,
}

/// A finished background run for the daemonless-TUI completion toast.
#[derive(Debug, Clone)]
pub(crate) struct FinishedRunInfo {
    pub agent_id: String,
    pub success: bool,
    pub exit_code: Option<i32>,
}

/// Per-graph header inputs: the node the graph last ran plus its completed
/// spec labels. The worker fills each only under the same conditions the UI
/// used to query them (blocked/failed for the node, running/pausing for the
/// labels).
#[derive(Debug, Clone, Default)]
pub(crate) struct GraphWhimInfo {
    pub node: Option<String>,
    pub done_specs: Vec<String>,
}

/// Database-side mission inputs. Memory-side inputs (`cli_usage`,
/// `system_info`, project languages, live agent state) stay on the UI, which
/// merges them into the full mission snapshot.
#[derive(Debug, Default)]
pub(crate) struct MissionDbCounters {
    pub gardener_edits: u64,
    pub graph_node_count_max: usize,
    pub completed_graph_runs: i64,
    pub total_graph_node_runs: i64,
    pub has_parallel_graph: bool,
    pub max_seed_session_bindings: i64,
    pub intelligence_nodes: i64,
    pub cross_project_links: i64,
    pub max_cpu_frequency_seen: Option<u64>,
    pub canopy_uptime_secs: u64,
}

/// What the worker needs from the UI to build one snapshot.
#[derive(Debug, Default)]
pub(crate) struct SnapshotRequest {
    pub selected_project_hash: Option<String>,
    pub history_hash: Option<String>,
    pub selected_graph_id: Option<String>,
    pub selected_spec_id: Option<String>,
    pub graph_view_archived: bool,
    pub backlog_workdir: Option<String>,
    pub activity_workdirs: Vec<String>,
    pub activity_limit: usize,
    pub overview_project_hash: Option<String>,
    pub overview_project_path: Option<String>,
    pub pending_sends_session: Option<String>,
    pub has_parallel_seen: bool,
    pub prev_active_ids: HashSet<String>,
    pub persist_unlocks: Vec<crate::domain::gamification::MissionId>,
    pub rag_embeddings_model: String,
    pub cpu_frequency_mhz: Option<u64>,
}

/// The background worker: owns a [`Database`] on its thread, builds one
/// [`SlowSnapshot`] per request, and hands the latest to the UI.
pub(crate) struct SnapshotWorker {
    req_tx: mpsc::Sender<SnapshotRequest>,
    snap_rx: mpsc::Receiver<SlowSnapshot>,
    in_flight: bool,
}

impl SnapshotWorker {
    /// Spawn the worker thread with its own database connection.
    pub(crate) fn spawn(db_path: &Path, data_dir: &Path) -> Self {
        let (req_tx, req_rx) = mpsc::channel::<SnapshotRequest>();
        let (snap_tx, snap_rx) = mpsc::channel::<SlowSnapshot>();
        let db_path = db_path.to_path_buf();
        let data_dir = data_dir.to_path_buf();
        std::thread::spawn(move || {
            worker_loop(&db_path, &data_dir, req_rx, &snap_tx);
        });
        Self {
            req_tx,
            snap_rx,
            in_flight: false,
        }
    }

    /// Test-only worker fed by the test: the test sends canned snapshots
    /// into the returned sender; the worker never touches a database.
    #[cfg(test)]
    pub(crate) fn fake() -> (Self, mpsc::Sender<SlowSnapshot>) {
        let (req_tx, req_rx) = mpsc::channel::<SnapshotRequest>();
        let (snap_tx, snap_rx) = mpsc::channel::<SlowSnapshot>();
        std::mem::forget(req_rx);
        (
            Self {
                req_tx,
                snap_rx,
                in_flight: false,
            },
            snap_tx,
        )
    }

    /// Send a build request when no build is in flight. Drops the request
    /// when the worker is gone (the next poll retires the worker).
    pub(crate) fn kick(&mut self, request: SnapshotRequest) {
        if self.in_flight {
            return;
        }
        if self.req_tx.send(request).is_ok() {
            self.in_flight = true;
        }
    }

    /// Non-blocking drain: the latest finished snapshot, if any. True when
    /// the worker is gone and the UI should fall back to synchronous reads.
    pub(crate) fn poll(&mut self) -> (Option<SlowSnapshot>, bool) {
        let mut latest = None;
        loop {
            match self.snap_rx.try_recv() {
                Ok(snapshot) => {
                    latest = Some(snapshot);
                    self.in_flight = false;
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return (latest, true),
            }
        }
        (latest, false)
    }
}

/// Worker thread body: open one connection, then build one snapshot per
/// request until the UI goes away.
fn worker_loop(
    db_path: &PathBuf,
    data_dir: &Path,
    req_rx: mpsc::Receiver<SnapshotRequest>,
    snap_tx: &mpsc::Sender<SlowSnapshot>,
) {
    let db = match Database::new_safe(db_path, data_dir) {
        Ok(db) => db,
        Err(error) => {
            tracing::warn!("Snapshot worker could not open the database: {error:#}");
            return;
        }
    };
    let mut state = WorkerState::default();
    for request in req_rx {
        let snapshot = super::snapshot_build::build_snapshot(&db, &mut state, &request);
        state.has_parallel_seen = snapshot.mission.has_parallel_graph;
        if snap_tx.send(snapshot).is_err() {
            return;
        }
    }
}

/// Worker-side sticky state: the monotonic mission inputs that used to live
/// on `App` (uptime anchor, max CPU).
#[derive(Default)]
pub(crate) struct WorkerState {
    pub has_parallel_seen: bool,
    pub uptime_anchor: Option<Instant>,
    pub max_cpu_frequency_seen: Option<u64>,
}

impl WorkerState {
    pub(crate) fn tick_uptime_secs(&mut self, db: &Database) -> u64 {
        let stored: u64 = db
            .get_state("gamification:accumulated_uptime_secs")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let now = Instant::now();
        let Some(anchor) = self.uptime_anchor else {
            self.uptime_anchor = Some(now);
            return stored;
        };
        let pending = now.duration_since(anchor).as_secs();
        if pending >= 30 {
            let total = stored + pending;
            let _ = db.set_state("gamification:accumulated_uptime_secs", &total.to_string());
            self.uptime_anchor = Some(now);
            return total;
        }
        stored + pending
    }

    pub(crate) fn note_cpu_frequency(&mut self, db: &Database, frequency: Option<u64>) {
        let stored: u64 = db
            .get_state("gamification:max_cpu_freq_mhz")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let seen = self.max_cpu_frequency_seen.unwrap_or(0);
        let Some(current) = frequency else {
            let max = stored.max(seen);
            self.max_cpu_frequency_seen = if max > 0 { Some(max) } else { None };
            return;
        };
        let max = stored.max(current).max(seen);
        if max > stored {
            let _ = db.set_state("gamification:max_cpu_freq_mhz", &max.to_string());
        }
        self.max_cpu_frequency_seen = Some(max);
    }
}

// ── UI-side request building and snapshot application ────────────────

impl App {
    /// Build one snapshot request from the current UI selections. No database
    /// call: only in-memory state is read, plus a take of the pending
    /// unlock-persist queue.
    pub(crate) fn build_snapshot_request(&mut self) -> SnapshotRequest {
        let (project_hash, project_path) = self
            .selected_project()
            .map(|p| (p.hash.clone(), p.path.clone()))
            .unzip();
        let pending_sends_session = self.selected_agent().and_then(|entry| match entry {
            AgentEntry::Interactive(idx) => self
                .interactive_agents
                .get(*idx)
                .map(|agent| agent.id.clone()),
            _ => None,
        });
        let mut activity_workdirs = Vec::new();
        if let Some(workdir) = self.selected_activity_workdir() {
            activity_workdirs.push(workdir.to_string());
        }
        if let Some(path) = project_path.as_deref() {
            if !activity_workdirs.iter().any(|w| w == path) {
                activity_workdirs.push(path.to_string());
            }
        }
        SnapshotRequest {
            selected_project_hash: project_hash.clone(),
            history_hash: if self.project_focus == Some(ProjectTab::History) {
                project_hash.clone()
            } else {
                None
            },
            selected_graph_id: self.selected_graph_id.clone(),
            selected_spec_id: self.selected_graph_spec().map(|s| s.spec.id.clone()),
            graph_view_archived: self.graph_view_archived,
            backlog_workdir: project_path.clone(),
            activity_workdirs,
            activity_limit: self.message_window_limit_for_scroll(),
            overview_project_hash: project_hash,
            overview_project_path: project_path,
            pending_sends_session,
            has_parallel_seen: self.has_parallel_graph,
            prev_active_ids: self.prev_active_run_ids.clone(),
            persist_unlocks: std::mem::take(&mut self.mission_pending_persist),
            rag_embeddings_model: self.rag_embeddings_model.clone(),
            cpu_frequency_mhz: self.system_info.cpu_frequency_mhz,
        }
    }

    /// Swap the latest finished snapshot in: every slow-tick output the sync
    /// path reads from the database is copied here instead, with the same
    /// selection-following and clamping the sync refreshes apply. No database
    /// call on the calling thread. Sets `snapshot_dirty` so the fast tick
    /// redraws for the new data.
    pub(crate) fn apply_snapshot(&mut self, snapshot: SlowSnapshot) -> anyhow::Result<()> {
        self.daemon_version = snapshot.daemon_version;
        self.daemon_port = snapshot.daemon_port;
        self.apply_settings_revision(snapshot.settings_revision);
        self.apply_snapshot_agents(snapshot.agents, snapshot.corrupt_agents);
        self.apply_snapshot_runs(
            snapshot.active_runs,
            snapshot.recent_runs,
            &snapshot.finished_runs,
        );
        self.apply_snapshot_projects(
            snapshot.projects,
            snapshot.project_last_activity,
            snapshot.project_knowledge,
            snapshot.backlog_specs,
            snapshot.project_preview_cache,
        );
        self.apply_snapshot_history(snapshot.project_history_hash, snapshot.project_history);
        self.apply_snapshot_graphs(
            snapshot.graphs,
            snapshot.archived_graphs,
            snapshot.archived_graph_count,
            snapshot.graph_sidebar_meta,
        );
        self.apply_snapshot_graph_detail(
            snapshot.graph_details,
            snapshot.graph_runs,
            snapshot.graph_live_state,
        );
        self.project_graph_edges = snapshot.project_graph_edges;
        self.project_graph_trees = snapshot.project_graph_trees;
        self.apply_snapshot_rag(
            snapshot.rag_queue,
            snapshot.rag_info,
            snapshot.rag_paused,
            snapshot.rag_model_loaded,
            snapshot.rag_acquisition_state,
            snapshot.rag_file_status,
        );
        self.graph_whim_cache = snapshot.graph_whim;
        self.activity_digest = snapshot.activity_digest;
        self.overview_sessions_digest = snapshot.overview_sessions_digest;
        self.hub_counts_digest = snapshot.hub_counts_digest;
        self.pending_sends_digest = snapshot.pending_sends_digest;
        for (id, timestamp) in &snapshot.confirmed_unlocks {
            if let Some(title) =
                self.mission_manager
                    .mark_confirmed(*id, *timestamp, &mut self.whimsg)
            {
                crate::domain::notification::send_notification(
                    &title,
                    "Mission unlocked",
                    crate::domain::notification::NotificationLevel::Success,
                );
            }
        }
        self.mission_pending_events.extend(snapshot.mission_events);
        self.tick_missions_from_snapshot(&snapshot.mission)?;
        self.tick_panel_face_from_snapshot(
            snapshot.panel_knowledge_updated,
            snapshot.panel_backlog_updated,
        );
        self.snapshot_dirty = true;
        Ok(())
    }

    /// Rebuild the sidebar agent list from snapshot rows — mirrors
    /// `refresh_agents` without the queries.
    fn apply_snapshot_agents(
        &mut self,
        agents: Vec<crate::domain::models::Agent>,
        corrupt: Vec<crate::domain::models::CorruptAgent>,
    ) {
        self.agents.clear();
        for agent in agents {
            self.agents.push(AgentEntry::Agent(agent));
        }
        for entry in corrupt {
            self.agents.push(AgentEntry::Corrupt(entry));
        }
        for i in 0..self.interactive_agents.len() {
            self.agents.push(AgentEntry::Interactive(i));
        }
        for i in 0..self.terminal_agents.len() {
            self.agents.push(AgentEntry::Terminal(i));
        }
        for i in 0..self.orphaned_sessions.len() {
            self.agents.push(AgentEntry::Orphaned(i));
        }
        for i in 0..self.split_groups.len() {
            self.agents.push(AgentEntry::Group(i));
        }
        let total = self.agents.len();
        if total > 0 && self.selected >= total {
            self.selected = total - 1;
        }
    }

    /// Swap active/recent runs from the snapshot — mirrors
    /// `refresh_active_runs`, with the worker-resolved completions driving the
    /// daemonless-TUI fallback toast instead of per-run queries.
    fn apply_snapshot_runs(
        &mut self,
        active_runs: std::collections::HashMap<String, crate::domain::models::RunLog>,
        recent_runs: Vec<crate::domain::models::RunLog>,
        finished_runs: &[FinishedRunInfo],
    ) {
        self.active_runs = active_runs;
        if self.notifications_enabled && !self.daemon_running {
            for info in finished_runs {
                self.notification_service.notify_task_completed(
                    &info.agent_id,
                    info.success,
                    info.exit_code,
                );
            }
        }
        self.prev_active_run_ids = self.active_runs.keys().cloned().collect();
        self.recent_runs = recent_runs;
    }

    /// Swap projects and their scoped lists — mirrors `refresh_projects`
    /// (hash-following selection, knowledge normalize, backlog clamp) without
    /// the queries.
    fn apply_snapshot_projects(
        &mut self,
        projects: Vec<crate::domain::project::Project>,
        project_last_activity: std::collections::HashMap<String, i64>,
        project_knowledge: Vec<crate::db::intelligence::IntelligenceNodeRecord>,
        backlog_specs: Vec<crate::domain::graphs::GraphSpec>,
        project_preview_cache: std::collections::HashMap<String, ProjectPreviewSummary>,
    ) {
        let previous = self.selected_project;
        let selected_hash = self.selected_project().map(|p| p.hash.clone());
        self.project_last_activity = project_last_activity;
        self.projects = projects;
        self.selected_project = if self.projects.is_empty() {
            0
        } else {
            selected_hash
                .and_then(|hash| self.projects.iter().position(|p| p.hash == hash))
                .unwrap_or_else(|| previous.min(self.projects.len() - 1))
        };
        if self.projects.get(self.selected_project).is_some() {
            self.project_knowledge = project_knowledge;
            self.normalize_selected_knowledge();
        } else {
            self.project_knowledge.clear();
            self.selected_knowledge = 0;
        }
        self.backlog_specs = backlog_specs;
        if self.backlog_specs.is_empty() {
            self.selected_backlog = 0;
        } else {
            self.selected_backlog = self.selected_backlog.min(self.backlog_specs.len() - 1);
        }
        self.project_preview_cache = project_preview_cache;
    }

    /// Swap the live History tab entries the worker built for the focused
    /// project — mirrors the `load_project_history` tail of `refresh_projects`.
    fn apply_snapshot_history(
        &mut self,
        project_history_hash: Option<String>,
        project_history: Vec<crate::db::project::ProjectHistoryEntry>,
    ) {
        if let Some(hash) = project_history_hash {
            self.project_history_cache.insert(hash, project_history);
        }
    }

    /// Swap the graph lists and sidebar meta — mirrors `refresh_graphs`
    /// without the queries.
    fn apply_snapshot_graphs(
        &mut self,
        graphs: Vec<crate::domain::graphs::Graph>,
        archived_graphs: Vec<crate::domain::graphs::Graph>,
        archived_graph_count: usize,
        graph_sidebar_meta: std::collections::HashMap<String, GraphSidebarMeta>,
    ) {
        self.graphs = graphs;
        self.archived_graph_count = archived_graph_count;
        if self.graph_view_archived {
            self.archived_graphs = archived_graphs;
        } else {
            self.archived_graphs.clear();
        }
        self.graph_sidebar_meta = graph_sidebar_meta;
    }

    /// Swap the selected graph's detail — mirrors the selection, default-node
    /// and live-follow bookkeeping of
    /// `refresh_graphs_selection`/`refresh_graph_live_state`, without the
    /// queries.
    fn apply_snapshot_graph_detail(
        &mut self,
        graph_details: Option<crate::domain::graphs::GraphDetails>,
        graph_runs: Vec<crate::domain::graphs::GraphNodeRun>,
        graph_live_state: Option<GraphLiveState>,
    ) {
        let visible_ids: Vec<String> = self
            .visible_graphs()
            .iter()
            .map(|lp| lp.id.clone())
            .collect();
        if visible_ids.is_empty() {
            self.selected_graph_id = None;
            self.graph_details = None;
            self.graph_runs.clear();
            self.reset_graph_live_selection();
            self.graph_live_state = None;
            return;
        }
        let previous_selected = self.selected_graph_id.clone();
        if self
            .selected_graph_id
            .as_ref()
            .is_none_or(|selected| !visible_ids.contains(selected))
        {
            self.selected_graph_id = Some(visible_ids[0].clone());
        }
        let selected_changed = previous_selected != self.selected_graph_id;
        self.graph_details = graph_details;
        if selected_changed {
            self.graph_selected_spec = self.default_graph_spec_index();
            self.reset_graph_live_selection();
        } else {
            self.clamp_graph_selection();
        }
        self.graph_runs = graph_runs;
        self.select_default_graph_node_if_needed(selected_changed);
        self.graph_live_state = graph_live_state;
        self.retire_stale_graph_live_selection();
        self.normalize_automation_kind();
    }

    /// Reset the live-view navigation to auto-follow — the shared tail of a
    /// selected-graph change.
    fn reset_graph_live_selection(&mut self) {
        self.graph_selected_node = 0;
        self.graph_live_follow = true;
        self.graph_live_selected_node = None;
        self.graph_live_follow_anchor = None;
        self.graph_live_focus = GraphLiveFocus::Graph;
        self.graph_spec_strip_selected = None;
        self.graph_spec_strip_scroll = 0;
        self.graph_live_view_scroll = 0;
    }

    /// Drop a manual live selection that the new live state no longer
    /// contains — mirrors the tail of `refresh_graph_live_state`.
    fn retire_stale_graph_live_selection(&mut self) {
        if !self.graph_live_follow {
            let still_present = self.graph_live_state.as_ref().is_some_and(|state| {
                self.graph_live_selected_node
                    .as_deref()
                    .is_some_and(|id| state.effective_nodes.iter().any(|n| n.id == id))
            });
            if !still_present {
                self.graph_live_follow = true;
                self.graph_live_selected_node = None;
                self.graph_live_follow_anchor = None;
            }
        }
        if let Some(selected) = self.graph_spec_strip_selected.as_deref() {
            let still_present = self
                .graph_live_state
                .as_ref()
                .is_some_and(|state| state.spec_queue.iter().any(|e| e.spec_id == selected));
            if !still_present {
                self.graph_spec_strip_selected = None;
            }
        }
    }

    /// Swap RAG state — mirrors `refresh_rag_state` without the queries.
    fn apply_snapshot_rag(
        &mut self,
        rag_queue: Vec<crate::db::project::RagQueueItem>,
        rag_info: crate::db::project::RagInfoSummary,
        rag_paused: bool,
        rag_model_loaded: bool,
        rag_acquisition_state: Option<crate::rag::status::AcquisitionState>,
        rag_file_status: Vec<crate::db::project::RagPerFileStatus>,
    ) {
        self.global_rag_queue = rag_queue;
        self.rag_paused = rag_paused;
        self.rag_model_loaded = rag_model_loaded;
        self.rag_acquisition_state = rag_acquisition_state;
        self.rag_info = rag_info;
        if self.global_rag_queue.is_empty() {
            self.selected_rag_queue = 0;
        } else {
            self.selected_rag_queue = self
                .selected_rag_queue
                .min(self.global_rag_queue.len().saturating_sub(1));
        }
        if !self.rag_info.has_rag_activity() {
            self.agents_rag_focused = false;
        }
        self.rag_file_status = rag_file_status;
    }
}
