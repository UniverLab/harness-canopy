//! Operator directives on a graph's paused or failed spec: the human ruling
//! that clears the per-spec execution limit, the directed restart a
//! `retry_current_node` becomes, and the refusal for statuses it cannot act
//! on. Split out of `handler_run_exec` to keep that module within budget.

use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_run_exec::{
    directed_run_report, find_in_flight_run, in_flight_run_error, node_name_for_error,
    queue_running_spec, resolve_directed_spec,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphSpec, GraphSpecStatus, GraphStatus};
use crate::graph_engine::{DirectedStart, GraphEngine};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

/// FR1: the sentence every explicit operator directive's response carries.
/// The zeroing itself happens where a directed run starts
/// ([`GraphEngine::run_graph_dispatch`]), so every directed entry point gets it.
pub(crate) fn limit_reset_note(spec_name: &str) -> String {
    format!(" Execution limit reset for spec '{spec_name}'.")
}

/// FR1 for a non-directed directive (a plain `retry_current_node`): zero the
/// spec's count now, then say so. The worktree and status are untouched.
pub(crate) fn reset_execution_limit(
    db: &Database,
    spec_id: &str,
    spec_name: &str,
) -> Result<String, McpError> {
    db.clear_graph_spec_cross_run_attempts(spec_id)
        .map_err(internal_error)?;
    Ok(limit_reset_note(spec_name))
}

/// The member a paused queue-driven graph is held on when nothing is in flight:
/// the spec its next dispatch would start, which is where the dirty-tree guard
/// leaves it (`pending`). `None` unless the graph is paused, draws from a queue,
/// and has no resumable member.
pub(crate) fn held_queue_spec(
    db: &Database,
    graph_id: &str,
) -> Result<Option<GraphSpec>, McpError> {
    let Some(lp) = db.get_graph(graph_id).map_err(internal_error)? else {
        return Ok(None);
    };
    let Some(queue_id) = lp.active_run_queue_id.as_deref() else {
        return Ok(None);
    };
    if lp.status != GraphStatus::Paused || queue_running_spec(db, graph_id)?.is_some() {
        return Ok(None);
    }
    let Some(spec_id) = db
        .queue_next_pending_spec_id(queue_id)
        .map_err(internal_error)?
    else {
        return Ok(None);
    };
    db.get_graph_spec(&spec_id).map_err(internal_error)
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
    // On a paused queue the spec to restart is the member the guard held back;
    // `resolve_directed_spec` would prefer a `failed` predecessor instead.
    let Some(spec) =
        held_queue_spec(db, &lp.id)?.or_else(|| resolve_directed_spec(db, &lp.id, None).ok())
    else {
        return Ok(None);
    };
    let restart = lp.status == GraphStatus::Failed || spec.status == GraphSpecStatus::Pending;
    if !restart {
        return Ok(None);
    }
    Ok(spec_current_node(db, &spec.id)?.map(|node| (spec, node)))
}

/// Persist an explicit `graph_control continue` workdir for the resumed run
/// (`None` keeps the run's workdir untouched). Called only on resume paths,
/// after every refusal check, so a refused continue never mutates the run's
/// workdir — a value written by a refused call would silently steer the
/// next resume at the wrong checkout.
pub(crate) fn persist_continue_workdir(
    db: &Database,
    graph_id: &str,
    workdir: Option<&str>,
) -> Result<(), McpError> {
    if let Some(workdir) = workdir {
        db.set_graph_active_run_workdir(graph_id, Some(workdir))
            .map_err(internal_error)?;
    }
    Ok(())
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
    workdir: Option<&str>,
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
    let limit = limit_reset_note(&spec.name);
    persist_continue_workdir(db, graph_id, workdir)?;
    Arc::clone(engine).resume_background(
        graph_id.to_string(),
        Some(DirectedStart {
            spec_id: spec.id.clone(),
            node_arg: node_id,
        }),
    );
    Ok(success_result(&format!(
        "Graph '{graph_id}' resumed with mode 'retry_current_node'.{limit}{}",
        directed_run_report(Some(&plan))
    )))
}

/// Refusal for `graph_control` on a graph that is neither `paused` nor
/// `failed`: names the status and the ways out.
pub(crate) fn not_continuable_refusal(graph_id: &str, status: GraphStatus) -> String {
    format!(
        "Graph '{graph_id}' is '{}', not paused or failed, so graph_control continue cannot act on it. \
         Use `graph_control continue` with mode `run_from_node` and `spec_id`, or `graph_control reset` \
         with `specs: [<spec id>]` then `graph_control run`.",
        status.as_str()
    )
}
