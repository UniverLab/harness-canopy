//! Remote graph plates: the `graphs` list, plus `graph_watch` /
//! `graph_unwatch` following one graph's drawing, active node and spec
//! checklist live.
//!
//! Everything here is read-only — no frame in this module changes a graph,
//! a spec or a queue. The first `graph` answer carries exactly the
//! catalogue-plate document [`build_export_document_with_pending`] produces
//! (so the web plate renderer reads it as it reads catalogue documents);
//! after it, a 2-second diffed poll keeps the wire quiet: `graph_state`
//! travels only when the state actually differs from the last one sent on
//! that connection. The polling is deliberate — the graph engine has no
//! event bus into the hub, and this spec adds none.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::daemon::handler_read::{active_graph_spec_context, ActiveGraphSpec};
use crate::daemon::handler_specs_helpers::{build_spec_run_info, SpecRunInfo};
use crate::db::Database;
use crate::domain::graph_transfer::{build_export_document_with_pending, GraphExportDocument};
use crate::domain::graphs::{Graph, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus};
use crate::graph_engine::is_no_spec_placeholder;

use super::protocol::{error_frame, ERR_BUSY, ERR_INTERNAL, ERR_NOT_FOUND};
use super::ws::{OutTx, SessionsState};

/// Poll period of a live watch: the graph engine has no event bus into the
/// hub, so a diffed 2-second poll is the deliberate quiet-wire choice.
const WATCH_POLL: Duration = Duration::from_secs(2);
/// A connection may hold at most four `graph_watch`es; a fifth answers
/// `busy` and starts nothing (FR3).
const MAX_WATCHES: usize = 4;
/// Completed graphs stay on the `graphs` list this long after their last
/// activity.
const COMPLETED_TTL: chrono::Duration = chrono::Duration::hours(24);
/// Hard cap on the `graphs` answer (FR1).
const MAX_SUMMARIES: usize = 50;

/// One row of the `graphs` answer: what the remote sidebar shows for one
/// graph.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSummary {
    pub id: String,
    pub name: String,
    /// The graph status string the DB stores (`running`, `paused`,
    /// `failed`, `completed`, `draft`).
    pub status: String,
    /// The workdir's last path component — never the absolute path.
    pub workdir: String,
    /// The current spec's name, when the graph is on one.
    pub current_spec: Option<String>,
    /// The current node's name, never its id.
    pub current_node: Option<String>,
    /// Completed specs of the graph's queue (or its own specs).
    pub done: i64,
    /// All specs of the graph's queue (or its own specs).
    pub total: i64,
    pub blocked: bool,
    /// RFC3339 `MAX(created_at, started_at, completed_at, last run)`.
    pub updated_at: String,
}

/// The `graph`/`graph_state` payload: the plate's live overlay.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphState {
    pub status: String,
    pub blocked: bool,
    pub blocker: Option<String>,
    pub current_spec_id: Option<String>,
    /// The current node's name, never its id.
    pub current_node: Option<String>,
    /// Node name → `"running"` | `"pass"` | `"fail"` for the current
    /// spec's latest iteration (the last run per node); nodes with no run
    /// are absent.
    pub nodes: BTreeMap<String, String>,
    /// Queue order — or the graph's own spec order when it has no queue.
    pub specs: Vec<GraphSpecEntry>,
    /// RFC3339, same stamp as [`GraphSummary::updated_at`].
    pub updated_at: String,
}

/// One spec row of [`GraphState::specs`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphSpecEntry {
    pub id: String,
    /// The spec's code (e.g. `FEAT-12`), or null for a legacy spec.
    pub code: Option<String>,
    pub name: String,
    pub status: String,
    /// The persisted cross-run attempt count behind the attempt budget.
    pub attempts: i64,
}

/// `graphs{req,graphs}`: the answer to `graphs`.
pub fn graphs_frame(req: &str, graphs: &[GraphSummary]) -> String {
    serde_json::json!({"t": "graphs", "req": req, "graphs": graphs}).to_string()
}

