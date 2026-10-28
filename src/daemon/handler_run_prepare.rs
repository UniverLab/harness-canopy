//! Section helpers for `graph_run_impl`, split out so the orchestrator
//! stays within the size gate. Behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_queues::{validate_queue_exists, validate_queue_not_consumed};
use crate::daemon::handler_read::build_graph_summary_json;
use crate::daemon::handler_run_directive::limit_reset_note;
use crate::daemon::handler_run_exec::{
    find_in_flight_run, graph_run_status_guard, in_flight_run_error, node_name_for_error,
    validate_graph_ensembles_for_run,
};
use crate::daemon::handler_validate::validate_absolute_dir;
use crate::daemon::helpers::EarlyReturn;
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphNode, GraphNodeKind, GraphSpecStatus, GraphStatus};
use crate::domain::sandbox::Sandbox;
use crate::graph_engine::{DirectedPlan, DirectedStart};

/// A directed run is the operator's way into a completed/failed
/// graph, so the status guard and blocked-graph refusal apply only to a
/// plain launch. The in-flight check always applies.
pub(crate) fn refuse_blocked_plain_launch(
    db: &Database,
    graph_id: &str,
    lp: &Graph,
) -> Result<(), EarlyReturn> {
    if let Err(message) = graph_run_status_guard(graph_id, lp.status) {
        return Err(EarlyReturn::tool_error(&message));
    }
    // C19: a graph paused with an active blocker (whether from
    // `graph_report_blocker` or from a spec exceeding its cross-run
    // attempt budget) needs a human, not another relaunch —
    // `graph_run_status_guard` alone still accepts `Paused` (that's the
    // normal resume-after-`graph_control` path), so this is a second,
    // narrower check on top of it. `graph_control` always clears the
    // graph's own status back to `draft` regardless of which specs it
    // targeted, so this only ever refuses the exact window FR4 asks
    // for: still-`paused`-and-blocked, not yet reset.
    if lp.status == GraphStatus::Paused {
        let summary = build_graph_summary_json(db, lp)?;
        if let Some(blocker) = summary.get("blocker").and_then(|v| v.as_str()) {
            return Err(EarlyReturn::tool_error(&format!(
                "Graph '{graph_id}' is blocked and cannot be relaunched via graph_control run: {blocker}. \
                 Resolve it, then graph_control reset the affected spec (naming it explicitly \
                 clears its cross-run attempt count) before relaunching."
            )));
        }
    }
    Ok(())
}

pub(crate) fn refuse_in_flight_run(db: &Database, graph_id: &str) -> Result<(), EarlyReturn> {
    if let Some(run) = find_in_flight_run(db, graph_id).map_err(internal_error)? {
        let node_name = node_name_for_error(db, &run.node_id).map_err(internal_error)?;
        return Err(EarlyReturn::tool_error(&in_flight_run_error(
            graph_id,
            &run.id,
            &node_name,
            run.started_at,
        )));
    }
    Ok(())
}

/// Validated launch inputs: the queue, the free-form idea, and the
/// workdir override.
pub(crate) struct LaunchInputs<'a> {
    pub queue_id: Option<&'a str>,
    pub idea: Option<String>,
    pub workdir: Option<&'a str>,
}

pub(crate) fn validate_launch_inputs<'a>(
    db: &Database,
    graph_id: &str,
    params: &'a GraphRunParams,
) -> Result<LaunchInputs<'a>, EarlyReturn> {
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
        return Err(EarlyReturn::tool_error(
            "`idea` and `queue_id` are mutually exclusive: an idea feeds {{spec_content}} directly, a queue provides its own specs.",
        ));
    }
    if let Some(queue_id) = queue_id {
        if let Err(e) = validate_queue_exists(db, queue_id) {
            return Err(EarlyReturn::tool_error(&e));
        }
        if let Err(e) = validate_queue_not_consumed(db, queue_id, graph_id) {
            return Err(EarlyReturn::tool_error(&e));
        }
    }
    let workdir = params
        .workdir
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(workdir) = workdir {
        if let Err(e) = validate_absolute_dir(workdir) {
            return Err(EarlyReturn::tool_error(&e));
        }
    }
    Ok(LaunchInputs {
        queue_id,
        idea,
        workdir,
    })
}

pub(crate) fn validate_launch_guards(
    db: &Database,
    graph_id: &str,
    queue_id: Option<&str>,
    require_spec_type: bool,
) -> Result<(), EarlyReturn> {
    if let Err(e) = validate_graph_ensembles_for_run(db, graph_id, queue_id) {
        return Err(EarlyReturn::tool_error(&e));
    }
    // SPECCODE: opt-in enforcement — a graph that demands spec types
    // refuses to launch when a spec it would actually execute (a bound
    // pending spec, or a pending queue member) still has none, naming it.
    if require_spec_type {
        if let Err(message) = validate_specs_have_types_for_run(db, graph_id, queue_id) {
            return Err(EarlyReturn::tool_error(&message));
        }
    }
    Ok(())
}

