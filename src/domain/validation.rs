//! Domain validation rules for identifiers, prompts, and paths.

use std::collections::{HashMap, HashSet};

use crate::domain::graphs::{
    EnsembleDetails, EnsembleKind, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind,
};

pub const MAX_ID_LENGTH: usize = 64;
pub const MAX_PROMPT_LENGTH: usize = 50_000;
pub const MAX_PATH_LENGTH: usize = 4096;

/// Validate an identifier: non-empty, max length, alphanumeric + hyphens/underscores.
pub fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("ID cannot be empty".to_string());
    }
    if id.len() > MAX_ID_LENGTH {
        return Err(format!(
            "ID exceeds maximum length of {MAX_ID_LENGTH} characters"
        ));
    }
    if !id
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "ID must contain only alphanumeric characters, hyphens, and underscores".to_string(),
        );
    }
    Ok(())
}

/// Validate a prompt string: non-empty, max length.
pub fn validate_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() {
        return Err("Prompt cannot be empty".to_string());
    }
    if prompt.len() > MAX_PROMPT_LENGTH {
        return Err(format!(
            "Prompt exceeds maximum length of {MAX_PROMPT_LENGTH} characters"
        ));
    }
    Ok(())
}

/// Validate a path string: non-empty, max length, absolute.
pub fn validate_watch_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("Path cannot be empty".to_string());
    }
    if path.len() > MAX_PATH_LENGTH {
        return Err(format!(
            "Path exceeds maximum length of {MAX_PATH_LENGTH} characters"
        ));
    }
    if !std::path::Path::new(path).is_absolute() {
        return Err("Path must be absolute".to_string());
    }
    Ok(())
}

pub use super::validation_mutex::validate_mutex_list;

/// Returns `Err` when `min_pass` is out of the [1, member_count] range.
/// Applies to every ensemble kind. Used by every write site so the writer
/// and the importer (`graph_transfer.rs`) cannot drift apart.
/// Error text: "Ensemble '<name>' has an invalid min_pass (<n>) for <m> members."
pub fn validate_ensemble_min_pass(
    ensemble_name: &str,
    min_pass: i64,
    member_count: usize,
) -> Result<(), String> {
    if min_pass < 1 || min_pass > member_count as i64 {
        return Err(format!(
            "Ensemble '{ensemble_name}' has an invalid min_pass ({min_pass}) for {member_count} members."
        ));
    }
    Ok(())
}