/// `graph{req,id,document,state}`: the first answer to `graph_watch`.
pub fn graph_frame(
    req: &str,
    id: &str,
    document: &GraphExportDocument,
    state: &GraphState,
) -> String {
    serde_json::json!({
        "t": "graph", "req": req, "id": id, "document": document, "state": state,
    })
    .to_string()
}

/// `graph_state{id,state}`: one diffed watch push.
pub fn graph_state_frame(id: &str, state: &GraphState) -> String {
    serde_json::json!({"t": "graph_state", "id": id, "state": state}).to_string()
}

/// `no graph '<id>'` — the `not_found` message for `graph_watch`.
pub(crate) fn no_such_graph(id: &str) -> String {
    format!("no graph '{id}'")
}

/// Why a watch could not start: the id is unknown or archived (the
/// client's fault, answered `not_found`, no watch started), or the read
/// failed (ours, answered `internal`).
enum WatchError {
    NotFound,
    Internal,
}

/// One live `graph_watch` poll task: counted on `SessionsState` the moment
/// the watch registers, dropped with the task whether it is aborted (the
/// connection closed or `graph_unwatch`) or exits on its own — so a test
/// can show that a closed socket ends every watch it held.
pub(crate) struct GraphWatchGuard {
    state: Arc<SessionsState>,
}

impl GraphWatchGuard {
    pub(crate) fn register(state: Arc<SessionsState>) -> Self {
        state.graph_watch_started();
        Self { state }
    }
}

impl Drop for GraphWatchGuard {
    fn drop(&mut self) {
        self.state.graph_watch_ended();
    }
}

/// `graphs`: the remote graph list. The query runs off the executor like
/// every other blocking hub call; the answer goes to `out_tx`.
pub fn handle_graphs(state: &Arc<SessionsState>, out_tx: &OutTx, req: String) {
    let db = Arc::clone(state.hub.db());
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let built = tokio::task::spawn_blocking(move || build_summaries(&db)).await;
        match built {
            Ok(Ok(summaries)) => {
                let _ = out_tx.send(graphs_frame(&req, &summaries));
            }
            _ => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "graphs failed"));
            }
        }
    });
}

/// `graph_watch`: validate the id first — an unknown or archived graph
/// answers `not_found` and starts no watch — then answer `graph` and hand
/// the connection a poll task that pushes `graph_state` only on a real
/// diff. The watch registers before the first frame goes out, so a client
/// that received `graph` knows the watch is counted and owned by this
/// connection.
pub async fn handle_graph_watch(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    watches: &mut HashMap<String, JoinHandle<()>>,
    req: String,
    id: String,
) {
    // A watch whose graph vanished ends on its own, but its handle stays in
    // the map until the connection drops. Count only live watches, or that
    // finished watch would spend a slot the connection no longer holds.
    watches.retain(|_, task| !task.is_finished());
    if watches.len() >= MAX_WATCHES && !watches.contains_key(&id) {
        let _ = out_tx.send(error_frame(Some(&req), ERR_BUSY, "too many graph watches"));
        return;
    }
    let db = Arc::clone(state.hub.db());
    let build_id = id.clone();
    let built = tokio::task::spawn_blocking(move || build_watch(&db, &build_id)).await;
    let (document, initial) = match built {
        Ok(Ok(pair)) => pair,
        Ok(Err(WatchError::NotFound)) => {
            let _ = out_tx.send(error_frame(Some(&req), ERR_NOT_FOUND, &no_such_graph(&id)));
            return;
        }
        Ok(Err(WatchError::Internal)) | Err(_) => {
            let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "graph watch failed"));
            return;
        }
    };
    let guard = GraphWatchGuard::register(Arc::clone(state));
    let _ = out_tx.send(graph_frame(&req, &id, &document, &initial));
    let task = tokio::spawn(watch_loop(
        Arc::clone(state),
        id.clone(),
        out_tx.clone(),
        initial,
        guard,
    ));
    if let Some(previous) = watches.insert(id, task) {
        previous.abort();
    }
}

