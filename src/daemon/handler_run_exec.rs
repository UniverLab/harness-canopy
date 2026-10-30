//! MCP handler tools split out of `daemon::handler` (graph run dispatch and run guards).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_run_directive::held_queue_spec;
use crate::daemon::handler_run_prepare::{
    create_launch_sandbox, refuse_blocked_plain_launch, refuse_in_flight_run,
    validate_launch_guards, validate_launch_inputs, DirectedStartOutcome, LaunchInputs,
};
use crate::daemon::handler_validate::resolve_prefix_or_error;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{GraphNodeRun, GraphSpec, GraphSpecStatus, GraphStatus};
use crate::graph_engine::DirectedPlan;
use crate::or_early_return;
use rmcp::model::CallToolResult;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_run_exec, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_run_impl(
        &self,
        params: GraphRunParams,
    ) -> Result<CallToolResult, McpError> {
        let graph_id = match resolve_prefix_or_error(
            &self.db,
            params.graph_id.trim(),
            Database::resolve_graph_id_by_prefix,
            "graph",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let lp = match self.db.get_graph(&graph_id) {
            Ok(Some(w)) => w,
            Ok(None) => return Ok(error_result(&format!("Graph '{}' not found.", graph_id))),
            Err(e) => return Err(internal_error(e.to_string())),
        };

        let start_node_id = params
            .start_node_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        if start_node_id.is_none() {
            let () = or_early_return!(refuse_blocked_plain_launch(&self.db, &graph_id, &lp));
        }
        let () = or_early_return!(refuse_in_flight_run(&self.db, &graph_id));

        let LaunchInputs {
            queue_id,
            idea,
            workdir,
        } = or_early_return!(validate_launch_inputs(&self.db, &graph_id, &params));
        let () = or_early_return!(validate_launch_guards(
            &self.db,
            &graph_id,
            queue_id,
            lp.require_spec_type
        ));

        // (CB22) Synchronous launch validation: the same check the engine's
        // dispatch runs, so the TUI receives "what is missing" immediately
        // instead of a misleading successful background-launch response. A
        // run with no bound specs and no queue is refused here — the TUI
        // must not manufacture a blank spec to fill the gap.
        match self
            .graph_engine
            .empty_launch_check(&graph_id, queue_id, idea.as_deref())
        {
            Ok(Some(message)) => return Ok(error_result(&message)),
            Ok(None) => {}
            Err(e) => return Err(internal_error(e.to_string())),
        }

        let (next_spec_preview, launch_warning) = or_early_return!(
            self.preview_and_dirty_check(
                &graph_id,
                workdir,
                &lp.workdir,
                queue_id,
                start_node_id,
                lp.allow_dirty_start
            )
            .await
        );

        let sandbox = if params.sandbox == Some(true) {
            or_early_return!(create_launch_sandbox(&self.db, &graph_id, workdir, &lp.workdir).await)
        } else {
            None
        };

        let outcome = match start_node_id {
            Some(start) => or_early_return!(self.plan_launch_directed_start(
                &graph_id,
                &next_spec_preview,
                start
            )),
            None => DirectedStartOutcome {
                plan: None,
                limit_note: String::new(),
                directed: None,
            },
        };
        Arc::clone(&self.graph_engine).start_background_run(
            graph_id.clone(),
            queue_id.map(str::to_string),
            workdir.map(str::to_string),
            idea,
            sandbox,
            outcome.directed,
        );
        let report = format!(
            "{}{}",
            outcome.limit_note,
            directed_run_report(outcome.plan.as_ref())
        );
        Ok(success_result(&match launch_warning {
            Some(warning) => format!(
                "Graph '{}' launched in background. WARNING: {}{}",
                graph_id, warning, report
            ),
            None => format!("Graph '{}' launched in background.{}", graph_id, report),
        }))
    }
}

