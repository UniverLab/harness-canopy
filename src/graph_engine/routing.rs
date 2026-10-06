use super::*;

pub(crate) fn find_entry_node(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    spec_name: &str,
) -> Result<String> {
    let incoming = edges
        .iter()
        .map(|edge| edge.to_node.as_str())
        .collect::<HashSet<_>>();
    let entry_nodes = nodes
        .iter()
        .filter(|node| !incoming.contains(node.id.as_str()))
        .collect::<Vec<_>>();

    match entry_nodes.as_slice() {
        [entry] => Ok(entry.id.clone()),
        // Every node has an incoming edge: the graph is a retry cycle (e.g.
        // implement <-> review). There is no source node, so fall back to the
        // designated start — the node with the lowest position.
        [] => nodes
            .iter()
            .min_by_key(|node| node.position)
            .map(|node| node.id.clone())
            .ok_or_else(|| anyhow!("Spec '{}' has no nodes.", spec_name)),
        _ => bail!("Spec '{}' has multiple entry nodes.", spec_name),
    }
}

/// Result of [`select_next_step`]: the next cursor plus the edge condition
/// that matched (needed for B43 lifecycle logging).
#[derive(Debug)]
pub(crate) struct StepSelection {
    pub(crate) cursor: SpecCursor,
    pub(crate) edge_condition: GraphEdgeCondition,
}

/// Resolve the next graph step from `from_node`'s outgoing edges matching
/// `status`. Ordinarily a single matching edge (or several identical-target
/// edges) resolves to [`SpecCursor::Node`]. Multiple *distinct* targets are
/// ambiguous — unless they are exactly one ensemble's full member set, in
/// which case this is F1's fan-out point and resolves to
/// [`SpecCursor::Ensemble`] instead of erroring.
pub(crate) fn select_next_step(
    edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
    from_node: &str,
    status: GraphRunStatus,
) -> Result<Option<StepSelection>> {
    let matching = edges
        .iter()
        .filter(|edge| edge.from_node == from_node)
        .filter(|edge| match status {
            GraphRunStatus::Pass => {
                edge.condition == GraphEdgeCondition::Pass
                    || edge.condition == GraphEdgeCondition::Always
            }
            GraphRunStatus::Fail | GraphRunStatus::Interrupted => {
                edge.condition == GraphEdgeCondition::Fail
                    || edge.condition == GraphEdgeCondition::Always
            }
            GraphRunStatus::Error => edge.condition == GraphEdgeCondition::Error,
            GraphRunStatus::Running => false,
        })
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [] => Ok(None),
        [edge] => Ok(Some(StepSelection {
            cursor: SpecCursor::Node(edge.to_node.clone()),
            edge_condition: edge.condition.clone(),
        })),
        _ => {
            let distinct_targets = matching
                .iter()
                .map(|edge| edge.to_node.as_str())
                .collect::<HashSet<_>>();
            if distinct_targets.len() == 1 {
                let to_node = *distinct_targets.iter().next().expect("len == 1");
                return Ok(Some(StepSelection {
                    cursor: SpecCursor::Node(to_node.to_string()),
                    edge_condition: matching[0].condition.clone(),
                }));
            }
            for details in ensembles {
                let member_ids: HashSet<&str> = details
                    .members
                    .iter()
                    .map(|member| member.node_id.as_str())
                    .collect();
                if member_ids == distinct_targets {
                    return Ok(Some(StepSelection {
                        cursor: SpecCursor::Ensemble(details.ensemble.id.clone()),
                        edge_condition: matching[0].condition.clone(),
                    }));
                }
            }
            bail!("Node '{}' has ambiguous outgoing edges.", from_node)
        }
    }
}

/// CM2: like [`select_next_step`], but matches edges by an explicit condition
/// rather than by run status. Used to find `Error` edges after an
/// infrastructure failure — the engine knows there was an infra crash, but
/// the agent declared nothing, so only the engine can resolve this edge.
pub(crate) fn select_next_step_with_condition(
    edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
    from_node: &str,
    condition: &GraphEdgeCondition,
) -> Result<Option<StepSelection>> {
    let matching = edges
        .iter()
        .filter(|edge| edge.from_node == from_node)
        .filter(|edge| edge.condition == *condition)
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [] => Ok(None),
        [edge] => Ok(Some(StepSelection {
            cursor: SpecCursor::Node(edge.to_node.clone()),
            edge_condition: edge.condition.clone(),
        })),
        _ => {
            let distinct_targets = matching
                .iter()
                .map(|edge| edge.to_node.as_str())
                .collect::<HashSet<_>>();
            if distinct_targets.len() == 1 {
                let to_node = *distinct_targets.iter().next().expect("len == 1");
                return Ok(Some(StepSelection {
                    cursor: SpecCursor::Node(to_node.to_string()),
                    edge_condition: matching[0].condition.clone(),
                }));
            }
            for details in ensembles {
                let member_ids: HashSet<&str> = details
                    .members
                    .iter()
                    .map(|member| member.node_id.as_str())
                    .collect();
                if member_ids == distinct_targets {
                    return Ok(Some(StepSelection {
                        cursor: SpecCursor::Ensemble(details.ensemble.id.clone()),
                        edge_condition: matching[0].condition.clone(),
                    }));
                }
            }
            bail!(
                "Node '{}' has ambiguous outgoing '{}' edges.",
                from_node,
                condition.as_str()
            )
        }
    }
}