/// Validate every ensemble (F1) found within one graph (a graph's top-level
/// graph, or a single spec's own graph — never both mixed together, since an
/// ensemble belongs to exactly one) as a unit: entry reachable, every member
/// wired to the join, and both exits wired to nodes that actually exist in
/// this same graph. Called at `graph_run` so a structurally broken ensemble
/// fails fast with an actionable message instead of surfacing as a runtime
/// "ambiguous outgoing edges" or "node not found" deep into a run.
///
/// In practice every one of these invariants is guaranteed by construction —
/// `graph_add_ensemble`/`graph_update_ensemble` are the only writers of
/// ensemble-owned nodes/edges — so this exists as defense in depth against a
/// future bug or direct DB edit, not because callers are expected to trip it
/// today.
pub fn validate_ensembles_in_graph(
    ensembles: &[EnsembleDetails],
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> Result<(), String> {
    for details in ensembles {
        // CM49: a crew-bound ensemble's members are materialized by the sync
        // path (which runs before its first dispatch), so its member wiring is
        // not yet present at import time; its size is the crew's and is
        // enforced at bind/dispatch. Literature/variable wiring checks
        // (entry/join/exit targets) still apply.
        if details.ensemble.crew_id.is_some() || details.ensemble.crew_name.is_some() {
            ensure_ensemble_wiring_ends(details, ensembles, nodes, edges)?;
            continue;
        }
        ensure_ensemble_membership_bounds(details)?;
        ensure_ensemble_wiring_ends(details, ensembles, nodes, edges)?;
        ensure_ensemble_member_edges(details, nodes, edges)?;
        ensure_ensemble_entry_fanout(details, edges)?;
    }
    Ok(())
}

/// Per-ensemble member-count and `min_pass` bounds.
fn ensure_ensemble_membership_bounds(details: &EnsembleDetails) -> Result<(), String> {
    let ensemble = &details.ensemble;
    let label = format!("Ensemble '{}' ('{}')", ensemble.id, ensemble.name);

    let min_members = match ensemble.kind {
        EnsembleKind::Parallel => 2,
        EnsembleKind::Cascade | EnsembleKind::RoundRobin => 1,
    };
    if details.members.len() < min_members {
        return Err(format!("{label} has fewer than {min_members} members."));
    }
    if ensemble.kind == EnsembleKind::Parallel
        && (ensemble.min_pass < 1 || ensemble.min_pass > details.members.len() as i64)
    {
        return Err(format!(
            "{label} has an invalid min_pass ({}) for {} members.",
            ensemble.min_pass,
            details.members.len()
        ));
    }
    Ok(())
}

/// Existence of the entry/join/exit-target nodes, then the quorum's pass and
/// fail exit edges — in that exact order.
fn ensure_ensemble_wiring_ends(
    details: &EnsembleDetails,
    ensembles: &[EnsembleDetails],
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> Result<(), String> {
    let ensemble = &details.ensemble;
    let label = format!("Ensemble '{}' ('{}')", ensemble.id, ensemble.name);

    if !graph_node_exists(nodes, &ensemble.entry_from_node) {
        return Err(format!(
            "{label}'s entry node '{}' is not reachable in this graph.",
            ensemble.entry_from_node
        ));
    }
    if !graph_node_exists(nodes, &ensemble.join_node_id) {
        return Err(format!(
            "{label}'s quorum node '{}' is missing from this graph.",
            ensemble.join_node_id
        ));
    }
    if !graph_node_exists(nodes, &ensemble.on_pass_to) {
        return Err(format!(
            "{label}'s on_pass_to target '{}' is not wired into this graph.",
            ensemble.on_pass_to
        ));
    }
    if let Some(on_fail_to) = &ensemble.on_fail_to {
        if !graph_node_exists(nodes, on_fail_to) {
            return Err(format!(
                "{label}'s on_fail_to target '{on_fail_to}' is not wired into this graph."
            ));
        }
    }
    if !has_ensemble_exit_edge(
        ensembles,
        ensemble,
        &ensemble.on_pass_to,
        &GraphEdgeCondition::Pass,
        edges,
    ) {
        let target = exit_target_label(ensembles, ensemble, &ensemble.on_pass_to);
        return Err(format!(
            "{label}'s quorum has no pass edge to its on_pass_to target ({target})."
        ));
    }
    if let Some(on_fail_to) = &ensemble.on_fail_to {
        if !has_ensemble_exit_edge(
            ensembles,
            ensemble,
            on_fail_to,
            &GraphEdgeCondition::Fail,
            edges,
        ) {
            let target = exit_target_label(ensembles, ensemble, on_fail_to);
            return Err(format!(
                "{label}'s quorum has no fail edge to its on_fail_to target ({target})."
            ));
        }
    }
    Ok(())
}

/// Every member must exist, carry the primary entry edge, and route to the
/// quorum.
fn ensure_ensemble_member_edges(
    details: &EnsembleDetails,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> Result<(), String> {
    let ensemble = &details.ensemble;
    let label = format!("Ensemble '{}' ('{}')", ensemble.id, ensemble.name);

    for member in &details.members {
        if !graph_node_exists(nodes, &member.node_id) {
            return Err(format!(
                "{label}'s member node '{}' is missing from this graph.",
                member.node_id
            ));
        }
        if !graph_has_edge(
            edges,
            &ensemble.entry_from_node,
            &member.node_id,
            &ensemble.entry_condition,
        ) {
            return Err(format!(
                "{label}'s member '{}' has no entry edge from '{}'.",
                member.node_id, ensemble.entry_from_node
            ));
        }
        if !graph_has_edge(
            edges,
            &member.node_id,
            &ensemble.join_node_id,
            &GraphEdgeCondition::Always,
        ) {
            return Err(format!(
                "{label}'s member '{}' is not wired to the quorum — every member must route to the quorum.",
                member.node_id
            ));
        }
    }
    Ok(())
}

/// Every entry source — the primary `entry_from_node` and any added
/// via `add_entry_from` — must reach EVERY member. A source reaching
/// only some members would make entering from it ambiguous at runtime
/// (the engine resolves an ensemble only when the outgoing targets are
/// exactly the full member set). This runs after the per-member
/// primary-entry check in [`ensure_ensemble_member_edges`] so a broken
/// primary entry keeps its original message.
fn ensure_ensemble_entry_fanout(
    details: &EnsembleDetails,
    edges: &[GraphEdge],
) -> Result<(), String> {
    let ensemble = &details.ensemble;
    let label = format!("Ensemble '{}' ('{}')", ensemble.id, ensemble.name);

    let member_ids: HashSet<&str> = details
        .members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let mut reached: HashMap<(&str, GraphEdgeCondition), HashSet<&str>> = HashMap::new();
    for edge in edges {
        if !member_ids.contains(edge.to_node.as_str()) {
            continue;
        }
        if member_ids.contains(edge.from_node.as_str()) || edge.from_node == ensemble.join_node_id {
            continue;
        }
        reached
            .entry((edge.from_node.as_str(), edge.condition.clone()))
            .or_default()
            .insert(edge.to_node.as_str());
    }
    let mut sources: Vec<((&str, GraphEdgeCondition), usize)> = reached
        .iter()
        .map(|(source, to)| (source.clone(), to.len()))
        .collect();
    sources.sort_by(|a, b| {
        a.0 .0
            .cmp(b.0 .0)
            .then(a.0 .1.as_str().cmp(b.0 .1.as_str()))
    });
    for ((from, _condition), count) in sources {
        if count != member_ids.len() {
            return Err(format!(
                "{label} has incomplete entry wiring from '{from}': reaches {count} of {} members — rewire with graph_update_ensemble (from_node/add_entry_from).",
                member_ids.len()
            ));
        }
    }
    Ok(())
}

/// Whether `id` names a node of this graph.
fn graph_node_exists(nodes: &[GraphNode], id: &str) -> bool {
    nodes.iter().any(|node| node.id == id)
}

/// Whether a `from → to` edge with `condition` exists in this graph.
fn graph_has_edge(
    edges: &[GraphEdge],
    from: &str,
    to: &str,
    condition: &GraphEdgeCondition,
) -> bool {
    edges
        .iter()
        .any(|edge| edge.from_node == from && edge.to_node == to && edge.condition == *condition)
}

/// Whether the quorum's exit wiring for `condition` is complete. `target` is
/// usually a node id (one edge join→node), but when ensembles are chained the
/// row holds the *target ensemble's quorum id* instead — in which case every
/// member of that ensemble must have an edge from this join (the fan-out
/// `graph_update_ensemble` builds, with no intermediate node).
fn has_ensemble_exit_edge(
    ensembles: &[EnsembleDetails],
    ensemble: &crate::domain::graphs::Ensemble,
    target: &str,
    condition: &GraphEdgeCondition,
    edges: &[GraphEdge],
) -> bool {
    if let Some(target_details) = ensembles.iter().find(|details| {
        details.ensemble.join_node_id == target && details.ensemble.id != ensemble.id
    }) {
        return target_details.members.iter().all(|member| {
            edges.iter().any(|edge| {
                edge.from_node == ensemble.join_node_id
                    && edge.to_node == member.node_id
                    && edge.condition == *condition
            })
        });
    }
    edges.iter().any(|edge| {
        edge.from_node == ensemble.join_node_id
            && edge.to_node == target
            && edge.condition == *condition
    })
}

/// Human-readable form of an exit target for validation errors: the chained
/// ensemble's id when the row holds its quorum id, else the node id itself.
fn exit_target_label(
    ensembles: &[EnsembleDetails],
    ensemble: &crate::domain::graphs::Ensemble,
    target: &str,
) -> String {
    match ensembles.iter().find(|details| {
        details.ensemble.join_node_id == target && details.ensemble.id != ensemble.id
    }) {
        Some(target_details) => format!(
            "ensemble '{}' ('{}')",
            target_details.ensemble.id, target_details.ensemble.name
        ),
        None => format!("node '{target}'"),
    }
}

/// A state that ends the graph — a node with no outgoing edge for that state.
/// Informational; does not affect the Ok verdict.
#[derive(Debug)]
pub struct GraphTerminal {
    pub node_id: String,
    /// "pass" or "fail"
    pub state: String,
}

/// A non-terminal agent/check/gate node with no outgoing edge for the `fail`
/// status (CM19). When such a node returns a fail verdict the engine has no
/// edge to route it down: the spec terminates at that node with the message
/// "no outgoing edge for that status" and the run sits blocked until a human
/// intervenes.
///
/// Deliberate terminals are excluded: if the node's `pass` status also has no
/// outgoing edge the graph is meant to end there, and that is reported in
/// [`GraphValidationReport::terminals`] instead.
///
/// `has_error_path` records whether the node has an outgoing `error` edge —
/// the infrastructure-failure path, normally wired to the graph's infra node.
/// An `error` edge does NOT satisfy the `fail` requirement (a real fail verdict
/// is never routed down it); this flag only tells the reader whether an
/// infrastructure crash would also dead-end at this node.
#[derive(Debug)]
pub struct FailDeadEnd {
    pub node_id: String,
    pub has_error_path: bool,
}

/// An ensemble's quorum (join) node as seen by graph-level validation. The
/// join node itself is engine-managed and skipped by [`collect_outgoing_coverage`],
/// so the ensemble's fail exit has to be checked from the ensemble record.
pub struct EnsembleQuorumView<'a> {
    pub ensemble_id: &'a str,
    pub ensemble_name: &'a str,
    pub join_node_id: &'a str,
    /// `None` means the quorum has no `on_fail_to`: when the quorum fails, the
    /// spec ends at the join node.
    pub on_fail_to: Option<&'a str>,
}

/// An ensemble whose quorum can only end the spec on failure: it has
/// `on_pass_to` but no `on_fail_to`, and its join node has no outgoing
/// `fail`/`always` edge. Advisory, like [`FailDeadEnd`].
#[derive(Debug)]
pub struct QuorumFailDeadEnd {
    pub node_id: String,
    pub ensemble_id: String,
    pub ensemble_name: String,
}

/// Result of structural graph validation.
#[derive(Debug)]
pub struct GraphValidationReport {
    /// Nodes where a state ends the graph (no outgoing edge for that state).
    pub terminals: Vec<GraphTerminal>,
    /// Non-terminal agent/check/gate nodes with no outgoing `fail`/`always`
    /// edge (CM19). Advisory — does not affect the Ok verdict.
    pub fail_dead_ends: Vec<FailDeadEnd>,
    /// Ensembles whose quorum has no fail exit (no `on_fail_to` and no
    /// outgoing `fail`/`always` edge from the join node). Advisory.
    pub quorum_fail_dead_ends: Vec<QuorumFailDeadEnd>,
}

/// A node as seen by graph-level validation — identified by an opaque
/// string (a name in an import document, an id in a live graph).
pub struct GraphNodeView<'a> {
    pub id: &'a str,
    pub kind: GraphNodeKind,
    /// Declared route labels for router nodes; empty for every other kind.
    pub route_labels: &'a [String],
}

/// An edge as seen by graph-level validation.
pub struct GraphEdgeView<'a> {
    pub from: &'a str,
    pub to: &'a str,
    pub condition: &'a GraphEdgeCondition,
}