/// Whether `graph_control` accepts relaunching a graph in `status`, factored out as
/// a pure function so the guard (and its wording) can be unit-tested without
/// building a full `TaskTriggerHandler`.
///
/// `graph_control` refuses `failed`/`completed` graphs outright — `graph_control` (or,
/// for a `failed` graph, `graph_control`'s auto-reset-and-resume) is
/// the sanctioned way out, mirroring [`Database::reset_graph`].
pub(crate) fn graph_run_status_guard(graph_id: &str, status: GraphStatus) -> Result<(), String> {
    match status {
        GraphStatus::Running | GraphStatus::Pausing => {
            Err(format!("Graph '{graph_id}' is already running."))
        }
        GraphStatus::Completed | GraphStatus::Failed => Err(
            "Completed or failed graphs cannot be resumed directly; call graph_control reset first, \
             then graph_control run — or, for a failed graph, graph_control schedule_autorun to have it \
             auto-reset and resume once its schedule fires."
                .to_string(),
        ),
        GraphStatus::Draft | GraphStatus::Paused => Ok(()),
    }
}

/// Ground truth for "is this graph actually busy right now": any `graph_runs`
/// row still `running` under it, regardless of what the graph's own `status`
/// column says. Status and a run's lifetime are independent — a sibling
/// node's `graph_report_blocker` can flip status to `paused` while a
/// different node under the same graph keeps executing — so `graph_control` and
/// [`Database::reset_graph`] both consult this (the latter internally, since
/// the scheduler's autorun also calls it directly) instead of trusting
/// status alone. One indexed query (`idx_graph_runs_graph_started`) at
/// dispatch time.
///
/// Returns the first still-`running` row found — an actionable pointer for a
/// human operator (wait or kill), not an exhaustive list.
pub(crate) fn find_in_flight_run(
    db: &Database,
    graph_id: &str,
) -> Result<Option<GraphNodeRun>, String> {
    Ok(db
        .list_running_graph_runs(graph_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next())
}

/// The actionable refusal text for a `graph_control`/`graph_control` call blocked by
/// [`find_in_flight_run`] (or [`Database::reset_graph`]'s own equivalent
/// check): names the node and run still executing, and since when, because
/// the caller's next decision is to wait for it or terminate it.
pub(crate) fn in_flight_run_error(
    graph_id: &str,
    run_id: &str,
    node_name: &str,
    started_at: chrono::DateTime<chrono::Utc>,
) -> String {
    format!(
        "Graph '{graph_id}' has node '{node_name}' (run '{run_id}') still executing, running \
         since {} — wait for it to finish, or terminate it, before retrying.",
        started_at.to_rfc3339()
    )
}

/// `node_id`'s display name, falling back to the raw id if the node row is
/// somehow gone (e.g. deleted out from under a still-running attempt) — used
/// to build [`in_flight_run_error`]'s message from a bare `GraphNodeRun`.
pub(crate) fn node_name_for_error(db: &Database, node_id: &str) -> Result<String, String> {
    Ok(db
        .get_graph_node(node_id)
        .map_err(|e| e.to_string())?
        .map(|node| node.name)
        .unwrap_or_else(|| node_id.to_string()))
}

/// Validate every ensemble (F1) reachable by a `graph_control` call — the graph's
/// own top-level graph, plus the own graph of every spec that could actually
/// run (the graph's bound specs, and a queue's members when `queue_id` is
/// given). A spec with no nodes of its own falls back to the top-level
/// graph at execution time (see `GraphEngine::run_spec`), so it's skipped
/// here rather than double-validated.
pub(crate) fn validate_graph_ensembles_for_run(
    db: &Database,
    graph_id: &str,
    queue_id: Option<&str>,
) -> Result<(), String> {
    let graph_nodes = db
        .list_graph_nodes_for_graph(graph_id)
        .map_err(|e| e.to_string())?;
    let graph_edges = db
        .list_graph_edges_for_graph(graph_id)
        .map_err(|e| e.to_string())?;
    let graph_ensembles = db
        .list_ensembles_for_graph(graph_id)
        .map_err(|e| e.to_string())?;
    crate::domain::validation::validate_ensembles_in_graph(
        &graph_ensembles,
        &graph_nodes,
        &graph_edges,
    )?;

    let mut spec_ids: Vec<String> = db
        .list_graph_specs(graph_id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|spec| spec.id)
        .collect();
    if let Some(queue_id) = queue_id {
        spec_ids.extend(
            db.list_queue_member_spec_ids(queue_id)
                .map_err(|e| e.to_string())?,
        );
    }

    for spec_id in spec_ids {
        let nodes = db.list_graph_nodes(&spec_id).map_err(|e| e.to_string())?;
        if nodes.is_empty() {
            continue;
        }
        let edges = db.list_graph_edges(&spec_id).map_err(|e| e.to_string())?;
        let ensembles = db
            .list_ensembles_for_spec(&spec_id)
            .map_err(|e| e.to_string())?;
        crate::domain::validation::validate_ensembles_in_graph(&ensembles, &nodes, &edges)?;
    }

    Ok(())
}

/// Re-export of the moved `resolve_reported_run` for its test-only
/// callers (via `handler`'s `#[cfg(test)]` re-export).
#[cfg(test)]
pub(crate) use crate::daemon::handler_report_ids::resolve_reported_run;

/// Find and skip `graph_id`'s current spec — its own bound spec, or, for a
/// queue-driven run, the queue member currently in flight. "Current" means
/// resumable ([`GraphSpecStatus::is_resumable`]: `running`, or `interrupted`
/// after a no-verdict block), so `skip_next_spec` applies to the same paused
/// graphs `retry_current_node` does. A queue
/// member's `graph_id` column stays `None` (queue membership never binds it),
/// so `list_graph_specs(graph_id)` alone can't see it (B18): the graph's
/// persisted `active_run_queue_id` is what names the queue to look in instead.
pub(crate) fn handle_skip_next_spec(db: &Database, graph_id: &str) -> Result<(), McpError> {
    let bound_running = db
        .list_graph_specs(graph_id)
        .map_err(internal_error)?
        .into_iter()
        .find(|spec| spec.status.is_resumable());

    // FR6: a failed graph skips its current non-terminal spec, the one
    // `run_from_node` names. A paused queue held back by the dirty-tree guard
    // skips the member it held, never the `failed` predecessor before it.
    let current_spec = match bound_running {
        Some(spec) => spec,
        None => match queue_running_spec(db, graph_id)? {
            Some(spec) => spec,
            None => match held_queue_spec(db, graph_id)? {
                Some(spec) => spec,
                None => resolve_directed_spec(db, graph_id, None).map_err(|_| {
                    McpError::invalid_params(
                        "No running spec found to skip from this paused graph.",
                        None,
                    )
                })?,
            },
        },
    };

    db.update_graph_spec_status(
        &current_spec.id,
        GraphSpecStatus::Skipped,
        None,
        Some(chrono::Utc::now()),
    )
    .map_err(internal_error)?;
    Ok(())
}

/// Validate that a resumable spec exists for `retry_current_node` — `Running`
/// or `Interrupted` (see [`GraphSpecStatus::is_resumable`], CB50), the same
/// lookup shape as [`handle_skip_next_spec`] but only checks, never mutates.
/// When nothing resumable is found, the error names whichever spec IS
/// current (if any) and its status, and points at the verb that does apply —
/// `loop_run` to restart a non-`Failed` spec from its entry node, `loop_reset`
/// for a `Failed` one (FR5) — rather than a bare "not found".
pub(crate) fn handle_retry_current_node(db: &Database, graph_id: &str) -> Result<(), McpError> {
    let bound_specs = db.list_graph_specs(graph_id).map_err(internal_error)?;
    if bound_specs.iter().any(|spec| spec.status.is_resumable()) {
        return Ok(());
    }
    let queue_spec = queue_current_spec(db, graph_id)?;
    if let Some(spec) = &queue_spec {
        if spec.status.is_resumable() {
            return Ok(());
        }
    }

    let current = bound_specs
        .into_iter()
        .find(|spec| {
            !matches!(
                spec.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            )
        })
        .or(queue_spec);

    Err(match current {
        Some(spec) if spec.status == GraphSpecStatus::Failed => McpError::invalid_params(
            format!(
                "Graph '{graph_id}': current spec '{}' is 'failed', not resumable by \
                 graph_control continue. Use `graph_control continue` with mode `run_from_node` to restart \
                 it at a chosen node (reusing earlier nodes' outputs), or graph_control reset to reset it.",
                spec.name
            ),
            None,
        ),
        Some(spec) => McpError::invalid_params(
            format!(
                "Graph '{graph_id}': current spec '{}' is '{}', not resumable by \
                 graph_control continue. Use graph_control run to restart it from its entry node.",
                spec.name,
                spec.status.as_str()
            ),
            None,
        ),
        None => McpError::invalid_params(
            "No running spec found to retry from this paused graph.",
            None,
        ),
    })
}

/// The current member of `graph_id`'s active queue run — the first resumable
/// one (`running`, or `interrupted` after a no-verdict block; see
/// [`GraphSpecStatus::is_resumable`]) — if any. `None` if the graph isn't
/// drawing from a queue, or no member is resumable.
pub(crate) fn queue_running_spec(
    db: &Database,
    graph_id: &str,
) -> Result<Option<GraphSpec>, McpError> {
    let Some(queue_id) = db
        .get_graph(graph_id)
        .map_err(internal_error)?
        .and_then(|lp| lp.active_run_queue_id)
    else {
        return Ok(None);
    };
    for spec_id in db
        .list_queue_member_spec_ids(&queue_id)
        .map_err(internal_error)?
    {
        if let Some(spec) = db.get_graph_spec(&spec_id).map_err(internal_error)? {
            if spec.status.is_resumable() {
                return Ok(Some(spec));
            }
        }
    }
    Ok(None)
}

/// The current member of `graph_id`'s active queue: the first `Running` or
/// `Interrupted` member (either is resumable — CB50), found in queue
/// position order. If none is resumable, falls back to a non-terminal member
/// — preferring `Failed` (the spec a directed `run_from_node` with no
/// `spec_id` restarts, and the spec an FR5 error names) over a still `Pending`
/// one — so callers choosing a directed default spec, or building an error
/// message, always land on the spec worth acting on instead of nothing.
/// `None` if the graph isn't drawing from a queue, or every member is
/// terminal.
pub(crate) fn queue_current_spec(
    db: &Database,
    graph_id: &str,
) -> Result<Option<GraphSpec>, McpError> {
    let Some(queue_id) = db
        .get_graph(graph_id)
        .map_err(internal_error)?
        .and_then(|lp| lp.active_run_queue_id)
    else {
        return Ok(None);
    };
    let mut failed: Option<GraphSpec> = None;
    let mut pending: Option<GraphSpec> = None;
    for spec_id in db
        .list_queue_member_spec_ids(&queue_id)
        .map_err(internal_error)?
    {
        let Some(spec) = db.get_graph_spec(&spec_id).map_err(internal_error)? else {
            continue;
        };
        if spec.status.is_resumable() {
            // Running or Interrupted — resume the in-flight spec.
            return Ok(Some(spec));
        }
        if spec.status == GraphSpecStatus::Failed {
            failed.get_or_insert(spec);
        } else if spec.status == GraphSpecStatus::Pending {
            pending.get_or_insert(spec);
        }
    }
    // No live spec: a Failed member is a more useful "current" spec to name
    // (and the default a directed run_from_node without spec_id targets) than
    // a still-Pending one.
    Ok(failed.or(pending))
}

/// The spec a `run_from_node` directed run targets: the explicit `spec_id` when
/// given (a bound spec, or a member of the graph's active queue), else the
/// graph's current spec — the first resumable bound spec, else the first
/// non-terminal one, else the queue's current member.
pub(crate) fn resolve_directed_spec(
    db: &Database,
    graph_id: &str,
    spec_id: Option<&str>,
) -> Result<GraphSpec, String> {
    let bound = db.list_graph_specs(graph_id).map_err(|e| e.to_string())?;
    if let Some(spec_ref) = spec_id.map(str::trim).filter(|s| !s.is_empty()) {
        let lp = db.get_graph(graph_id).map_err(|e| e.to_string())?;
        let workdir = lp.as_ref().map(|graph| graph.workdir.as_str());
        // Accept a full id, an unambiguous id prefix, or a spec code —
        // resolved within the graph's workdir like every other spec tool —
        // then check membership against the graph's bound specs and, for a
        // queue-driven graph, the queue being run.
        let resolved = db
            .resolve_spec_id_by_prefix(spec_ref, workdir)
            .map_err(|e| e.to_string())?;
        let Some(resolved) = resolved else {
            return Err(format!(
                "Spec '{spec_ref}' does not belong to graph '{graph_id}'."
            ));
        };
        if let Some(spec) = bound.iter().find(|s| s.id == resolved) {
            return Ok(spec.clone());
        }
        let Some(queue_id) = lp.and_then(|graph| graph.active_run_queue_id) else {
            return Err(format!(
                "Spec '{spec_ref}' does not belong to graph '{graph_id}'."
            ));
        };
        for member_id in db
            .list_queue_member_spec_ids(&queue_id)
            .map_err(|e| e.to_string())?
        {
            if member_id != resolved {
                continue;
            }
            if let Some(spec) = db.get_graph_spec(&resolved).map_err(|e| e.to_string())? {
                return Ok(spec);
            }
        }
        return Err(format!(
            "Spec '{spec_ref}' does not belong to graph '{graph_id}'."
        ));
    }
    if let Some(spec) = bound.iter().find(|s| s.status.is_resumable()).or_else(|| {
        bound.iter().find(|s| {
            !matches!(
                s.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            )
        })
    }) {
        return Ok(spec.clone());
    }
    if let Some(spec) = queue_current_spec(db, graph_id).map_err(|e| e.to_string())? {
        return Ok(spec);
    }
    Err(format!(
        "Graph '{graph_id}' has no current spec; pass spec_id to choose a completed one."
    ))
}

pub(crate) fn graph_run_json(run: &crate::domain::graphs::GraphNodeRun) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "graph_id": run.graph_id,
        "spec_id": run.spec_id,
        "node_id": run.node_id,
        "status": run.status.as_str(),
        "input": run.input,
        "output": run.output,
        "started_at": run.started_at.to_rfc3339(),
        "completed_at": run.completed_at.map(|value| value.to_rfc3339()),
        "iteration": run.iteration,
    })
}