/// `graph_unwatch`: stop one watch; deliberately no answer frame (FR3).
pub fn handle_graph_unwatch(watches: &mut HashMap<String, JoinHandle<()>>, id: &str) {
    if let Some(task) = watches.remove(id) {
        task.abort();
    }
}

/// One watch's life: re-read the state every [`WATCH_POLL`] and push
/// `graph_state` only when it differs from the last one sent on this
/// connection. Ends when the connection closes (its handle is aborted with
/// the connection) or the graph becomes unreadable.
async fn watch_loop(
    state: Arc<SessionsState>,
    id: String,
    out_tx: OutTx,
    mut last: GraphState,
    _guard: GraphWatchGuard,
) {
    let mut ticker = tokio::time::interval(WATCH_POLL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; the initial `graph` frame already
    // carried this exact state.
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let db = Arc::clone(state.hub.db());
        let read_id = id.clone();
        match tokio::task::spawn_blocking(move || watch_state(&db, &read_id)).await {
            Ok(Ok(next)) => {
                if next != last {
                    let _ = out_tx.send(graph_state_frame(&id, &next));
                    last = next;
                }
            }
            // The graph vanished mid-watch (or the read failed): this
            // watch is over; the connection may start another.
            Ok(Err(_)) | Err(_) => break,
        }
        if out_tx.is_closed() {
            break;
        }
    }
}

/// Build a watch's first answer: the export document the plate renders
/// (the production shape — `build_export_document_with_pending` over the
/// graph's own rows, `platform`/`model` in node configs kept so the plate
/// can colour nodes by harness) plus the live state.
fn build_watch(db: &Database, id: &str) -> Result<(GraphExportDocument, GraphState), WatchError> {
    let graph = db
        .get_graph(id)
        .map_err(|_| WatchError::Internal)?
        .filter(|graph| !graph.archived)
        .ok_or(WatchError::NotFound)?;
    let nodes = db
        .list_graph_nodes_for_graph(id)
        .map_err(|_| WatchError::Internal)?;
    let edges = db
        .list_graph_edges_for_graph(id)
        .map_err(|_| WatchError::Internal)?;
    let ensembles = db
        .list_ensembles_for_graph(id)
        .map_err(|_| WatchError::Internal)?;
    let pending = db
        .pending_entry_sources_for_graph(id)
        .map_err(|_| WatchError::Internal)?;
    let document = build_export_document_with_pending(&graph, &nodes, &edges, &ensembles, &pending)
        .map_err(|_| WatchError::Internal)?;
    let state = build_state(db, &graph).map_err(|_| WatchError::Internal)?;
    Ok((document, state))
}

/// One poll's read: the graph's current state, or an error when the graph
/// is gone (which ends the watch).
fn watch_state(db: &Database, id: &str) -> anyhow::Result<GraphState> {
    let graph = db
        .get_graph(id)?
        .ok_or_else(|| anyhow::anyhow!("{}", no_such_graph(id)))?;
    build_state(db, &graph)
}

/// The `graph_state` payload for one graph right now.
fn build_state(db: &Database, graph: &Graph) -> anyhow::Result<GraphState> {
    let current = current_spec(db, graph)?;
    let live = match &current {
        Some(spec) => current_spec_state(db, spec)?,
        None => CurrentSpecState::default(),
    };
    Ok(GraphState {
        status: graph.status.as_str().to_string(),
        blocked: live.blocked,
        blocker: live.blocker,
        current_spec_id: current.map(|spec| spec.id),
        current_node: live.node,
        nodes: live.nodes,
        specs: spec_entries(db, graph)?,
        updated_at: graph_updated_at(db, graph)?.to_rfc3339(),
    })
}