impl TaskTriggerHandler {
    /// Preview the first spec this run executes and run the dirty-start
    /// check against it. Returns `(next_spec_preview, launch_warning)`.
    pub(crate) async fn preview_and_dirty_check(
        &self,
        graph_id: &str,
        workdir: Option<&str>,
        lp_workdir: &str,
        queue_id: Option<&str>,
        start_node_id: Option<&str>,
        allow_dirty_start: bool,
    ) -> Result<(Option<String>, Option<String>), EarlyReturn> {
        let effective_workdir = workdir.unwrap_or(lp_workdir);
        // With `start_node_id`, the preview must be the first spec a
        // FRESH dispatch executes (the directed target — see
        // [`GraphEngine::preview_directed_run_spec_id`]); without it, the
        // running-member-first preview keeps feeding the dirty-start
        // exclusion as before.
        let next_spec_preview = if start_node_id.is_some() {
            self.graph_engine
                .preview_directed_run_spec_id(graph_id, queue_id)
        } else {
            self.graph_engine.preview_next_spec_id(graph_id, queue_id)
        }
        .map_err(internal_error)?;
        let mut launch_warning: Option<String> = None;
        match self
            .graph_engine
            .dirty_start_check(
                graph_id,
                effective_workdir,
                next_spec_preview.as_deref(),
                allow_dirty_start,
            )
            .await
            .map_err(internal_error)?
        {
            Some(notice) if notice.refuse => {
                return Err(EarlyReturn::tool_error(&format!(
                    "Refusing to launch graph '{graph_id}': {}",
                    notice.message
                )));
            }
            Some(notice) => launch_warning = Some(notice.message),
            None => {}
        }
        Ok((next_spec_preview, launch_warning))
    }

    /// Resolve the directed start synchronously so a bad node or a
    /// missing reachable output refuses the launch before any dispatch. The
    /// target spec is the first spec this run would execute (the same
    /// preview the dirty-start check above used).
    pub(crate) fn plan_launch_directed_start(
        &self,
        graph_id: &str,
        next_spec_preview: &Option<String>,
        start: &str,
    ) -> Result<DirectedStartOutcome, EarlyReturn> {
        let Some(spec_id) = next_spec_preview.clone() else {
            return Err(EarlyReturn::tool_error(&format!(
                "Graph '{graph_id}' has no spec to start a directed run at."
            )));
        };
        match self
            .graph_engine
            .plan_directed_run(graph_id, &spec_id, start)
        {
            Ok(plan) => {
                // FR1: the engine zeroes the count when the directed run
                // starts; the response says so.
                let limit_note = limit_reset_note(&plan.spec_name);
                Ok(DirectedStartOutcome {
                    plan: Some(plan),
                    limit_note,
                    directed: Some(DirectedStart {
                        spec_id,
                        node_arg: start.to_string(),
                    }),
                })
            }
            Err(message) => Err(EarlyReturn::tool_error(&message)),
        }
    }
}

/// The resolved directed start: the engine plan, the limit-reset note
/// for the response, and the start descriptor for the background run.
pub(crate) struct DirectedStartOutcome {
    pub plan: Option<DirectedPlan>,
    pub limit_note: String,
    pub directed: Option<DirectedStart>,
}

pub(crate) async fn create_launch_sandbox(
    db: &Database,
    graph_id: &str,
    workdir: Option<&str>,
    lp_workdir: &str,
) -> Result<Option<Sandbox>, EarlyReturn> {
    // The instruction file is per-worktree and the worktree is shared
    // across the graph's nodes, so it must carry the *first* agent
    // node's platform filename (CLAUDE.md / GEMINI.md / …) — a
    // hardcoded "opencode" would write AGENTS.md and leave a
    // claude/gemini node with no protocol file at all.
    let cli_name = sandbox_cli_name(db, graph_id);
    let protocol = crate::domain::sandbox::canopy_protocol_block();
    let effective_workdir = workdir.unwrap_or(lp_workdir);
    match crate::domain::sandbox::create_sandbox(effective_workdir, &cli_name, protocol).await {
        Ok(sb) => {
            let _ = db.insert_sandbox_run(&sb, "graph", graph_id);
            Ok(Some(sb))
        }
        Err(e) => Err(EarlyReturn::tool_error(&format!(
            "Failed to create sandbox: {e}"
        ))),
    }
}

/// The platform filename target for a sandboxed run's instruction file: the
/// first agent node's configured `platform`/`cli`, else `"opencode"`. The
/// worktree is shared across the graph's nodes, so the *first* agent node
/// decides — a hardcoded `"opencode"` would write AGENTS.md and leave a
/// claude/gemini node with no protocol file at all.
fn sandbox_cli_name(db: &Database, graph_id: &str) -> String {
    let Some(details) = db.get_graph_details(graph_id).ok().flatten() else {
        return "opencode".to_string();
    };
    let mut agents: Vec<&GraphNode> = details
        .graph_nodes
        .iter()
        .chain(details.specs.iter().flat_map(|s| s.nodes.iter()))
        .filter(|n| n.kind == GraphNodeKind::Agent)
        .collect();
    agents.sort_by_key(|n| n.position);
    agents
        .iter()
        .find_map(|n| {
            n.config
                .get("platform")
                .or_else(|| n.config.get("cli"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "opencode".to_string())
}

/// SPECCODE: `require_spec_type` support for `graph_control` — name every
/// would-run spec (bound to the graph as `pending`, or a pending queue
/// member) that still has no type. Empty list is a pass.
pub(crate) fn validate_specs_have_types_for_run(
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
        "Graph '{graph_id}' requires every spec to have a type (require_spec_type); still untyped: {}. Set the type with `spec_edit update` before relaunching.",
        untyped.join(", ")
    ))
}
