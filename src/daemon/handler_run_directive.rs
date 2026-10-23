//! Operator directives on a graph's paused or failed spec: the human ruling
//! that clears the per-spec execution limit, the directed restart a
//! `retry_current_node` becomes, and the refusal for statuses it cannot act
//! on. Split out of `handler_run_exec` to keep that module within budget.

use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_run_exec::{
    directed_run_report, find_in_flight_run, in_flight_run_error, node_name_for_error,
    resolve_directed_spec,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphSpec, GraphSpecStatus, GraphStatus};
use crate::graph_engine::{DirectedStart, GraphEngine};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

/// FR1: an explicit operator directive is the human ruling the per-spec
/// execution limit waits for. Zero the spec's count before its run starts
/// (the worktree and status are untouched) and say so in the response.
pub(crate) fn reset_execution_limit(
    db: &Database,
    spec_id: &str,
    spec_name: &str,
) -> Result<String, McpError> {
    db.clear_graph_spec_cross_run_attempts(spec_id)
        .map_err(internal_error)?;
    Ok(format!(" Execution limit reset for spec '{spec_name}'."))
}

/// The node the spec's most recent recorded run sits on: where a pause, or
/// daemon recovery after a reboot mid-node, left the spec.
pub(crate) fn spec_current_node(db: &Database, spec_id: &str) -> Result<Option<String>, McpError> {
    let runs = db
        .list_graph_runs_for_spec(spec_id)
        .map_err(internal_error)?;
    Ok(runs.last().map(|run| run.node_id.clone()))
}

/// The directed restart a `retry_current_node` becomes: on a `failed` graph,
/// the failed spec at the node it failed on (FR6); on a `paused` graph whose
/// current spec is `pending` with a recorded current node, that node (FR7).
/// `None` leaves the resumable-spec path in charge.
pub(crate) fn retry_restart_target(
    db: &Database,
    lp: &Graph,
) -> Result<Option<(GraphSpec, String)>, McpError> {
    let Ok(spec) = resolve_directed_spec(db, &lp.id, None) else {
        return Ok(None);
    };
    let restart = lp.status == GraphStatus::Failed || spec.status == GraphSpecStatus::Pending;
    if !restart {
        return Ok(None);
    }
    Ok(spec_current_node(db, &spec.id)?.map(|node| (spec, node)))
}

/// A directed run at `node_id` of `spec`, the same path `run_from_node` takes:
/// earlier outputs are reused and `{{spec_start_head}}` is kept or captured
/// fresh exactly as that directed run would. The limit is reset first.
pub(crate) fn restart_at_node(
    engine: &Arc<GraphEngine>,
    db: &Database,
    graph_id: &str,
    spec: &GraphSpec,
    node_id: String,
) -> Result<CallToolResult, McpError> {
    if let Some(run) = find_in_flight_run(db, graph_id).map_err(internal_error)? {
        let node_name = node_name_for_error(db, &run.node_id).map_err(internal_error)?;
        return Ok(error_result(&in_flight_run_error(
            graph_id,
            &run.id,
            &node_name,
            run.started_at,
        )));
    }
    let plan = match engine.plan_directed_run(graph_id, &spec.id, &node_id) {
        Ok(plan) => plan,
        Err(message) => return Ok(error_result(&message)),
    };
    let limit = reset_execution_limit(db, &spec.id, &spec.name)?;
    Arc::clone(engine).resume_background(
        graph_id.to_string(),
        Some(DirectedStart {
            spec_id: spec.id.clone(),
            node_arg: node_id,
        }),
    );
    Ok(success_result(&format!(
        "Graph '{graph_id}' resumed with action 'retry_current_node'.{limit}{}",
        directed_run_report(Some(&plan))
    )))
}

/// Refusal for `graph_continue` on a graph that is neither `paused` nor
/// `failed`: names the status and the ways out.
pub(crate) fn not_continuable_refusal(graph_id: &str, status: GraphStatus) -> String {
    format!(
        "Graph '{graph_id}' is '{}', not paused or failed, so graph_continue cannot act on it. \
         Use `run_from_node` with `spec_id`, or `graph_reset` with `specs: [<spec id>]` then \
         `graph_run`.",
        status.as_str()
    )
}
