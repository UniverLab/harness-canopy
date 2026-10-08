//! MCP handler tools split out of `daemon::handler` (graph run dispatch and run guards).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_queues::{validate_queue_exists, validate_queue_not_consumed};
use crate::daemon::handler_read::build_graph_summary_json;
use crate::daemon::handler_validate::{resolve_prefix_or_error, validate_absolute_dir};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{
    GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_run_exec, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_run",
        description = "Run a graph in the background, spec by spec. With `queue_id`, runs the queue's pending specs (in queue order) through the graph instead of the graph's own bound specs. `workdir` overrides the graph's workdir for this run only. `idea` is free-form text fed to nodes as `{{spec_content}}` when the graph has no bound specs and no queue (mutually exclusive with `queue_id`)."
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
        let next_spec_preview = self
            .graph_engine
            .preview_next_spec_id(&graph_id, queue_id)
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

        Arc::clone(&self.graph_engine).start_background_run(
            graph_id.clone(),
            queue_id.map(str::to_string),
            workdir.map(str::to_string),
            idea,
            sandbox,
        );
        Ok(success_result(&match launch_warning {
            Some(warning) => format!(
                "Graph '{}' launched in background. WARNING: {}",
                graph_id, warning
            ),
            None => format!("Graph '{}' launched in background.", graph_id),
        }))
    }
}

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

pub(crate) fn node_name_for_error(db: &Database, node_id: &str) -> Result<String, String> {
    Ok(db
        .get_graph_node(node_id)
        .map_err(|e| e.to_string())?
        .map(|node| node.name)
        .unwrap_or_else(|| node_id.to_string()))
}

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

pub(crate) fn handle_skip_next_spec(db: &Database, graph_id: &str) -> Result<(), McpError> {
    let bound_running = db
        .list_graph_specs(graph_id)
        .map_err(internal_error)?
        .into_iter()
        .find(|spec| spec.status == GraphSpecStatus::Running);

    let current_spec = match bound_running {
        Some(spec) => spec,
        None => queue_running_spec(db, graph_id)?.ok_or_else(|| {
            McpError::invalid_params(
                "No running spec found to skip from this paused graph.",
                None,
            )
        })?,
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
                 graph_continue. Use graph_reset to reset it.",
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
            if spec.status == GraphSpecStatus::Running {
                return Ok(Some(spec));
            }
        }
    }
    Ok(None)
}

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
    let mut fallback: Option<GraphSpec> = None;
    for spec_id in db
        .list_queue_member_spec_ids(&queue_id)
        .map_err(internal_error)?
    {
        let Some(spec) = db.get_graph_spec(&spec_id).map_err(internal_error)? else {
            continue;
        };
        if spec.status.is_resumable() {
            return Ok(Some(spec));
        }
        if fallback.is_none()
            && !matches!(
                spec.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            )
        {
            fallback = Some(spec);
        }
    }
    Ok(fallback)
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
