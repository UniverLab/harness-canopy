//! Outgoing-edge coverage for graph validation — extracted from
//! `validation.rs` to stay below the rust-quality.sh file-size limit.
//! [`collect_outgoing_coverage`] is the only entry point used by
//! `validate_graph_with_ensembles`; everything else here is private to this
//! module. Public report types stay re-exported through
//! `crate::domain::validation` so callers are unaffected.

use std::collections::HashMap;

use super::{
    EnsembleQuorumView, FailDeadEnd, GraphEdgeView, GraphNodeView, GraphTerminal, QuorumFailDeadEnd,
};
use crate::domain::graphs::{GraphEdgeCondition, GraphNodeKind};

/// The three collections produced by outgoing coverage: deliberate terminals,
/// CM19 fail dead-ends, and ensemble quorum dead-ends.
type OutgoingCoverage = (Vec<GraphTerminal>, Vec<FailDeadEnd>, Vec<QuorumFailDeadEnd>);

/// 4. Outgoing coverage — collect terminals (states with no outgoing edge),
///    CM19 fail dead-ends, and ensemble quorum dead-ends; routers must cover
///    every declared route.
pub(super) fn collect_outgoing_coverage(
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
        if node.kind == GraphNodeKind::Deliver {
            // A deliver node routes exactly one step onward — one outgoing
            // edge carrying its pass verdict. Zero or 2+ is a broken graph,
            // not a terminal. `error` edges never carry a verdict, so the
            // infra autowire does not count.
            ensure_deliver_single_edge(node, &outgoing)?;
        }
        // Agent / Check / Gate / Deliver — missing pass or fail is a
        // terminal, not an error.
        if matches!(
            node.kind,
            GraphNodeKind::Agent
                | GraphNodeKind::Check
                | GraphNodeKind::Gate
                | GraphNodeKind::Deliver
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

/// A deliver node's single step onward: exactly one outgoing edge carrying
/// a verdict (`error` edges never do, so the infra autowire does not
/// count). Anything else is a broken graph — refused, like an uncovered
/// router route, rather than reported as a terminal.
fn ensure_deliver_single_edge(
    node: &GraphNodeView<'_>,
    outgoing: &HashMap<&str, Vec<&GraphEdgeCondition>>,
) -> Result<(), String> {
    let count = outgoing
        .get(node.id)
        .map(|conds| {
            conds
                .iter()
                .filter(|c| ***c != GraphEdgeCondition::Error)
                .count()
        })
        .unwrap_or(0);
    if count == 1 {
        return Ok(());
    }
    Err(format!(
        "Deliver node '{}' must have exactly one outgoing edge, got {}.",
        node.id, count
    ))
}
