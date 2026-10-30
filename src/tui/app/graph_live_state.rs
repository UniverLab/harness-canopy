use std::collections::{HashMap, HashSet};

use super::types::App;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::db::Database;
use crate::domain::graphs::{
    GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphRunStatus, GraphSpecStatus,
    GraphStatus,
};

/// A snapshot of the currently-selected graph's runtime state, assembled
/// fresh on every TUI tick. Renderer-agnostic (no ratatui types).
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct GraphLiveState {
    // ── Graph metadata ───────────────────────────────────────────
    pub graph_id: String,
    pub graph_name: String,
    pub graph_status: GraphStatus,
    pub workdir: String,
    /// `"cron"` / `"watch"` / `"manual"`.
    pub trigger_type: String,
    /// Cron expression when trigger_type is `"cron"`.
    pub schedule_expr: Option<String>,
    /// Watched path when trigger_type is `"watch"`.
    pub watch_path: Option<String>,
    /// Scheduled one-shot resume time, if any.
    pub autorun_at: Option<DateTime<Utc>>,

    // ── Spec queue ──────────────────────────────────────────────
    /// Ordered specs: queue members for a queue run, bound specs otherwise.
    pub spec_queue: Vec<SpecQueueEntry>,
    pub done_count: usize,
    pub total_count: usize,
    /// Id of the spec currently executing (Running) or the next pending one.
    pub current_spec_id: Option<String>,

    // ── Effective graph for the current spec ────────────────────
    /// Spec's own graph if it has nodes, else the top-level graph.
    pub effective_nodes: Vec<GraphNode>,
    pub effective_edges: Vec<GraphEdge>,
    /// Every ensemble (F1) whose join lives in `effective_nodes` — lets the
    /// graph view collapse its N member boxes + join into one "name [N
    /// models]" box with live per-member state, instead of drawing N+1
    /// separate boxes.
    pub ensembles: Vec<EnsembleLiveInfo>,
    /// For every [`GraphNodeKind::Router`] node in `effective_nodes` with a
    /// completed run, the route label it selected — lets the graph view mark
    /// which of a router's N edges a run actually took, not just list them.
    pub router_taken_routes: HashMap<String, String>,

    // ── Current node ────────────────────────────────────────────
    /// Id of the node currently executing, or the most recent completed node.
    pub current_node_id: Option<String>,
    /// Latest run status for the current node.
    pub current_node_status: Option<GraphRunStatus>,
    /// When the current node's latest run started.
    pub current_node_started_at: Option<DateTime<Utc>>,
    /// Iteration counter for the current node's latest run.
    pub current_node_iteration: Option<i64>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct SpecQueueEntry {
    pub spec_id: String,
    pub spec_name: String,
    pub status: GraphSpecStatus,
    /// Why this spec ended up `Failed`/`Skipped`: the admin-recorded reason
    /// if it was administratively transitioned, else the output tail of its
    /// last run. `None` for every other status.
    pub failure_reason: Option<String>,
    /// SPECCODE: engine-generated quick reference (`FIX-7`).
    pub spec_code: Option<String>,
    /// SPECCODE: Conventional Commits type.
    pub spec_type: Option<crate::domain::spec_type::SpecType>,
    /// SPECCODE: optional scope.
    pub spec_scope: Option<String>,
}

/// One ensemble (F1) collapsed for the graph view: its join node id (so the
/// renderer can fold both the members and the join into a single box) plus
/// live per-member state.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct EnsembleLiveInfo {
    pub ensemble_id: String,
    pub name: String,
    pub join_node_id: String,
    /// Members in position order.
    pub members: Vec<EnsembleMemberLiveInfo>,
    /// CM24: the join's straggler/grace config, so the node detail can show
    /// them side by side without another DB lookup.
    pub straggler_timeout_minutes: Option<i64>,
    pub quorum_grace_minutes: Option<i64>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct EnsembleMemberLiveInfo {
    pub node_id: String,
    /// `"display-platform"` or `"display-platform/model"` (display via
    /// `platform_display_name`; the slug when unknown).
    pub label: String,
    pub status: Option<GraphRunStatus>,
}

/// Assemble a [`GraphLiveState`] for the given graph. Returns `None` when
/// `details` is `None` (no graph selected / mid-migration).
pub(crate) fn assemble_graph_live_state(
    db: &Database,
    details: &crate::domain::graphs::GraphDetails,
) -> Option<GraphLiveState> {
    let lp = &details.lp;

    // ── Spec queue ──────────────────────────────────────────────
    let spec_queue = build_spec_queue(db, lp);

    let done_count = spec_queue
        .iter()
        .filter(|e| e.status == GraphSpecStatus::Completed)
        .count();
    let total_count = spec_queue.len();

    // Current spec: first Running, else first Pending/Interrupted (equally
    // runnable — see `Database::queue_next_pending_spec_id`).
    let current_spec_id = spec_queue
        .iter()
        .find(|e| e.status == GraphSpecStatus::Running)
        .or_else(|| {
            spec_queue.iter().find(|e| {
                matches!(
                    e.status,
                    GraphSpecStatus::Pending | GraphSpecStatus::Interrupted
                )
            })
        })
        .map(|e| e.spec_id.clone());

    // ── Effective graph ─────────────────────────────────────────
    let (effective_nodes, effective_edges) =
        resolve_effective_graph(db, details, current_spec_id.as_deref());
    // Product display names for platform slugs, from ~/.canopy/config.toml.
    // Missing home/config/entry degrades to the slug with no warning.
    let display_names: HashMap<String, (Option<String>, Option<String>)> = dirs::home_dir()
        .map(|h| crate::domain::canopy_config::CanopyConfig::load(&h.join(".canopy")))
        .map(|c| {
            c.clis
                .iter()
                .map(|cli| {
                    (
                        cli.name.clone(),
                        (cli.provider.clone(), cli.tool_name.clone()),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let ensembles = resolve_ensembles_live_info(
        db,
        &effective_nodes,
        current_spec_id.as_deref(),
        &display_names,
    );
    let router_taken_routes =
        resolve_router_taken_routes(db, &effective_nodes, current_spec_id.as_deref());

    // ── Current node + output tail ──────────────────────────────
    let (current_node_id, current_node_info) = resolve_current_node(db, current_spec_id.as_deref());

    Some(GraphLiveState {
        graph_id: lp.id.clone(),
        graph_name: lp.name.clone(),
        graph_status: lp.status,
        workdir: lp.workdir.clone(),
        trigger_type: lp.trigger_type_label().to_string(),
        schedule_expr: lp.schedule_expr().map(String::from),
        watch_path: lp.watch_path().map(String::from),
        autorun_at: lp.autorun_at,
        spec_queue,
        done_count,
        total_count,
        current_spec_id,
        effective_nodes,
        effective_edges,
        ensembles,
        router_taken_routes,
        current_node_id,
        current_node_status: current_node_info.status,
        current_node_started_at: current_node_info.started_at,
        current_node_iteration: current_node_info.iteration,
    })
}

/// Resolve every ensemble (F1) whose join node appears in `effective_nodes`,
/// alongside each member's live run status (if a spec is selected).
fn resolve_ensembles_live_info(
    db: &Database,
    effective_nodes: &[GraphNode],
    current_spec_id: Option<&str>,
    display_names: &HashMap<String, (Option<String>, Option<String>)>,
) -> Vec<EnsembleLiveInfo> {
    effective_nodes
        .iter()
        .filter(|node| node.kind == crate::domain::graphs::GraphNodeKind::Join)
        .filter_map(|node| db.get_ensemble_by_join_node(&node.id).ok().flatten())
        .map(|details| {
            let members = details
                .members
                .iter()
                .map(|member| {
                    let (provider, tool_name) = display_names
                        .get(&member.platform)
                        .map(|(p, t)| (p.as_deref(), t.as_deref()))
                        .unwrap_or((None, None));
                    let display = crate::domain::cli_config::platform_display_name(
                        &member.platform,
                        provider,
                        tool_name,
                    );
                    let label = match member.model.as_deref().map(str::trim) {
                        Some(model) if !model.is_empty() => {
                            format!("{display}/{model}")
                        }
                        _ => display,
                    };
                    let status = current_spec_id
                        .map(|spec_id| resolve_node_run_info(db, spec_id, &member.node_id))
                        .and_then(|info| info.status);
                    EnsembleMemberLiveInfo {
                        node_id: member.node_id.clone(),
                        label,
                        status,
                    }
                })
                .collect();
            EnsembleLiveInfo {
                ensemble_id: details.ensemble.id,
                name: details.ensemble.name,
                join_node_id: details.ensemble.join_node_id,
                members,
                straggler_timeout_minutes: details.ensemble.straggler_timeout_minutes,
                quorum_grace_minutes: details.ensemble.quorum_grace_minutes,
            }
        })
        .collect()
}

/// For every [`GraphNodeKind::Router`] node in `effective_nodes`, resolve the
/// route its latest run selected (if it has completed one) — the graph view
/// uses this to mark which of a router's several edges was actually taken,
/// per the live view's "shows which route a completed run took" contract.
/// Routers with no run yet, or whose latest run hasn't recorded a route
/// (still running), are simply absent from the map.
fn resolve_router_taken_routes(
    db: &Database,
    effective_nodes: &[GraphNode],
    current_spec_id: Option<&str>,
) -> HashMap<String, String> {
    let Some(spec_id) = current_spec_id else {
        return HashMap::new();
    };
    effective_nodes
        .iter()
        .filter(|node| node.kind == GraphNodeKind::Router)
        .filter_map(|node| {
            let info = resolve_node_run_info(db, spec_id, &node.id);
            info.chosen_route.map(|route| (node.id.clone(), route))
        })
        .collect()
}

/// Latest-run info for a single node: status, start time, iteration, and a
/// bounded output tail. Shared by the snapshot's auto-detected "current"
/// node and by [`resolve_node_run_info`] for a caller-requested node.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct NodeRunInfo {
    pub status: Option<GraphRunStatus>,
    pub started_at: Option<DateTime<Utc>>,
    pub iteration: Option<i64>,
    /// The route label a router node's run selected, if this run is a
    /// router's (see `graph_engine::execute_router_node`'s `"route"` output
    /// field). `None` for every other node kind.
    pub chosen_route: Option<String>,
    /// What the run waits for, when its status is `waiting` — one entry per
    /// requested mutex with its current holder. Empty otherwise.
    pub waiting_for: Vec<crate::db::mutex_store::MutexWaitDetail>,
}

impl NodeRunInfo {
    fn from_run(db: &Database, run: &crate::domain::graphs::GraphNodeRun) -> Self {
        let waiting_for = if run.status == GraphRunStatus::Waiting {
            db.get_graph_run_waiting_for(&run.id).unwrap_or_default()
        } else {
            Vec::new()
        };
        NodeRunInfo {
            status: Some(run.status),
            started_at: Some(run.started_at),
            iteration: Some(run.iteration),
            chosen_route: extract_chosen_route(&run.output),
            waiting_for,
        }
    }
}

/// Pull the `"route"` field out of a router run's output JSON (see
/// `graph_engine::execute_router_node`'s `NodeExecution::output`) — `None` for
/// any run whose output isn't shaped like a router's (every other node
/// kind).
fn extract_chosen_route(output: &Option<Value>) -> Option<String> {
    output
        .as_ref()?
        .get("route")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Latest run info for `node_id` within `spec_id`: the active (running) run
/// if any, else the most recent run recorded for that node. Unlike
/// [`resolve_current_node`], this takes an explicit node id — used when the
/// caller (e.g. the graph view) has navigated to a node other than the
/// snapshot's auto-detected current one.
#[allow(dead_code)]
pub(crate) fn resolve_node_run_info(db: &Database, spec_id: &str, node_id: &str) -> NodeRunInfo {
    if let Ok(Some(run)) = db.get_active_graph_run_for_node(node_id) {
        if run.spec_id == spec_id {
            return NodeRunInfo::from_run(db, &run);
        }
    }
    db.list_graph_runs_for_spec(spec_id)
        .unwrap_or_default()
        .iter()
        .rev()
        .find(|run| run.node_id == node_id)
        .map(|run| NodeRunInfo::from_run(db, run))
        .unwrap_or_default()
}

// ── Internal helpers ──────────────────────────────────────────────────

/// Build the ordered spec queue from either the active queue's members or
/// the graph's bound specs.
fn build_spec_queue(db: &Database, lp: &crate::domain::graphs::Graph) -> Vec<SpecQueueEntry> {
    let spec_ids = if let Some(ref queue_id) = lp.active_run_queue_id {
        db.list_queue_member_spec_ids(queue_id).unwrap_or_default()
    } else {
        db.list_graph_specs(&lp.id)
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.id)
            .collect()
    };

    spec_ids
        .into_iter()
        .filter_map(|spec_id| {
            let spec = db.get_graph_spec(&spec_id).ok().flatten()?;
            let failure_reason = spec_failure_reason(db, &spec);
            Some(SpecQueueEntry {
                spec_id: spec.id,
                spec_name: spec.name,
                status: spec.status,
                failure_reason,
                spec_code: spec.spec_code,
                spec_type: spec.spec_type,
                spec_scope: spec.spec_scope,
            })
        })
        .collect()
}

/// Completed spec labels for the header's graph events — `[CODE] name`
/// when the spec has a code, the bare name when it does not. Queue members
/// included, since it reads the same source `build_spec_queue` does. Callers
/// bound this to running graphs so the per-tick query stays small.
pub(crate) fn completed_spec_labels(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
) -> Vec<String> {
    build_spec_queue(db, lp)
        .into_iter()
        .filter(|e| e.status == GraphSpecStatus::Completed)
        .map(|e| match e.spec_code {
            Some(code) => format!("[{code}] {}", e.spec_name),
            None => e.spec_name,
        })
        .collect()
}

/// Reason a spec ended up `Failed`/`Skipped`, for the marker strip's detail
/// view: the admin-recorded reason (`completed_via_reason`, set only for
/// administrative transitions) if present, else the last run's self-reported
/// `summary`/`error`/`blocker` — the best available proxy for an
/// engine-driven failure, which doesn't persist a reason on the spec row
/// itself. `None` for every other status, and when neither source has
/// anything.
fn spec_failure_reason(db: &Database, spec: &crate::domain::graphs::GraphSpec) -> Option<String> {
    if !matches!(
        spec.status,
        GraphSpecStatus::Failed | GraphSpecStatus::Skipped
    ) {
        return None;
    }
    if let Some(reason) = spec.completed_via_reason.clone() {
        return Some(reason);
    }
    // CT28: the live footer tail is gone; fall back to the run's own
    // `summary` field (self-reported by the engine) as the best proxy for a
    // failure reason, then the raw error text.
    db.list_graph_runs_for_spec(&spec.id)
        .unwrap_or_default()
        .iter()
        .rev()
        .find_map(|run| {
            run.output.as_ref().and_then(|o| {
                o.get("summary")
                    .or_else(|| o.get("error"))
                    .or_else(|| o.get("blocker"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
        })
}

/// Resolve the effective graph: spec's own graph if it has nodes, else the
/// top-level graph. Same precedence rule as `GraphEngine::run_spec`.
fn resolve_effective_graph(
    db: &Database,
    details: &crate::domain::graphs::GraphDetails,
    current_spec_id: Option<&str>,
) -> (Vec<GraphNode>, Vec<GraphEdge>) {
    if let Some(spec_id) = current_spec_id {
        // Check if the current spec has its own graph via GraphDetails.
        if let Some(spec_detail) = details.specs.iter().find(|s| s.spec.id == spec_id) {
            if !spec_detail.nodes.is_empty() {
                return (spec_detail.nodes.clone(), spec_detail.edges.clone());
            }
        }
        // Fallback: try direct DB lookup (queue specs not in details).
        if let Ok(Some(detail)) = db.get_graph_spec_details(spec_id) {
            if !detail.nodes.is_empty() {
                return (detail.nodes, detail.edges);
            }
        }
    }

    // Fall back to top-level graph.
    (details.graph_nodes.clone(), details.graph_edges.clone())
}

/// Determine the current node: find the latest running run, or the most
/// recent completed run for the current spec. Returns the node id alongside
/// its full [`NodeRunInfo`] (status, timing, output tail) from that same
/// run, so a completed node's output tail isn't lost.
fn resolve_current_node(
    db: &Database,
    current_spec_id: Option<&str>,
) -> (Option<String>, NodeRunInfo) {
    let Some(spec_id) = current_spec_id else {
        return (None, NodeRunInfo::default());
    };

    // Try active (running) run first.
    if let Ok(Some(run)) = db.get_active_graph_run_for_spec(spec_id) {
        let node_id = run.node_id.clone();
        return (Some(node_id), NodeRunInfo::from_run(db, &run));
    }

    // Fall back to most recent run for this spec.
    let runs = db.list_graph_runs_for_spec(spec_id).unwrap_or_default();
    if let Some(run) = runs.last() {
        return (Some(run.node_id.clone()), NodeRunInfo::from_run(db, run));
    }

    (None, NodeRunInfo::default())
}

impl App {
    pub(super) fn edge_priority(
        condition: &crate::domain::graphs::GraphEdgeCondition,
    ) -> (u8, Option<String>) {
        match condition {
            crate::domain::graphs::GraphEdgeCondition::Pass => (0, None),
            crate::domain::graphs::GraphEdgeCondition::Fail => (1, None),
            crate::domain::graphs::GraphEdgeCondition::Always => (2, None),
            crate::domain::graphs::GraphEdgeCondition::Route(label) => (3, Some(label.clone())),
            crate::domain::graphs::GraphEdgeCondition::Error => (4, None),
        }
    }

    /// Collapse join nodes and ensemble members into one key per ensemble
    /// (`true` = ensemble), carrying each key's node position. Mirrors what
    /// the graph view draws.
    pub(super) fn collapse_nodes(state: &GraphLiveState) -> HashMap<String, (bool, i64)> {
        let join_ids: HashSet<&str> = state
            .ensembles
            .iter()
            .map(|e| e.join_node_id.as_str())
            .collect();
        let ensemble_by_member: HashMap<&str, &EnsembleLiveInfo> = state
            .ensembles
            .iter()
            .flat_map(|e| e.members.iter().map(move |m| (m.node_id.as_str(), e)))
            .collect();
        let mut nodes: HashMap<String, (bool, i64)> = HashMap::new(); // true=ensemble
        let mut seen_ens: HashSet<String> = HashSet::new();
        for node in &state.effective_nodes {
            if join_ids.contains(node.id.as_str()) {
                continue;
            }
            let Some(ens) = ensemble_by_member.get(node.id.as_str()) else {
                nodes.insert(node.id.clone(), (false, node.position));
                continue;
            };
            if !seen_ens.insert(ens.ensemble_id.clone()) {
                continue;
            }
            nodes.insert(ens.ensemble_id.clone(), (true, node.position));
        }
        nodes
    }

    /// member-id -> ensemble-id for every member of any ensemble that has a
    /// member in the effective graph (matching `collapse_nodes`'s scope).
    pub(super) fn member_to_ensemble(state: &GraphLiveState) -> HashMap<String, String> {
        let ensemble_by_member: HashMap<&str, &EnsembleLiveInfo> = state
            .ensembles
            .iter()
            .flat_map(|e| e.members.iter().map(move |m| (m.node_id.as_str(), e)))
            .collect();
        let mut member_to_ensemble: HashMap<String, String> = HashMap::new();
        let mut seen_ens: HashSet<String> = HashSet::new();
        for node in &state.effective_nodes {
            let Some(ens) = ensemble_by_member.get(node.id.as_str()) else {
                continue;
            };
            if !seen_ens.insert(ens.ensemble_id.clone()) {
                continue;
            }
            member_to_ensemble.extend(
                ens.members
                    .iter()
                    .map(|m| (m.node_id.clone(), ens.ensemble_id.clone())),
            );
        }
        member_to_ensemble
    }

    /// Resolve one raw node id to its collapsed key: the ensemble it is the
    /// join of, else its ensemble (as a member), else itself when collapsed.
    pub(super) fn dfs_endpoint(
        raw: &str,
        by_join: &HashMap<&str, &EnsembleLiveInfo>,
        member_to_ensemble: &HashMap<String, String>,
        collapsed: &HashMap<String, (bool, i64)>,
    ) -> Option<String> {
        if let Some(ens) = by_join.get(raw) {
            return Some(ens.ensemble_id.clone());
        }
        if let Some(ek) = member_to_ensemble.get(raw) {
            return Some(ek.clone());
        }
        if collapsed.contains_key(raw) {
            return Some(raw.to_string());
        }
        None
    }

    /// Build the collapsed-edge adjacency with member->join edges removed and
    /// duplicates dropped, sorting each node's outgoing edges by priority.
    pub(super) fn dfs_collapsed_edges(
        state: &GraphLiveState,
        collapsed: &HashMap<String, (bool, i64)>,
        by_join: &HashMap<&str, &EnsembleLiveInfo>,
        member_to_ensemble: &HashMap<String, String>,
    ) -> HashMap<String, Vec<(String, GraphEdgeCondition)>> {
        let by_member: HashMap<&str, &EnsembleLiveInfo> = state
            .ensembles
            .iter()
            .flat_map(|e| e.members.iter().map(move |m| (m.node_id.as_str(), e)))
            .collect();
        let mut collapsed_edges: HashMap<String, Vec<(String, GraphEdgeCondition)>> =
            HashMap::new();
        for edge in &state.effective_edges {
            let from_raw = edge.from_node.as_str();
            let to_raw = edge.to_node.as_str();
            if by_member
                .get(from_raw)
                .is_some_and(|e| e.join_node_id.as_str() == to_raw)
            {
                continue;
            }
            let from_key = Self::dfs_endpoint(from_raw, by_join, member_to_ensemble, collapsed);
            let to_key = Self::dfs_endpoint(to_raw, by_join, member_to_ensemble, collapsed);
            let (Some(fk), Some(tk)) = (from_key, to_key) else {
                continue;
            };
            let entry = collapsed_edges.entry(fk).or_default();
            if !entry
                .iter()
                .any(|(ek, ec)| ek == &tk && ec == &edge.condition)
            {
                entry.push((tk, edge.condition.clone()));
            }
        }
        for edges in collapsed_edges.values_mut() {
            edges.sort_by(|a, b| {
                let (pa, la) = Self::edge_priority(&a.1);
                let (pb, lb) = Self::edge_priority(&b.1);
                pa.cmp(&pb).then_with(|| la.cmp(&lb))
            });
        }
        collapsed_edges
    }

    /// The DFS entry key: the collapsed key with no incoming edge, else the
    /// one at the lowest node position. The `unwrap` panics only on an empty
    /// `collapsed`, which the caller guards.
    pub(super) fn dfs_entry_key(
        collapsed: &HashMap<String, (bool, i64)>,
        collapsed_pos: &HashMap<String, i64>,
        adjacency: &HashMap<String, Vec<(String, GraphEdgeCondition)>>,
    ) -> String {
        let mut incoming: HashSet<String> = HashSet::new();
        for tos in adjacency.values() {
            for (tk, _) in tos {
                incoming.insert(tk.clone());
            }
        }
        collapsed
            .keys()
            .find(|k| !incoming.contains(*k))
            .cloned()
            .or_else(|| {
                collapsed
                    .keys()
                    .min_by_key(|k| collapsed_pos.get(*k).copied().unwrap_or(i64::MAX))
                    .cloned()
            })
            .unwrap()
    }

    /// Depth-first walk of the collapsed adjacency, appending each key once to
    /// `order`.
    pub(super) fn dfs_rec(
        key: &str,
        adjacency: &HashMap<String, Vec<(String, GraphEdgeCondition)>>,
        visited: &mut HashSet<String>,
        order: &mut Vec<String>,
    ) {
        if visited.contains(key) {
            return;
        }
        visited.insert(key.to_string());
        order.push(key.to_string());
        let Some(edges) = adjacency.get(key) else {
            return;
        };
        for (tk, _) in edges {
            if !visited.contains(tk.as_str()) {
                Self::dfs_rec(tk, adjacency, visited, order);
            }
        }
    }

    /// Map collapsed keys back to concrete node ids for selection. For
    /// ensembles, use the first member's node id (renderer highlights on any
    /// member).
    pub(super) fn dfs_concrete_ids(order: Vec<String>, state: &GraphLiveState) -> Vec<String> {
        let mut ensemble_first_member: HashMap<String, String> = HashMap::new();
        for ens in &state.ensembles {
            if let Some(first) = ens.members.first() {
                ensemble_first_member.insert(ens.ensemble_id.clone(), first.node_id.clone());
            }
        }
        order
            .into_iter()
            .map(|ck| match ensemble_first_member.get(&ck) {
                Some(mid) => mid.clone(),
                None => ck,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::graphs::{
        Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphNodeRun,
        GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    use crate::domain::queues::Queue;
    use chrono::Utc;
    use serde_json::json;
    use tempfile::NamedTempFile;

    fn test_db() -> Database {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Database::new(&path).expect("create test db")
    }

    fn make_graph(id: &str, status: GraphStatus) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: format!("Graph {id}"),
            description: None,
            workdir: "/tmp/test".to_string(),
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
            require_spec_type: false,
        }
    }

    fn make_spec(id: &str, graph_id: &str, status: GraphSpecStatus, position: i64) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: Some(graph_id.to_string()),
            name: format!("Spec {id}"),
            description: None,
            position,
            parallelizable: false,
            status,
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
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        }
    }

    fn make_node(id: &str, spec_id: &str, kind: GraphNodeKind, position: i64) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("Node {id}"),
            kind,
            config: json!({}),
            position,
            created_at: Utc::now(),
        }
    }

    fn make_edge(
        id: &str,
        spec_id: &str,
        from: &str,
        to: &str,
        condition: GraphEdgeCondition,
    ) -> GraphEdge {
        GraphEdge {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition,
        }
    }

    fn make_run(
        graph_id: &str,
        spec_id: &str,
        node_id: &str,
        status: GraphRunStatus,
        iteration: i64,
        output: Option<Value>,
    ) -> GraphNodeRun {
        GraphNodeRun {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: graph_id.to_string(),
            spec_id: spec_id.to_string(),
            node_id: node_id.to_string(),
            status,
            input: None,
            output,
            started_at: Utc::now(),
            completed_at: None,
            iteration,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    fn details_from_graph(db: &Database, lp: &Graph) -> crate::domain::graphs::GraphDetails {
        let specs_with_details: Vec<crate::domain::graphs::GraphSpecDetails> = db
            .list_graph_specs(&lp.id)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|s| db.get_graph_spec_details(&s.id).ok().flatten())
            .collect();
        let graph_nodes = db.list_graph_nodes_for_graph(&lp.id).unwrap_or_default();
        let graph_edges = db.list_graph_edges_for_graph(&lp.id).unwrap_or_default();
        let completion_hook_runs = db
            .list_graph_completion_hook_runs(&lp.id)
            .unwrap_or_default();
        crate::domain::graphs::GraphDetails {
            lp: lp.clone(),
            graph_nodes,
            graph_edges,
            specs: specs_with_details,
            completion_hook_runs,
        }
    }

    // ── Tests ───────────────────────────────────────────────────

    #[test]
    fn empty_graph_yields_well_formed_snapshot() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Draft);
        db.insert_graph(&lp).unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.graph_id, "lp1");
        assert_eq!(state.graph_status, GraphStatus::Draft);
        assert!(state.spec_queue.is_empty());
        assert_eq!(state.done_count, 0);
        assert_eq!(state.total_count, 0);
        assert!(state.current_spec_id.is_none());
        assert!(state.current_node_id.is_none());
    }

    /// SPECCODE: the header's completed-spec labels carry the code before
    /// the name; a codeless spec yields its bare name with no placeholder.
    #[test]
    fn completed_spec_labels_prefix_code_before_name() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        let mut coded = make_spec("s1", "lp1", GraphSpecStatus::Completed, 1);
        coded.name = "Alpha".to_string();
        coded.spec_code = Some("FIX-7".to_string());
        db.insert_graph_spec(&coded).unwrap();
        let mut codeless = make_spec("s2", "lp1", GraphSpecStatus::Completed, 2);
        codeless.name = "Beta".to_string();
        db.insert_graph_spec(&codeless).unwrap();

        let labels = completed_spec_labels(&db, &lp);
        assert_eq!(
            labels,
            vec!["[FIX-7] Alpha".to_string(), "Beta".to_string()]
        );
    }

    #[test]
    fn running_graph_with_bound_specs_queue_order_and_progress() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Completed, 1))
            .unwrap();
        db.insert_graph_spec(&make_spec("s2", "lp1", GraphSpecStatus::Running, 2))
            .unwrap();
        db.insert_graph_spec(&make_spec("s3", "lp1", GraphSpecStatus::Pending, 3))
            .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.spec_queue.len(), 3);
        assert_eq!(state.spec_queue[0].spec_id, "s1");
        assert_eq!(state.spec_queue[0].status, GraphSpecStatus::Completed);
        assert_eq!(state.spec_queue[1].spec_id, "s2");
        assert_eq!(state.spec_queue[1].status, GraphSpecStatus::Running);
        assert_eq!(state.spec_queue[2].spec_id, "s3");
        assert_eq!(state.spec_queue[2].status, GraphSpecStatus::Pending);

        assert_eq!(state.done_count, 1);
        assert_eq!(state.total_count, 3);
        assert_eq!(state.current_spec_id.as_deref(), Some("s2"));
    }

    #[test]
    fn queue_run_queue_uses_queue_member_order() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);

        // Create a queue and add specs to it.
        let queue = Queue {
            id: "queue1".to_string(),
            name: "Test Queue".to_string(),
            created_at: Utc::now(),
        };
        db.insert_queue(&queue).unwrap();

        // Specs are NOT bound to the graph (graph_id = None) — they're queue members.
        let ps1 = GraphSpec {
            id: "ps1".to_string(),
            graph_id: None,
            name: "Queue Spec 1".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Completed,
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
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        };
        let ps2 = GraphSpec {
            id: "ps2".to_string(),
            graph_id: None,
            name: "Queue Spec 2".to_string(),
            description: None,
            position: 2,
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
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        };
        db.insert_graph_spec(&ps1).unwrap();
        db.insert_graph_spec(&ps2).unwrap();

        db.append_queue_member("queue1", "ps1", None).unwrap();
        db.append_queue_member("queue1", "ps2", None).unwrap();

        // Set the graph's active queue.
        let mut lp_with_queue = lp.clone();
        lp_with_queue.active_run_queue_id = Some("queue1".to_string());
        db.update_graph_details(
            &lp.id,
            Some(&lp.name),
            None,
            lp_with_queue.active_run_queue_id.as_deref(),
        )
        .unwrap();

        let details = details_from_graph(&db, &lp_with_queue);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.spec_queue.len(), 2);
        assert_eq!(state.spec_queue[0].spec_id, "ps1");
        assert_eq!(state.spec_queue[1].spec_id, "ps2");
        assert_eq!(state.done_count, 1);
        assert_eq!(state.total_count, 2);
    }

    #[test]
    fn effective_graph_spec_wins_over_graph() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        // Graph-level graph.
        db.insert_graph_node(&GraphNode {
            id: "ln1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            name: "Graph Node".to_string(),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();

        // Spec with its own graph.
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("sn1", "s1", GraphNodeKind::Agent, 1))
            .unwrap();
        db.insert_graph_node(&make_node("sn2", "s1", GraphNodeKind::Check, 2))
            .unwrap();
        db.insert_graph_edge(&make_edge(
            "se1",
            "s1",
            "sn1",
            "sn2",
            GraphEdgeCondition::Pass,
        ))
        .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        // Spec graph should win.
        assert_eq!(state.effective_nodes.len(), 2);
        assert_eq!(state.effective_nodes[0].id, "sn1");
        assert_eq!(state.effective_nodes[1].id, "sn2");
        assert_eq!(state.effective_edges.len(), 1);
    }

    #[test]
    fn effective_graph_falls_back_to_graph_when_spec_has_no_nodes() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        // Graph-level graph.
        db.insert_graph_node(&GraphNode {
            id: "ln1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            name: "Graph Node".to_string(),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();

        // Spec with NO nodes.
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        // Should fall back to top-level graph.
        assert_eq!(state.effective_nodes.len(), 1);
        assert_eq!(state.effective_nodes[0].id, "ln1");
    }

    #[test]
    fn current_node_from_active_run() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Agent, 1))
            .unwrap();

        let run = make_run("lp1", "s1", "n1", GraphRunStatus::Running, 1, None);
        db.insert_graph_run(&run).unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.current_node_id.as_deref(), Some("n1"));
        assert_eq!(state.current_node_status, Some(GraphRunStatus::Running));
        assert_eq!(state.current_node_iteration, Some(1));
    }

    #[test]
    fn current_node_from_latest_completed_run() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Agent, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n2", "s1", GraphNodeKind::Check, 2))
            .unwrap();

        // n1 completed, no active run for s1.
        let run = make_run(
            "lp1",
            "s1",
            "n2",
            GraphRunStatus::Pass,
            1,
            Some(json!({"summary": "done"})),
        );
        db.insert_graph_run(&run).unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.current_node_id.as_deref(), Some("n2"));
        assert_eq!(state.current_node_status, Some(GraphRunStatus::Pass));
    }

    #[test]
    fn requested_node_info_for_non_current_node() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Agent, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n2", "s1", GraphNodeKind::Check, 2))
            .unwrap();

        // n1 already completed; n2 is the active run (the "current" node).
        db.insert_graph_run(&make_run(
            "lp1",
            "s1",
            "n1",
            GraphRunStatus::Pass,
            1,
            Some(json!({"summary": "n1 finished"})),
        ))
        .unwrap();
        db.insert_graph_run(&make_run(
            "lp1",
            "s1",
            "n2",
            GraphRunStatus::Running,
            1,
            None,
        ))
        .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();
        assert_eq!(state.current_node_id.as_deref(), Some("n2"));

        // Requesting n1 explicitly (not the current node) still works.
        let n1_info = resolve_node_run_info(&db, "s1", "n1");
        assert_eq!(n1_info.status, Some(GraphRunStatus::Pass));

        let n2_info = resolve_node_run_info(&db, "s1", "n2");
        assert_eq!(n2_info.status, Some(GraphRunStatus::Running));
    }

    #[test]
    fn no_graph_selected_yields_none() {
        // assemble_graph_live_state always returns Some when given valid details.
        // The "no graph selected" case is handled by the caller not calling this.
        // But we test that a graph with no specs and no runs works.
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Draft);
        db.insert_graph(&lp).unwrap();
        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details);
        assert!(state.is_some());
        let state = state.unwrap();
        assert!(state.spec_queue.is_empty());
        assert!(state.current_node_id.is_none());
    }

    #[test]
    fn degraded_case_no_runs_no_specs() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();
        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.done_count, 0);
        assert_eq!(state.total_count, 0);
        assert!(state.current_spec_id.is_none());
        assert!(state.current_node_id.is_none());
        assert!(state.effective_nodes.is_empty());
        assert!(state.effective_edges.is_empty());
    }

    #[test]
    fn progress_all_completed() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Completed);
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Completed, 1))
            .unwrap();
        db.insert_graph_spec(&make_spec("s2", "lp1", GraphSpecStatus::Completed, 2))
            .unwrap();
        db.insert_graph_spec(&make_spec("s3", "lp1", GraphSpecStatus::Skipped, 3))
            .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.done_count, 2); // Skipped doesn't count as done
        assert_eq!(state.total_count, 3);
        assert!(state.current_spec_id.is_none()); // No Running or Pending
    }

    #[test]
    fn trigger_type_labels() {
        let db = test_db();
        let mut lp = make_graph("lp1", GraphStatus::Draft);
        lp.trigger = Some(crate::domain::models::Trigger::Cron {
            schedule_expr: "30 9 * * *".to_string(),
        });
        db.insert_graph(&lp).unwrap();
        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.trigger_type, "cron");
        assert_eq!(state.schedule_expr.as_deref(), Some("30 9 * * *"));
        assert!(state.watch_path.is_none());
    }

    #[test]
    fn autorun_at_propagated() {
        let db = test_db();
        let mut lp = make_graph("lp1", GraphStatus::Failed);
        let now = Utc::now();
        lp.autorun_at = Some(now);
        db.insert_graph(&lp).unwrap();
        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert!(state.autorun_at.is_some());
    }

    fn insert_ensemble_fixture(db: &Database, spec_id: &str) {
        use crate::domain::graphs::{Ensemble, EnsembleMember, GraphEdgeCondition, GraphNodeKind};

        let member_nodes: Vec<GraphNode> = (1..=3)
            .map(|i| GraphNode {
                id: format!("m{i}"),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: format!("Proposers [{i}]"),
                kind: GraphNodeKind::Agent,
                config: json!({}),
                position: i,
                created_at: Utc::now(),
            })
            .collect();
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: "Proposers (quorum)".to_string(),
            kind: GraphNodeKind::Join,
            config: json!({}),
            position: 4,
            created_at: Utc::now(),
        };
        let mut edges = Vec::new();
        for node in &member_nodes {
            edges.push(GraphEdge {
                id: format!("entry-{}", node.id),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                from_node: "n1".to_string(),
                to_node: node.id.clone(),
                condition: GraphEdgeCondition::Always,
            });
            edges.push(GraphEdge {
                id: format!("join-{}", node.id),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                from_node: node.id.clone(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            });
        }
        let ensemble = Ensemble {
            mutex: Vec::new(),
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "{{spec_content}}".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "n1".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 3,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "n2".to_string(),
            on_fail_to: None,
            kind: crate::domain::graphs::EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        };
        let members: Vec<EnsembleMember> = member_nodes
            .iter()
            .enumerate()
            .map(|(i, node)| EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: node.id.clone(),
                position: i as i64,
                platform: "openrouter".to_string(),
                model: Some(format!("model-{i}")),
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect();
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();
    }

    // ── NodeRunInfo::from_run and default ────────────────────────

    #[test]
    fn node_run_info_default_is_empty() {
        let info = NodeRunInfo::default();
        assert!(info.status.is_none());
        assert!(info.started_at.is_none());
        assert!(info.iteration.is_none());
    }

    #[test]
    fn node_run_info_from_run_captures_all_fields() {
        let db = test_db();
        let run = make_run(
            "lp1",
            "s1",
            "n1",
            GraphRunStatus::Pass,
            3,
            Some(json!({"summary": "all good"})),
        );
        let info = NodeRunInfo::from_run(&db, &run);
        assert_eq!(info.status, Some(GraphRunStatus::Pass));
        assert!(info.started_at.is_some());
        assert_eq!(info.iteration, Some(3));
    }

    #[test]
    fn node_run_info_from_run_with_none_output() {
        let db = test_db();
        let run = make_run("lp1", "s1", "n1", GraphRunStatus::Running, 1, None);
        let info = NodeRunInfo::from_run(&db, &run);
        assert_eq!(info.status, Some(GraphRunStatus::Running));
    }

    // ── resolve_node_run_info edge cases ─────────────────────────

    #[test]
    fn resolve_node_run_info_no_runs_returns_default() {
        let db = test_db();
        let info = resolve_node_run_info(&db, "nonexistent-spec", "nonexistent-node");
        assert!(info.status.is_none());
    }

    #[test]
    fn resolve_node_run_info_finds_completed_run() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Check, 1))
            .unwrap();

        db.insert_graph_run(&make_run(
            "lp1",
            "s1",
            "n1",
            GraphRunStatus::Pass,
            1,
            Some(json!("completed")),
        ))
        .unwrap();

        let info = resolve_node_run_info(&db, "s1", "n1");
        assert_eq!(info.status, Some(GraphRunStatus::Pass));
    }

    #[test]
    fn resolve_node_run_info_wrong_spec_returns_default() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Check, 1))
            .unwrap();

        db.insert_graph_run(&make_run(
            "lp1",
            "s1",
            "n1",
            GraphRunStatus::Pass,
            1,
            Some(json!("done")),
        ))
        .unwrap();

        // Wrong spec_id — the active run won't match and listing runs for
        // a nonexistent spec returns empty.
        let info = resolve_node_run_info(&db, "s-other", "n1");
        assert!(info.status.is_none());
    }

    // ── assemble_graph_live_state edge cases ──────────────────────

    #[test]
    fn paused_graph_has_no_current_spec() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Paused);
        db.insert_graph(&lp).unwrap();
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Completed, 1))
            .unwrap();
        db.insert_graph_spec(&make_spec("s2", "lp1", GraphSpecStatus::Skipped, 2))
            .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();
        assert!(state.current_spec_id.is_none());
        assert_eq!(state.done_count, 1); // Only Completed counts
    }

    #[test]
    fn watch_trigger_type_label() {
        let db = test_db();
        let mut lp = make_graph("lp1", GraphStatus::Draft);
        lp.trigger = Some(crate::domain::models::Trigger::Watch {
            path: "/tmp/watch".to_string(),
            events: vec![crate::domain::models::WatchEvent::Modify],
            debounce_seconds: 5,
            recursive: false,
        });
        db.insert_graph(&lp).unwrap();
        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();
        assert_eq!(state.trigger_type, "watch");
        assert_eq!(state.watch_path.as_deref(), Some("/tmp/watch"));
        assert!(state.schedule_expr.is_none());
    }

    #[test]
    fn manual_trigger_type_label() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Draft);
        db.insert_graph(&lp).unwrap();
        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();
        assert_eq!(state.trigger_type, "manual");
    }

    #[test]
    fn multiple_specs_progress_counting() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();

        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Completed, 1))
            .unwrap();
        db.insert_graph_spec(&make_spec("s2", "lp1", GraphSpecStatus::Completed, 2))
            .unwrap();
        db.insert_graph_spec(&make_spec("s3", "lp1", GraphSpecStatus::Running, 3))
            .unwrap();
        db.insert_graph_spec(&make_spec("s4", "lp1", GraphSpecStatus::Pending, 4))
            .unwrap();
        db.insert_graph_spec(&make_spec("s5", "lp1", GraphSpecStatus::Skipped, 5))
            .unwrap();

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.done_count, 2);
        assert_eq!(state.total_count, 5);
        assert_eq!(state.current_spec_id.as_deref(), Some("s3"));
    }

    #[test]
    fn ensembles_live_info_reports_join_and_per_member_status() {
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Agent, 0))
            .unwrap();
        db.insert_graph_node(&make_node("n2", "s1", GraphNodeKind::Agent, 5))
            .unwrap();
        insert_ensemble_fixture(&db, "s1");

        db.insert_graph_run(&make_run(
            "lp1",
            "s1",
            "m1",
            GraphRunStatus::Pass,
            1,
            Some(json!({"stdout": "draft one"})),
        ))
        .unwrap();
        db.insert_graph_run(&make_run(
            "lp1",
            "s1",
            "m2",
            GraphRunStatus::Running,
            1,
            None,
        ))
        .unwrap();
        // m3 has no run yet — still pending.

        let details = details_from_graph(&db, &lp);
        let state = assemble_graph_live_state(&db, &details).unwrap();

        assert_eq!(state.ensembles.len(), 1);
        let ensemble = &state.ensembles[0];
        assert_eq!(ensemble.name, "Proposers");
        assert_eq!(ensemble.join_node_id, "join1");
        assert_eq!(ensemble.members.len(), 3);
        assert_eq!(ensemble.members[0].label, "openrouter/model-0");
        assert_eq!(ensemble.members[0].status, Some(GraphRunStatus::Pass));
        assert_eq!(ensemble.members[1].status, Some(GraphRunStatus::Running));
        assert_eq!(ensemble.members[2].status, None);
    }

    #[test]
    fn live_member_label_uses_display() {
        use std::collections::HashMap;
        let db = test_db();
        let lp = make_graph("lp1", GraphStatus::Running);
        db.insert_graph(&lp).unwrap();
        db.insert_graph_spec(&make_spec("s1", "lp1", GraphSpecStatus::Running, 1))
            .unwrap();
        db.insert_graph_node(&make_node("n1", "s1", GraphNodeKind::Agent, 0))
            .unwrap();
        db.insert_graph_node(&make_node("n2", "s1", GraphNodeKind::Agent, 5))
            .unwrap();
        insert_ensemble_fixture(&db, "s1");

        let join = make_node("join1", "s1", GraphNodeKind::Join, 4);
        let mut map: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        map.insert(
            "openrouter".to_string(),
            (
                Some("OpenRouter".to_string()),
                Some("OpenRouter Hub".to_string()),
            ),
        );
        let infos = super::resolve_ensembles_live_info(&db, &[join], Some("s1"), &map);
        assert_eq!(infos.len(), 1);
        assert_eq!(
            infos[0].members[0].label,
            "OpenRouter · OpenRouter Hub/model-0"
        );

        let empty: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
        let join = make_node("join1", "s1", GraphNodeKind::Join, 4);
        let infos = super::resolve_ensembles_live_info(&db, &[join], Some("s1"), &empty);
        assert_eq!(infos[0].members[0].label, "openrouter/model-0");
    }
}