pub(crate) fn graph_run_blocker(run: &crate::domain::graphs::GraphNodeRun) -> Option<String> {
    run.output
        .as_ref()
        .and_then(|output| output.get("blocker"))
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

/// Human report for a directed `graph_control`/`graph_control run_from_node`
/// response: the start node, the skipped nodes, the reused outputs, the
/// counter restart, and whether `{{spec_start_head}}` is reused or fresh.
pub(crate) fn directed_run_report(plan: Option<&DirectedPlan>) -> String {
    let Some(plan) = plan else {
        return String::new();
    };
    let skipped = if plan.skipped_nodes.is_empty() {
        "none".to_string()
    } else {
        plan.skipped_nodes.join(", ")
    };
    let reused = if plan.reused_outputs.is_empty() {
        "none".to_string()
    } else {
        plan.reused_outputs
            .iter()
            .map(|(name, run_id)| format!("{name} ({run_id})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let head = match plan.start_head.as_deref() {
        Some(head) => format!("reusing the recorded {{{{spec_start_head}}}} ({head})"),
        None => "no recorded {{spec_start_head}}; a fresh one will be captured".to_string(),
    };
    format!(
        " Directed run: spec '{}' starts at {}; skipped {skipped} node(s); reused outputs: {reused}; \
         iteration counters restarted at 1; {head}.",
        plan.spec_name, plan.start_label
    )
}