/// Validate structural properties of a complete graph. Returns `Err`
/// with a message naming the concrete node(s) or edge(s) involved.
///
/// Checks performed:
/// 1. Every edge references nodes that exist in the graph.
/// 2. Exactly one entry point (node with no incoming edges). Zero or 2+ is an error.
/// 3. Every node is reachable from the entry point via outgoing edges.
/// 4. Outgoing coverage — a state with no outgoing edge is a terminal exit,
///    reported in [`GraphValidationReport::terminals`], not an error. Every
///    router node still requires an outgoing edge for each of its declared
///    routes. Join nodes are skipped (engine-managed).
pub fn validate_graph(
    nodes: &[GraphNodeView<'_>],
    edges: &[GraphEdgeView<'_>],
) -> Result<GraphValidationReport, String> {
    validate_graph_with_ensembles(nodes, edges, &[])
}

/// Validate a graph together with the ensembles that own its join nodes.
/// [`validate_graph`] is this function with no ensembles, so a caller that
/// has no ensemble rows gets exactly the historical checks.
pub fn validate_graph_with_ensembles(
    nodes: &[GraphNodeView<'_>],
    edges: &[GraphEdgeView<'_>],
    ensembles: &[EnsembleQuorumView<'_>],
) -> Result<GraphValidationReport, String> {
    if nodes.is_empty() {
        return Ok(GraphValidationReport {
            terminals: Vec::new(),
            fail_dead_ends: Vec::new(),
            quorum_fail_dead_ends: Vec::new(),
        });
    }

    ensure_graph_edges_reference_known_nodes(nodes, edges)?;
    ensure_router_edges_declare_routes(nodes, edges)?;
    let entry_id = resolve_graph_entry_point(nodes, edges)?;
    ensure_all_nodes_reachable(nodes, edges, entry_id)?;
    let (terminals, fail_dead_ends, quorum_fail_dead_ends) =
        collect_outgoing_coverage(nodes, edges, ensembles)?;
    Ok(GraphValidationReport {
        terminals,
        fail_dead_ends,
        quorum_fail_dead_ends,
    })
}

/// 1. Edge target validation — every edge must reference existing nodes.
fn ensure_graph_edges_reference_known_nodes(
    nodes: &[GraphNodeView<'_>],
    edges: &[GraphEdgeView<'_>],
) -> Result<(), String> {
    let node_ids: HashSet<&str> = nodes.iter().map(|n| n.id).collect();
    for edge in edges {
        if !node_ids.contains(edge.from) {
            return Err(format!(
                "Edge '{}' -> '{}' references unknown node '{}'.",
                edge.from, edge.to, edge.from
            ));
        }
        if !node_ids.contains(edge.to) {
            return Err(format!(
                "Edge '{}' -> '{}' references unknown node '{}'.",
                edge.from, edge.to, edge.to
            ));
        }
    }
    Ok(())
}

/// Every edge with a route label must leave a router that declares it.
fn ensure_router_edges_declare_routes(
    nodes: &[GraphNodeView<'_>],
    edges: &[GraphEdgeView<'_>],
) -> Result<(), String> {
    for edge in edges {
        let Some(label) = edge.condition.route_label() else {
            continue;
        };
        let source = nodes
            .iter()
            .find(|node| node.id == edge.from)
            .expect("edge endpoints were validated above");
        if source.kind != GraphNodeKind::Router
            || !source.route_labels.iter().any(|declared| declared == label)
        {
            return Err(format!(
                "Edge '{}' -> '{}' uses undeclared route '{}' on source node '{}'.",
                edge.from, edge.to, label, edge.from
            ));
        }
    }
    Ok(())
}

/// 2. Entry point check — exactly one node with no incoming edges.
fn resolve_graph_entry_point<'a>(
    nodes: &[GraphNodeView<'a>],
    edges: &[GraphEdgeView<'_>],
) -> Result<&'a str, String> {
    let incoming: HashSet<&str> = edges.iter().map(|e| e.to).collect();
    let entry_nodes: Vec<&GraphNodeView<'_>> =
        nodes.iter().filter(|n| !incoming.contains(n.id)).collect();

    match entry_nodes.as_slice() {
        [single] => Ok(single.id),
        [] => Err(
            "Graph has no entry point: every node has an incoming edge. Expected exactly one node with no incoming edges.".to_string()
        ),
        many => {
            let mut names: Vec<&str> = many.iter().map(|n| n.id).collect();
            names.sort_unstable();
            Err(format!(
                "Graph has multiple entry points (nodes with no incoming edges): {}. Expected exactly one entry point.",
                names.join(", ")
            ))
        }
    }
}

/// 3. Reachability — every node reachable from entry via outgoing edges.
fn ensure_all_nodes_reachable(
    nodes: &[GraphNodeView<'_>],
    edges: &[GraphEdgeView<'_>],
    entry_id: &str,
) -> Result<(), String> {
    let mut adjacency: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        adjacency.entry(edge.from).or_default().push(edge.to);
    }
    let mut visited: HashSet<&str> = HashSet::new();
    let mut stack = vec![entry_id];
    visited.insert(entry_id);
    while let Some(current) = stack.pop() {
        if let Some(neighbors) = adjacency.get(current) {
            for neighbor in neighbors {
                if visited.insert(neighbor) {
                    stack.push(neighbor);
                }
            }
        }
    }
    let unreachable: Vec<&str> = nodes
        .iter()
        .filter(|n| !visited.contains(n.id))
        .map(|n| n.id)
        .collect();
    if !unreachable.is_empty() {
        let mut sorted = unreachable;
        sorted.sort_unstable();
        if sorted.len() == 1 {
            return Err(format!(
                "Node '{}' is unreachable from entry point '{}'.",
                sorted[0], entry_id
            ));
        }
        return Err(format!(
            "Nodes unreachable from entry point '{}': {}.",
            entry_id,
            sorted.join(", ")
        ));
    }
    Ok(())
}

/// The three collections produced by outgoing coverage: deliberate terminals,
/// CM19 fail dead-ends, and ensemble quorum dead-ends.
type OutgoingCoverage = (Vec<GraphTerminal>, Vec<FailDeadEnd>, Vec<QuorumFailDeadEnd>);

/// 4. Outgoing coverage — collect terminals (states with no outgoing edge),
///    CM19 fail dead-ends, and ensemble quorum dead-ends; routers must cover
///    every declared route.
fn collect_outgoing_coverage(
    nodes: &[GraphNodeView<'_>],
    edges: &[GraphEdgeView<'_>],
    ensembles: &[EnsembleQuorumView<'_>],
) -> Result<OutgoingCoverage, String> {
    let mut outgoing: HashMap<&str, Vec<&GraphEdgeCondition>> = HashMap::new();
    for edge in edges {
        outgoing.entry(edge.from).or_default().push(edge.condition);
    }

    let mut terminals: Vec<GraphTerminal> = Vec::new();
    let mut fail_dead_ends: Vec<FailDeadEnd> = Vec::new();

    for node in nodes {
        if node.kind == GraphNodeKind::Join {
            continue;
        }
        if node.kind == GraphNodeKind::Router {
            // Router route coverage is still required — a router missing a route
            // edge is a broken graph, not a terminal.
            ensure_router_route_coverage(node, &outgoing)?;
            continue;
        }
        // Agent / Check / Gate — missing pass or fail is a terminal, not an error.
        if matches!(
            node.kind,
            GraphNodeKind::Agent | GraphNodeKind::Check | GraphNodeKind::Gate
        ) {
            let outgoing_for_node = outgoing.get(node.id);
            if outgoing_for_node.is_none() {
                // No outgoing at all — both states terminate here.
                terminals.push(GraphTerminal {
                    node_id: node.id.to_string(),
                    state: "pass".to_string(),
                });
                terminals.push(GraphTerminal {
                    node_id: node.id.to_string(),
                    state: "fail".to_string(),
                });
                continue;
            }
            let conds = outgoing_for_node.expect("just checked Some");
            let has_pass = conds
                .iter()
                .any(|c| **c == GraphEdgeCondition::Pass || **c == GraphEdgeCondition::Always);
            let has_fail = conds.iter().any(|c| {
                **c == GraphEdgeCondition::Fail
                    || **c == GraphEdgeCondition::Always
                    || **c == GraphEdgeCondition::Error
            });
            if !has_pass {
                terminals.push(GraphTerminal {
                    node_id: node.id.to_string(),
                    state: "pass".to_string(),
                });
            }
            if !has_fail {
                terminals.push(GraphTerminal {
                    node_id: node.id.to_string(),
                    state: "fail".to_string(),
                });
            }
            // CM19: a node that continues on `pass` (has a pass/always edge)
            // but has no `fail`/`always` edge dead-ends when it fails — the
            // engine terminates the spec there. `Error` does NOT count: a
            // real fail verdict is never routed down an error edge. A node
            // with no `pass` edge either is a deliberate terminal (already in
            // `terminals`) and is not reported here.
            let has_fail_verdict_edge = conds
                .iter()
                .any(|c| **c == GraphEdgeCondition::Fail || **c == GraphEdgeCondition::Always);
            let has_error_edge = conds.iter().any(|c| **c == GraphEdgeCondition::Error);
            if has_pass && !has_fail_verdict_edge {
                fail_dead_ends.push(FailDeadEnd {
                    node_id: node.id.to_string(),
                    has_error_path: has_error_edge,
                });
            }
        }
    }

    let quorum_fail_dead_ends = collect_quorum_dead_ends(ensembles, nodes, &outgoing);

    Ok((terminals, fail_dead_ends, quorum_fail_dead_ends))
}

/// Ensembles whose quorum has no way to route a failure onward: no
/// `on_fail_to` and no `fail`/`always` edge leaving the join node. Reuses the
/// outgoing-edge map built by [`collect_outgoing_coverage`] — no second walk.
/// An ensemble with no join node in this graph (e.g. a spec's ensemble checked
/// against the graph's nodes) is skipped; it is not this graph's concern.
fn collect_quorum_dead_ends(
    ensembles: &[EnsembleQuorumView<'_>],
    nodes: &[GraphNodeView<'_>],
    outgoing: &HashMap<&str, Vec<&GraphEdgeCondition>>,
) -> Vec<QuorumFailDeadEnd> {
    let mut dead_ends = Vec::new();
    for ensemble in ensembles {
        if ensemble.on_fail_to.is_some() {
            continue;
        }
        let Some(join) = nodes
            .iter()
            .find(|node| node.id == ensemble.join_node_id)
            .filter(|node| node.kind == GraphNodeKind::Join)
        else {
            continue;
        };
        let has_fail_exit = outgoing.get(join.id).is_some_and(|conds| {
            conds
                .iter()
                .any(|c| **c == GraphEdgeCondition::Fail || **c == GraphEdgeCondition::Always)
        });
        if !has_fail_exit {
            dead_ends.push(QuorumFailDeadEnd {
                node_id: join.id.to_string(),
                ensemble_id: ensemble.ensemble_id.to_string(),
                ensemble_name: ensemble.ensemble_name.to_string(),
            });
        }
    }
    dead_ends
}

/// Router-only outgoing coverage: no outgoing edge may use an undeclared
/// route, and every declared route needs an outgoing edge. A router with no
/// outgoing edges at all is an unwired router, not yet an error.
fn ensure_router_route_coverage(
    node: &GraphNodeView<'_>,
    outgoing: &HashMap<&str, Vec<&GraphEdgeCondition>>,
) -> Result<(), String> {
    let Some(conds) = outgoing.get(node.id) else {
        return Ok(()); // unwired router, not yet an error
    };
    let has_any_route = conds.iter().any(|c| c.route_label().is_some());
    if !has_any_route {
        return Ok(());
    }
    for label in conds.iter().filter_map(|condition| condition.route_label()) {
        if !node.route_labels.iter().any(|declared| declared == label) {
            return Err(format!(
                "Router node '{}' has outgoing edge for undeclared route '{}'.",
                node.id, label
            ));
        }
    }
    for label in node.route_labels {
        let has_route = conds
            .iter()
            .any(|c| c.route_label() == Some(label.as_str()));
        if !has_route {
            return Err(format!(
                "Router node '{}' has no outgoing edge for route '{}'.",
                node.id, label
            ));
        }
    }
    Ok(())
}

#[path = "stale_names.rs"]
mod stale_names;
pub use stale_names::{find_stale_2x_names, StaleNameHit};

#[cfg(test)]
#[path = "validation_tests.rs"]
mod tests;
