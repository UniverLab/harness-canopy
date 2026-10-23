//! MCP handler tools split out of `daemon::handler` (graph run dispatch and run guards).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_queues::{validate_queue_exists, validate_queue_not_consumed};
use crate::daemon::handler_read::build_graph_summary_json;
use crate::daemon::handler_run_directive::reset_execution_limit;
use crate::daemon::handler_validate::{resolve_prefix_or_error, validate_absolute_dir};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{
    GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
};
use crate::graph_engine::DirectedPlan;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_run_exec, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_run",
        description = "Run a graph in the background, spec by spec. With `queue_id`, runs the queue's pending specs (in queue order) through the graph instead of the graph's own bound specs. `workdir` overrides the graph's workdir for this run only. `idea` is free-form text fed to nodes as `{{spec_content}}` when the graph has no bound specs and no queue (mutually exclusive with `queue_id`). With `start_node_id`, start the first spec executed at that graph node (id, id prefix, node name, or ensemble name), reusing earlier nodes' recorded outputs; later specs begin at Start as usual."
    )]
    pub(crate) async fn graph_run(
        &self,
        Parameters(params): Parameters<GraphRunParams>,
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

        // A directed run is the operator's way into a completed/failed
        // graph, so its status guard and blocked-graph refusal apply only to a
        // plain launch. The in-flight check below always applies.
        if start_node_id.is_none() {
            if let Err(message) = graph_run_status_guard(&graph_id, lp.status) {
                return Ok(error_result(&message));
            }

            // C19: a graph paused with an active blocker (whether from
            // `graph_report_blocker` or from a spec exceeding its cross-run
            // attempt budget) needs a human, not another relaunch —
            // `graph_run_status_guard` alone still accepts `Paused` (that's the
            // normal resume-after-`graph_pause` path), so this is a second,
            // narrower check on top of it. `graph_reset` always clears the
            // graph's own status back to `draft` regardless of which specs it
            // targeted, so this only ever refuses the exact window FR4 asks
            // for: still-`paused`-and-blocked, not yet reset.
            if lp.status == GraphStatus::Paused {
                let summary = build_graph_summary_json(&self.db, &lp)?;
                if let Some(blocker) = summary.get("blocker").and_then(|v| v.as_str()) {
                    return Ok(error_result(&format!(
                        "Graph '{}' is blocked and cannot be relaunched via graph_run: {blocker}. \
                         Resolve it, then graph_reset the affected spec (naming it explicitly \
                         clears its cross-run attempt count) before relaunching.",
                        graph_id
                    )));
                }
            }
        }

        if let Some(run) = find_in_flight_run(&self.db, &graph_id).map_err(internal_error)? {
            let node_name = node_name_for_error(&self.db, &run.node_id).map_err(internal_error)?;
            return Ok(error_result(&in_flight_run_error(
                &graph_id,
                &run.id,
                &node_name,
                run.started_at,
            )));
        }

        let queue_id = params
            .queue_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());

        let idea = params
            .idea
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        if idea.is_some() && queue_id.is_some() {
            return Ok(error_result(
                "`idea` and `queue_id` are mutually exclusive: an idea feeds {{spec_content}} directly, a queue provides its own specs.",
            ));
        }

        if let Some(queue_id) = queue_id {
            if let Err(e) = validate_queue_exists(&self.db, queue_id) {
                return Ok(error_result(&e));
            }
            if let Err(e) = validate_queue_not_consumed(&self.db, queue_id, &graph_id) {
                return Ok(error_result(&e));
            }
        }

        let workdir = params
            .workdir
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(workdir) = workdir {
            if let Err(e) = validate_absolute_dir(workdir) {
                return Ok(error_result(&e));
            }
        }

        if let Err(e) = validate_graph_ensembles_for_run(&self.db, &graph_id, queue_id) {
            return Ok(error_result(&e));
        }

        // SPECCODE: opt-in enforcement — a graph that demands spec types
        // refuses to launch when a spec it would actually execute (a bound
        // pending spec, or a pending queue member) still has none, naming it.
        if lp.require_spec_type {
            if let Err(message) = validate_specs_have_types_for_run(&self.db, &graph_id, queue_id) {
                return Ok(error_result(&message));
            }
        }

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

        let effective_workdir = workdir.unwrap_or(&lp.workdir);
        // With `start_node_id`, the preview must be the first spec a
        // FRESH dispatch executes (the directed target — see
        // [`GraphEngine::preview_directed_run_spec_id`]); without it, the
        // running-member-first preview keeps feeding the dirty-start
        // exclusion as before.
        let next_spec_preview = if start_node_id.is_some() {
            self.graph_engine
                .preview_directed_run_spec_id(&graph_id, queue_id)
        } else {
            self.graph_engine.preview_next_spec_id(&graph_id, queue_id)
        }
        .map_err(internal_error)?;
        let mut launch_warning: Option<String> = None;
        match self
            .graph_engine
            .dirty_start_check(
                &graph_id,
                effective_workdir,
                next_spec_preview.as_deref(),
                lp.allow_dirty_start,
            )
            .await
            .map_err(internal_error)?
        {
            Some(notice) if notice.refuse => {
                return Ok(error_result(&format!(
                    "Refusing to launch graph '{}': {}",
                    graph_id, notice.message
                )));
            }
            Some(notice) => launch_warning = Some(notice.message),
            None => {}
        }

        let sandbox = if params.sandbox == Some(true) {
            // The instruction file is per-worktree and the worktree is shared
            // across the graph's nodes, so it must carry the *first* agent
            // node's platform filename (CLAUDE.md / GEMINI.md / …) — a
            // hardcoded "opencode" would write AGENTS.md and leave a
            // claude/gemini node with no protocol file at all.
            let cli_name = {
                let details = self.db.get_graph_details(&graph_id).ok().flatten();
                details
                    .as_ref()
                    .and_then(|d| {
                        let mut agents: Vec<&GraphNode> = d
                            .graph_nodes
                            .iter()
                            .chain(d.specs.iter().flat_map(|s| s.nodes.iter()))
                            .filter(|n| n.kind == GraphNodeKind::Agent)
                            .collect();
                        agents.sort_by_key(|n| n.position);
                        agents.iter().find_map(|n| {
                            n.config
                                .get("platform")
                                .or_else(|| n.config.get("cli"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                        })
                    })
                    .unwrap_or_else(|| "opencode".to_string())
            };
            let protocol = crate::domain::sandbox::canopy_protocol_block();
            let effective_workdir = workdir.unwrap_or(&lp.workdir);
            match crate::domain::sandbox::create_sandbox(effective_workdir, &cli_name, protocol)
                .await
            {
                Ok(sb) => {
                    let _ = self.db.insert_sandbox_run(&sb, "graph", &graph_id);
                    Some(sb)
                }
                Err(e) => {
                    return Ok(error_result(&format!("Failed to create sandbox: {e}")));
                }
            }
        } else {
            None
        };

        // Resolve the directed start synchronously so a bad node or a
        // missing reachable output refuses the launch before any dispatch. The
        // target spec is the first spec this run would execute (the same
        // preview the dirty-start check above used).
        let mut directed_plan = None;
        let mut limit_note = String::new();
        let directed = match start_node_id {
            Some(start) => {
                let Some(spec_id) = next_spec_preview.clone() else {
                    return Ok(error_result(&format!(
                        "Graph '{}' has no spec to start a directed run at.",
                        graph_id
                    )));
                };
                match self
                    .graph_engine
                    .plan_directed_run(&graph_id, &spec_id, start)
                {
                    Ok(plan) => {
                        // FR1: the directed start is the operator's ruling on
                        // the execution limit; clear it before the run starts.
                        limit_note = reset_execution_limit(&self.db, &spec_id, &plan.spec_name)?;
                        directed_plan = Some(plan);
                        Some(crate::graph_engine::DirectedStart {
                            spec_id,
                            node_arg: start.to_string(),
                        })
                    }
                    Err(message) => return Ok(error_result(&message)),
                }
            }
            None => None,
        };

        Arc::clone(&self.graph_engine).start_background_run(
            graph_id.clone(),
            queue_id.map(str::to_string),
            workdir.map(str::to_string),
            idea,
            sandbox,
            directed,
        );
        let report = format!(
            "{limit_note}{}",
            directed_run_report(directed_plan.as_ref())
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

/// Whether `graph_run` accepts relaunching a graph in `status`, factored out as
/// a pure function so the guard (and its wording) can be unit-tested without
/// building a full `TaskTriggerHandler`.
///
/// `graph_run` refuses `failed`/`completed` graphs outright — `graph_reset` (or,
/// for a `failed` graph, `graph_schedule_autorun`'s auto-reset-and-resume) is
/// the sanctioned way out, mirroring [`Database::reset_graph`].
pub(crate) fn graph_run_status_guard(graph_id: &str, status: GraphStatus) -> Result<(), String> {
    match status {
        GraphStatus::Running | GraphStatus::Pausing => {
            Err(format!("Graph '{graph_id}' is already running."))
        }
        GraphStatus::Completed | GraphStatus::Failed => Err(
            "Completed or failed graphs cannot be resumed directly; call graph_reset first, \
             then graph_run — or, for a failed graph, graph_schedule_autorun to have it \
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
/// different node under the same graph keeps executing — so `graph_run` and
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

/// The actionable refusal text for a `graph_run`/`graph_reset` call blocked by
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

/// Validate every ensemble (F1) reachable by a `graph_run` call — the graph's
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

/// Resolve the exact node run a `graph_complete_node`/`graph_report_blocker`
/// report belongs to (B12). A node can be retried, so more than one run can
/// exist for the same `node_id` over a spec's lifetime — matching on
/// `node_id` alone (as this used to) means a report arriving late from a
/// killed/superseded attempt (e.g. a timed-out or paused agent that ignores
/// its own termination and calls the tool anyway) would get silently applied
/// to whatever newer run is now active for that node. Requiring the exact
/// `run_id` and verifying it's both still `running` and actually for the
/// claimed `node_id` closes that gap: anything else is rejected outright
/// rather than guessed at.
///
/// Returns `Err(McpError)` only for a genuine DB failure; a stale/malformed
/// report is a normal `Ok(Err(CallToolResult))` — the tool call succeeded at
/// the protocol level, it's just telling the caller its report didn't stick.
pub(crate) fn resolve_reported_run(
    db: &Database,
    run_id: &str,
    node_id: &str,
) -> Result<Result<GraphNodeRun, CallToolResult>, McpError> {
    let resolved_run_id = match db
        .resolve_run_id_by_prefix(run_id)
        .map_err(|e| e.to_string())
    {
        Ok(Some(id)) => id,
        Ok(None) => run_id.to_string(),
        Err(e) => return Ok(Err(error_result(&e))),
    };
    let resolved_node_id = match db
        .resolve_graph_node_id_by_prefix(node_id)
        .map_err(|e| e.to_string())
    {
        Ok(Some(id)) => id,
        Ok(None) => node_id.to_string(),
        Err(e) => return Ok(Err(error_result(&e))),
    };
    let run = db.get_graph_run(&resolved_run_id).map_err(internal_error)?;
    let Some(run) = run else {
        return Ok(Err(error_result(&format!(
            "No graph run found with id '{resolved_run_id}'."
        ))));
    };
    if run.node_id != resolved_node_id {
        return Ok(Err(error_result(&format!(
            "Run '{resolved_run_id}' belongs to node '{}', not '{resolved_node_id}'.",
            run.node_id
        ))));
    }
    if run.status != GraphRunStatus::Running {
        return Ok(Err(error_result(&format!(
            "Run '{resolved_run_id}' for node '{resolved_node_id}' is no longer active (status: {}); this report \
             is stale — the run was already finalized (timed out, paused, reset, superseded by \
             a retry, or already reported) — and was rejected.",
            run.status.as_str()
        ))));
    }
    Ok(Ok(run))
}

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

    // FR6: a failed graph, or a paused one whose current spec never started
    // (a queue member held back by the dirty-tree guard), skips its current
    // non-terminal spec — the same spec `run_from_node` would name.
    let current_spec = match bound_running {
        Some(spec) => spec,
        None => match queue_running_spec(db, graph_id)? {
            Some(spec) => spec,
            None => resolve_directed_spec(db, graph_id, None).map_err(|_| {
                McpError::invalid_params(
                    "No running spec found to skip from this paused graph.",
                    None,
                )
            })?,
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
                 graph_continue. Use `graph_continue run_from_node` to restart it at a chosen \
                 node (reusing earlier nodes' outputs), or graph_reset to reset it.",
                spec.name
            ),
            None,
        ),
        Some(spec) => McpError::invalid_params(
            format!(
                "Graph '{graph_id}': current spec '{}' is '{}', not resumable by \
                 graph_continue. Use graph_run to restart it from its entry node.",
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
    if let Some(spec_id) = spec_id.map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(spec) = bound.iter().find(|s| s.id == spec_id) {
            return Ok(spec.clone());
        }
        let Some(queue_id) = db
            .get_graph(graph_id)
            .map_err(|e| e.to_string())?
            .and_then(|lp| lp.active_run_queue_id)
        else {
            return Err(format!(
                "Spec '{spec_id}' does not belong to graph '{graph_id}'."
            ));
        };
        for member_id in db
            .list_queue_member_spec_ids(&queue_id)
            .map_err(|e| e.to_string())?
        {
            if member_id != spec_id {
                continue;
            }
            if let Some(spec) = db.get_graph_spec(spec_id).map_err(|e| e.to_string())? {
                return Ok(spec);
            }
        }
        return Err(format!(
            "Spec '{spec_id}' does not belong to graph '{graph_id}'."
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

/// Human report for a directed `graph_run`/`graph_continue run_from_node`
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

/// SPECCODE: `require_spec_type` support for `graph_run` — name every
/// would-run spec (bound to the graph as `pending`, or a pending queue
/// member) that still has no type. Empty list is a pass.
fn validate_specs_have_types_for_run(
    db: &Database,
    graph_id: &str,
    queue_id: Option<&str>,
) -> Result<(), String> {
    let untyped: Vec<String> = match queue_id {
        Some(queue_id) => db
            .queue_members_in_order(queue_id)
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|member| !member.locked)
            .filter_map(|member| db.get_graph_spec(&member.spec_id).ok().flatten())
            .filter(|spec| spec.spec_type.is_none())
            .map(|spec| format!("'{}'", spec.name))
            .collect(),
        None => db
            .list_graph_specs(graph_id)
            .map_err(|e| e.to_string())?
            .iter()
            .filter(|spec| spec.status == GraphSpecStatus::Pending && spec.spec_type.is_none())
            .map(|spec| format!("'{}'", spec.name))
            .collect(),
    };
    if untyped.is_empty() {
        return Ok(());
    }
    Err(format!(
        "Graph '{graph_id}' requires every spec to have a type (require_spec_type); still untyped: {}. Set the type with spec_update before relaunching.",
        untyped.join(", ")
    ))
}