/// What the current spec contributes to [`GraphState`]: its blocker, the
/// active node's name, and the latest-iteration node map.
#[derive(Default)]
struct CurrentSpecState {
    blocked: bool,
    blocker: Option<String>,
    node: Option<String>,
    nodes: BTreeMap<String, String>,
}

/// Run info for the spec the graph is actually on: node id, blocker and
/// attempts through the shared daemon helper, so this surface agrees with
/// the `graph` tool's `get` action and `graph list`.
fn current_spec_state(db: &Database, spec: &GraphSpec) -> anyhow::Result<CurrentSpecState> {
    let info = run_info(db, spec)?;
    let node = node_name(db, info.current_node.as_deref())?;
    Ok(CurrentSpecState {
        blocked: info.blocker.is_some(),
        blocker: info.blocker,
        node,
        nodes: node_states(db, spec)?,
    })
}

/// `build_spec_run_info` without its MCP error type.
fn run_info(db: &Database, spec: &GraphSpec) -> anyhow::Result<SpecRunInfo> {
    build_spec_run_info(db, spec).map_err(|error| anyhow::anyhow!("spec run info failed: {error}"))
}

/// The spec the graph is actually on — bound or queue member; never the
/// idea placeholder (engine bookkeeping, not work).
fn current_spec(db: &Database, graph: &Graph) -> anyhow::Result<Option<GraphSpec>> {
    Ok(match active_graph_spec_context(db, graph)? {
        ActiveGraphSpec::Bound(spec) | ActiveGraphSpec::QueueMember { spec, .. } => Some(spec),
        ActiveGraphSpec::Idea { .. } | ActiveGraphSpec::None => None,
    })
}

/// The current spec's latest iteration: the last run per node, keyed by
/// node name — `running`/`pass`/`fail`; nodes with no run are absent.
fn node_states(db: &Database, spec: &GraphSpec) -> anyhow::Result<BTreeMap<String, String>> {
    let runs = db.list_graph_runs_for_spec(&spec.id)?;
    let Some(latest) = runs.iter().map(|run| run.iteration).max() else {
        return Ok(BTreeMap::new());
    };
    let mut states = BTreeMap::new();
    // List order is oldest first, so inserting overwrites with the last
    // run of each node in that iteration.
    for run in runs.iter().filter(|run| run.iteration == latest) {
        if let Some(node) = db.get_graph_node(&run.node_id)? {
            states.insert(node.name, run_state_label(run.status).to_string());
        }
    }
    Ok(states)
}

/// The wire label of one node run.
fn run_state_label(status: GraphRunStatus) -> &'static str {
    match status {
        GraphRunStatus::Running | GraphRunStatus::Waiting => "running",
        GraphRunStatus::Pass => "pass",
        _ => "fail",
    }
}

/// [`GraphState::specs`]: queue order (or the graph's own spec order when
/// it has no queue), each with its persisted attempt count.
fn spec_entries(db: &Database, graph: &Graph) -> anyhow::Result<Vec<GraphSpecEntry>> {
    queue_specs(db, graph)?
        .into_iter()
        .map(|spec| {
            Ok(GraphSpecEntry {
                id: spec.id.clone(),
                code: spec.spec_code,
                name: spec.name,
                status: spec.status.as_str().to_string(),
                attempts: db.get_graph_spec_cross_run_attempts(&spec.id)?,
            })
        })
        .collect()
}

/// The specs the plate counts and lists: the graph's queue in queue order,
/// or the graph's own bound specs in position order when it has no queue —
/// the idea placeholder filtered out, never counted as work.
fn queue_specs(db: &Database, graph: &Graph) -> anyhow::Result<Vec<GraphSpec>> {
    if let Some(queue_id) = graph.active_run_queue_id.as_deref() {
        if db.get_queue(queue_id)?.is_some() {
            let mut specs = Vec::new();
            for spec_id in db.list_queue_member_spec_ids(queue_id)? {
                if let Some(spec) = db.get_graph_spec(&spec_id)? {
                    specs.push(spec);
                }
            }
            return Ok(specs);
        }
    }
    Ok(db
        .list_graph_specs(&graph.id)?
        .into_iter()
        .filter(|spec| !is_no_spec_placeholder(spec))
        .collect())
}