/// Resolve a router node's next graph step: the edge out of `from_node`
/// whose declared route matches `route_label` exactly (see
/// [`GraphEdgeCondition::Route`]). Used instead of [`select_next_step`] only
/// when the node just executed is a [`GraphNodeKind::Router`] that finished
/// `Pass` — a router's own verdict carries no notion of success/failure, only
/// a choice among its declared routes, so Pass/Fail/Always-conditioned edges
/// never apply here. A failed router (spawn failure/timeout) instead falls
/// through to `select_next_step` like any other node, per its fail-edge
/// contract.
pub(crate) fn select_router_step(
    edges: &[GraphEdge],
    from_node: &str,
    route_label: &str,
) -> Result<Option<StepSelection>> {
    let matching = edges
        .iter()
        .filter(|edge| edge.from_node == from_node)
        .filter(|edge| edge.condition.route_label() == Some(route_label))
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [] => Ok(None),
        [edge] => Ok(Some(StepSelection {
            cursor: SpecCursor::Node(edge.to_node.clone()),
            edge_condition: edge.condition.clone(),
        })),
        _ => {
            let distinct_targets = matching
                .iter()
                .map(|edge| edge.to_node.as_str())
                .collect::<HashSet<_>>();
            if distinct_targets.len() == 1 {
                let to_node = *distinct_targets.iter().next().expect("len == 1");
                return Ok(Some(StepSelection {
                    cursor: SpecCursor::Node(to_node.to_string()),
                    edge_condition: matching[0].condition.clone(),
                }));
            }
            bail!(
                "Router node '{}' has ambiguous outgoing edges for route '{}'.",
                from_node,
                route_label
            )
        }
    }
}

/// Every node id belonging to a cursor step — one for [`SpecCursor::Node`],
/// or every member plus the join for [`SpecCursor::Ensemble`]. Used to reap
/// stale `running` rows (B12) across a whole ensemble fan-out, not just one
/// node.
pub(crate) fn cursor_node_ids(cursor: &SpecCursor, ensembles: &[EnsembleDetails]) -> Vec<String> {
    match cursor {
        SpecCursor::Node(node_id) => vec![node_id.clone()],
        SpecCursor::Ensemble(ensemble_id) => ensembles
            .iter()
            .find(|details| &details.ensemble.id == ensemble_id)
            .map(|details| {
                details
                    .members
                    .iter()
                    .map(|member| member.node_id.clone())
                    .chain(std::iter::once(details.ensemble.join_node_id.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Verbatim failure reason for an activity line: prefers the structured
/// `error`/`failure_kind` fields the engine itself stamps (`is_infra_crash`
/// family — see `src/graph_engine.rs:4309-4327`), falls back to the node's
/// own summary.
pub(crate) fn failure_reason_text(output: &serde_json::Value, summary: &str) -> Option<String> {
    if let Some(reason) = output.get("reason").and_then(serde_json::Value::as_str) {
        return Some(reason.to_string());
    }
    if let Some(err) = output.get("error").and_then(serde_json::Value::as_str) {
        return Some(err.to_string());
    }
    if let Some(kind) = output
        .get("failure_kind")
        .and_then(serde_json::Value::as_str)
    {
        return Some(kind.to_string());
    }
    if summary.trim().is_empty() {
        None
    } else {
        Some(summary.to_string())
    }
}

/// First line of `failure_reason_text(output, summary)` (or `summary`),
/// trimmed and truncated to 200 chars; never empty (falls back to `summary`
/// or "no reason recorded"). Used for the per-member `reason` in a
/// no-verdict join output.
pub(crate) fn no_verdict_reason(output: &Value, summary: &str) -> String {
    let text = failure_reason_text(output, summary).unwrap_or_else(|| summary.to_string());
    let first = text.lines().next().unwrap_or("").trim();
    let fallback = if summary.trim().is_empty() {
        "no reason recorded".to_string()
    } else {
        summary.lines().next().unwrap_or("").trim().to_string()
    };
    let chosen = if first.is_empty() {
        fallback
    } else {
        first.to_string()
    };
    let mut out: String = chosen.chars().take(200).collect();
    out = out.trim().to_string();
    if out.is_empty() {
        "no reason recorded".to_string()
    } else {
        out
    }
}

/// Human-readable label for a cursor step, for failure summaries.
pub(crate) fn cursor_label(cursor: &SpecCursor, ensembles: &[EnsembleDetails]) -> String {
    match cursor {
        SpecCursor::Node(node_id) => format!("node '{node_id}'"),
        SpecCursor::Ensemble(ensemble_id) => ensembles
            .iter()
            .find(|details| &details.ensemble.id == ensemble_id)
            .map(|details| format!("ensemble '{}'", details.ensemble.name))
            .unwrap_or_else(|| format!("ensemble '{ensemble_id}'")),
    }
}

/// Reason recorded on a node run terminated because a newer attempt at the
/// same node superseded it (B42). One of several reasons
/// [`run_was_terminated_out_of_band`] recognises — see that function's doc
/// for why every one of them is treated identically.
pub(crate) const SUPERSEDE_REASON: &str = "superseded by a new attempt at this node";

/// Whether `run` was terminated by the engine out from under the dispatch
/// that owned it — a same-node supersede ([`SUPERSEDE_REASON`], B42), a
/// concurrent `graph_reset`, `graph_pause`, iteration-budget exhaustion, or
/// `fail_graph`'s own sweep — rather than a genuine node outcome (a clean
/// exit, or a self-report via `graph_complete_node`). Every one of those
/// paths is pure engine bookkeeping, not a node failure: the dispatch that
/// owned the run must recognise it and stop silently here, routing down no
/// edge, failing nothing, and completing nothing (see its use in
/// [`GraphEngine::run_spec`]) — exactly what let a stale dispatch's late
/// completion route a fail edge and take down a healthy sibling dispatch on
/// 2026-08-05.
///
/// Recognised by the `{ "terminated": true, "reason": … }` marker every one
/// of those paths writes via [`terminate_run_row`] (or, for `graph_reset`,
/// the identically-shaped write in [`crate::db::Database::reset_graph`]) —
/// matched on the `terminated` key alone, not a specific reason string, so
/// nothing that terminates a run out-of-band can be missed here. A genuine
/// agent output can never be mistaken for one: self-reports never set this
/// key.
pub(crate) fn run_was_terminated_out_of_band(run: &GraphNodeRun) -> bool {
    let Some(output) = run.output.as_ref() else {
        return false;
    };
    output.get("terminated").and_then(Value::as_bool) == Some(true)
}

/// Best-effort termination (B12) of `run`'s OS process, if it still has one
/// recorded, and finalization of its DB row as `Fail`. Free-function core of
/// [`GraphEngine::terminate_run`] — also used by ensemble member tasks, which
/// don't have a `&GraphEngine` to call the method on.
pub(crate) fn terminate_run_row(db: &Database, run: &GraphNodeRun, reason: &str) {
    tracing::info!(
        run_id = %run.id,
        node_id = %run.node_id,
        reason,
        "node run terminated"
    );
    if let Some(pid) = run.pid {
        crate::daemon::process::terminate_process_group_async(pid, KILL_GRACE);
    }
    let _ = db.update_graph_run_result(
        &run.id,
        GraphRunStatus::Fail,
        Some(&serde_json::json!({ "terminated": true, "reason": reason })),
        Some(chrono::Utc::now()),
    );
}

/// `"platform #N"` or `"platform/model #N"` (1-based `N` from the member's
/// position) — the label used in an ensemble's consolidated
/// `"## <label> [pass|fail]"` sections and in the TUI's collapsed ensemble
/// view. The position suffix is always included, not just when it would
/// disambiguate: a multi-angle panel commonly runs several members on the
/// same platform/model with only their `prompt_override` differing, so
/// platform/model alone can name the same label for every section — the
/// position is what actually attributes a section to one member.
pub(crate) fn member_label(member: &EnsembleMember) -> String {
    let base = match member.model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => format!("{}/{}", member.platform, model),
        _ => member.platform.clone(),
    };
    format!("{base} #{}", member.position + 1)
}

/// The human-readable text to carry into an ensemble's consolidated doc for
/// one member's output — its stdout when it produced any, the recorded
/// error when it didn't, else the raw output JSON.
pub(crate) fn member_output_text(output: &Value) -> String {
    if let Some(stdout) = output.get("stdout").and_then(Value::as_str) {
        if !stdout.trim().is_empty() {
            return stdout.to_string();
        }
    }
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return format!("(error: {error})");
    }
    serde_json::to_string_pretty(output).unwrap_or_default()
}

pub(crate) fn should_advance_to_next_spec(node: &GraphNode, status: GraphRunStatus) -> bool {
    let route_key = match status {
        GraphRunStatus::Pass => "pass_route",
        GraphRunStatus::Fail | GraphRunStatus::Interrupted => "fail_route",
        GraphRunStatus::Running | GraphRunStatus::Error => return false,
    };

    node.kind == GraphNodeKind::Gate
        && node
            .config
            .get(route_key)
            .and_then(Value::as_str)
            .is_some_and(|route| route == "next_spec")
}