/// `MAX(created_at, started_at, completed_at, last run)` — the activity
/// stamp both the list filter and the state diff key on.
fn graph_updated_at(db: &Database, graph: &Graph) -> anyhow::Result<DateTime<Utc>> {
    let mut latest = graph.created_at;
    for stamp in [graph.started_at, graph.completed_at].into_iter().flatten() {
        latest = latest.max(stamp);
    }
    if let Some(run_at) = db.latest_graph_run_at(&graph.id)? {
        latest = latest.max(run_at);
    }
    Ok(latest)
}

/// The `graphs` list: non-archived running/paused/failed graphs plus
/// completed ones active within the last 24 hours; running first, then
/// paused, failed, completed, most recent activity first, at most 50.
pub(crate) fn build_summaries(db: &Database) -> anyhow::Result<Vec<GraphSummary>> {
    let mut ranked: Vec<(u8, DateTime<Utc>, Graph)> = Vec::new();
    for graph in db.list_graphs(None, false)? {
        let Some(group) = status_group(graph.status) else {
            continue;
        };
        let updated = graph_updated_at(db, &graph)?;
        if graph.status == GraphStatus::Completed && updated < Utc::now() - COMPLETED_TTL {
            continue;
        }
        ranked.push((group, updated, graph));
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    ranked.truncate(MAX_SUMMARIES);
    ranked
        .into_iter()
        .map(|(_, updated, graph)| summarize(db, graph, updated))
        .collect()
}

/// The list group of one status: running first, then paused, failed,
/// completed. Anything else (`draft`, `pausing`) is not listable.
fn status_group(status: GraphStatus) -> Option<u8> {
    match status {
        GraphStatus::Running => Some(0),
        GraphStatus::Paused => Some(1),
        GraphStatus::Failed => Some(2),
        GraphStatus::Completed => Some(3),
        GraphStatus::Draft | GraphStatus::Pausing => None,
    }
}

/// One [`GraphSummary`] row: the current spec/node through the shared
/// daemon helpers, the queue progress, the basename-only workdir.
fn summarize(db: &Database, graph: Graph, updated: DateTime<Utc>) -> anyhow::Result<GraphSummary> {
    let current = current_spec(db, &graph)?;
    let (current_spec, current_node, blocked) = match &current {
        Some(spec) => {
            let info = run_info(db, spec)?;
            (
                Some(info.name),
                node_name(db, info.current_node.as_deref())?,
                info.blocker.is_some(),
            )
        }
        None => (None, None, false),
    };
    let specs = queue_specs(db, &graph)?;
    let total = specs.len() as i64;
    let done = specs
        .iter()
        .filter(|spec| spec.status == GraphSpecStatus::Completed)
        .count() as i64;
    Ok(GraphSummary {
        id: graph.id,
        name: graph.name,
        status: graph.status.as_str().to_string(),
        workdir: workdir_basename(&graph.workdir),
        current_spec,
        current_node,
        done,
        total,
        blocked,
        updated_at: updated.to_rfc3339(),
    })
}

/// The workdir's last path component — the remote must never see the
/// host's absolute path (a path with no component, like `/`, reads as
/// empty).
fn workdir_basename(workdir: &str) -> String {
    Path::new(workdir)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The node's display name for `node_id` — never the id on the wire — or
/// `None` when the node row is gone.
fn node_name(db: &Database, node_id: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(node_id) = node_id else {
        return Ok(None);
    };
    Ok(db.get_graph_node(node_id)?.map(|node| node.name))
}
