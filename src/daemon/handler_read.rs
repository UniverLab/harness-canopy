//! MCP handler tools split out of `daemon::handler` (graph read/display tools and JSON builders).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_run_exec::{graph_run_blocker, graph_run_json};
use crate::daemon::handler_specs::build_spec_run_info;
use crate::daemon::handler_validate::{parse_router_routes, resolve_prefix_or_error};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{
    Graph, GraphDetails, GraphEdge, GraphNode, GraphNodeKind, GraphRunStatus, GraphSpec,
    GraphSpecStatus, GraphStatus,
};
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_read, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_get",
        description = "Return a graph with its ordered specs, nodes, and edges. Hooks are not retroactive — a hook registered after its event has already happened does not fire."
    )]
    pub(crate) async fn graph_get(
        &self,
        Parameters(params): Parameters<GraphGetParams>,
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
        let lp = match self.db.get_graph_details(&graph_id) {
            Ok(Some(w)) => w,
            Ok(None) => return Ok(error_result(&format!("Graph '{}' not found.", graph_id))),
            Err(e) => return Err(internal_error(e.to_string())),
        };

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(
                &graph_details_json(&self.db, &lp).map_err(internal_error)?,
            )
            .unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "graph_list",
        description = "List graphs, optionally filtered by workdir."
    )]
    pub(crate) async fn graph_list(
        &self,
        Parameters(params): Parameters<GraphListParams>,
    ) -> Result<CallToolResult, McpError> {
        let graphs = self
            .db
            .list_graphs(
                params.workdir.as_deref(),
                params.include_archived.unwrap_or(false),
            )
            .map_err(internal_error)?;

        let out = build_graph_list_json(&self.db, &graphs)?;

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&out).unwrap_or_default(),
        )]))
    }
}

pub(crate) fn build_graph_summary_json(
    db: &Database,
    lp: &Graph,
) -> Result<serde_json::Value, McpError> {
    // CB30: one helper decides "what spec is this graph on" — never the
    // raw bound list, whose queue/idea runs read as a nameless placeholder.
    let current_spec = match active_graph_spec_context(db, lp).map_err(internal_error)? {
        ActiveGraphSpec::Bound(spec) | ActiveGraphSpec::QueueMember { spec, .. } => Some(spec),
        ActiveGraphSpec::Idea { .. } | ActiveGraphSpec::None => None,
    }
    .map(|spec| build_spec_run_info(db, &spec))
    .transpose()?;

    let mut out = serde_json::json!({
        "id": lp.id,
        "name": lp.name,
        "status": lp.status.as_str(),
        "trigger": graph_trigger_json(lp),
        "current_spec": current_spec.as_ref().map(|v| &v.name),
        "current_node": current_spec.as_ref().and_then(|v| v.current_node.as_ref()),
        "blocked": current_spec.as_ref().is_some_and(|v| v.blocker.is_some()),
        "blocker": current_spec.as_ref().and_then(|v| v.blocker.clone()),
        "spec_attempts": current_spec.as_ref().map(|v| v.cross_run_attempts),
        "created_at": lp.created_at.to_rfc3339(),
        "workdir": lp.workdir,
        "archived": lp.archived,
    });
    // CB30: when the graph draws from a queue, say so and name it, so the
    // reader knows where the sequence comes from.
    if let Some(queue_id) = lp.active_run_queue_id.as_deref() {
        if let Some(queue) = db.get_queue(queue_id).map_err(internal_error)? {
            out["queue"] = serde_json::json!({ "id": queue.id, "name": queue.name });
        }
    }
    Ok(out)
}

/// Serialize a graph's trigger for MCP responses: always a `type` label, plus
/// the cron schedule or watch path/events when applicable.
pub(crate) fn graph_trigger_json(lp: &Graph) -> serde_json::Value {
    let mut out = serde_json::json!({ "type": lp.trigger_type_label() });
    if let Some(schedule) = lp.schedule_expr() {
        out["schedule"] = serde_json::json!(schedule);
    }
    if let Some(path) = lp.watch_path() {
        out["path"] = serde_json::json!(path);
        if let Some(events) = lp.watch_events() {
            out["events"] =
                serde_json::json!(events.iter().map(|e| e.to_string()).collect::<Vec<_>>());
        }
    }
    out
}

pub(crate) fn build_graph_list_json(
    db: &Database,
    graphs: &[Graph],
) -> Result<Vec<serde_json::Value>, McpError> {
    graphs
        .iter()
        .map(|lp| build_graph_summary_json(db, lp))
        .collect()
}

/// What a graph is actually working on (CB30).
///
/// `list_graph_specs` returns the graph's *bound* specs, which for a queue or
/// idea run is the blank-name placeholder row the engine inserts as
/// `graph_runs.spec_id` bookkeeping — never the work itself. Every display
/// surface must go through [`active_graph_spec_context`] instead of reading
/// the bound list directly, so `graph_get`, `graph list` and `canopy graph
/// info` agree on the answer.
#[derive(Debug)]
pub(crate) enum ActiveGraphSpec {
    /// A normal bound spec (never a placeholder).
    Bound(GraphSpec),
    /// A queue member spec currently in flight.
    QueueMember {
        spec: GraphSpec,
        queue_id: String,
        queue_name: String,
    },
    /// An idea-driven run: a placeholder exists but must not be shown.
    Idea { placeholder_id: String },
    /// Nothing running (all done, or nothing queued yet).
    None,
}

/// The single answer to "what spec is this graph on", shared by `graph_get`
/// ([`graph_details_json`]), `graph list` ([`build_graph_summary_json`]) and
/// `canopy graph info` (`handle_graph_info` in `daemon/graph_cli.rs`).
///
/// The placeholder is hidden, never relabelled: a fabricated name would
/// replace a confusing answer with a false one.
pub(crate) fn active_graph_spec_context(
    db: &Database,
    lp: &Graph,
) -> anyhow::Result<ActiveGraphSpec> {
    if let Some(queue_id) = lp.active_run_queue_id.as_deref() {
        if let Some(queue_name) = db.get_queue(queue_id)?.map(|queue| queue.name) {
            if let Some(active) = queue_active_graph_spec(db, queue_id, &queue_name)? {
                return Ok(active);
            }
            // Queue drained (or empty): fall through to the bound/idea
            // logic so a finished queue graph reports `None` rather than a
            // stale member. Callers still render the queue link itself from
            // `active_run_queue_id`.
        }
    }
    bound_active_graph_spec(db, &lp.id, lp.active_run_queue_id.is_some())
}

/// The spec dispatch is on inside `queue_id`, or `None` when the queue holds
/// nothing runnable: a `Running` member anywhere beats an earlier pending one
/// (dispatch is sequential, so a running member means everything before it
/// already completed); with nothing running, the first runnable — or stalled —
/// member in queue order is the one to report.
pub(crate) fn queue_active_graph_spec(
    db: &Database,
    queue_id: &str,
    queue_name: &str,
) -> anyhow::Result<Option<ActiveGraphSpec>> {
    let mut running: Option<GraphSpec> = None;
    let mut pending: Option<GraphSpec> = None;
    for spec_id in db.list_queue_member_spec_ids(queue_id)? {
        let Some(spec) = db.get_graph_spec(&spec_id)? else {
            continue;
        };
        if spec.status == GraphSpecStatus::Running {
            if running.is_none() {
                running = Some(spec);
            }
        // Same fallback shape as `build_graph_summary_json`'s
        // bound-spec search: the next runnable (or stalled)
        // member in queue order when nothing is `Running`.
        } else if pending.is_none()
            && matches!(
                spec.status,
                GraphSpecStatus::Pending | GraphSpecStatus::Interrupted | GraphSpecStatus::Failed
            )
        {
            pending = Some(spec);
        }
    }
    let Some(spec) = running.or(pending) else {
        return Ok(None);
    };
    Ok(Some(ActiveGraphSpec::QueueMember {
        spec,
        queue_id: queue_id.to_string(),
        queue_name: queue_name.to_string(),
    }))
}

/// The bound spec dispatch is on — the first non-terminal real spec in
/// position order, the same pick `build_graph_summary_json` historically
/// made — or the idea placeholder when that's all the graph has. The
/// placeholder is hidden, never relabelled: a fabricated name would
/// replace a confusing answer with a false one.
pub(crate) fn bound_active_graph_spec(
    db: &Database,
    graph_id: &str,
    has_active_queue: bool,
) -> anyhow::Result<ActiveGraphSpec> {
    let mut placeholder_id: Option<String> = None;
    let mut current: Option<GraphSpec> = None;
    for spec in db.list_graph_specs(graph_id)? {
        if crate::graph_engine::is_no_spec_placeholder(&spec) {
            if placeholder_id.is_none() {
                placeholder_id = Some(spec.id.clone());
            }
            continue;
        }
        if current.is_none()
            && matches!(
                spec.status,
                GraphSpecStatus::Running
                    | GraphSpecStatus::Pending
                    | GraphSpecStatus::Interrupted
                    | GraphSpecStatus::Failed
            )
        {
            current = Some(spec);
        }
    }
    if let Some(spec) = current {
        return Ok(ActiveGraphSpec::Bound(spec));
    }
    if let Some(placeholder_id) = placeholder_id {
        // CB30: a completed queue-driven graph still has `active_run_queue_id`
        // set (B31 keeps it as last-run context) and only the placeholder in
        // its bound specs. Without this guard, we'd report `Idea` for a
        // finished queue — replacing one false answer with another.
        if has_active_queue {
            return Ok(ActiveGraphSpec::None);
        }
        return Ok(ActiveGraphSpec::Idea { placeholder_id });
    }
    Ok(ActiveGraphSpec::None)
}

/// CB52 FR4: one `orphan_join_warnings` entry for a join node with no
/// ensemble row. Shared by `graph_get` (advisory, always reported) and
/// `graph_preflight` (refusal — a graph in this state would die with
/// "ambiguous outgoing edges", so no probe quota is spent on it).
pub(crate) fn orphan_join_warning_text(node: &GraphNode) -> String {
    format!(
        "join node '{}' ({}) belongs to no ensemble — it was orphaned when an ensemble that owned or referenced its entry/exit was deleted. Delete this orphan node with graph_delete_node or wire it into a repaired graph; do not re-create an ensemble around it automatically. (canopy doctor also lists orphaned joins per graph.)",
        node.name, node.id
    )
}

pub(crate) fn orphan_join_warning_json(node: &GraphNode) -> serde_json::Value {
    serde_json::json!({
        "node_id": node.id,
        "name": node.name,
        "kind": "join",
        "spec_id": node.spec_id,
        "graph_id": node.graph_id,
        "warning": orphan_join_warning_text(node),
    })
}

pub(crate) fn graph_details_json(
    db: &Database,
    lp: &GraphDetails,
) -> anyhow::Result<serde_json::Value> {
    let mut specs = lp
        .specs
        .iter()
        // Skip the blank-name placeholder an idea-driven run binds to the
        // graph to carry `{{spec_content}}`: it's engine bookkeeping (purged
        // once terminal on the next dispatch) and must not read as work.
        // Trimmed to match the engine's bookkeeping sentinel.
        .filter(|spec| !spec.spec.name.trim().is_empty())
        .map(|spec| graph_spec_details_json(db, spec, lp.lp.status))
        .collect::<anyhow::Result<Vec<_>>>()?;
    // CB30: while the graph draws from a queue, the bound list above is just
    // the placeholder — name the queue spec actually running instead, so
    // this surface agrees with `graph_node_runs_list`.
    let ctx = active_graph_spec_context(db, &lp.lp)?;
    match &ctx {
        ActiveGraphSpec::QueueMember { spec, .. } => {
            if !specs
                .iter()
                .any(|s| s.get("id").and_then(|v| v.as_str()) == Some(spec.id.as_str()))
            {
                let details = crate::domain::graphs::GraphSpecDetails {
                    nodes: db.list_graph_nodes(&spec.id)?,
                    edges: db.list_graph_edges(&spec.id)?,
                    spec: spec.clone(),
                };
                specs.push(graph_spec_details_json(db, &details, lp.lp.status)?);
            }
        }
        // The placeholder id is engine bookkeeping (a bound row like any
        // other); it is read here only to assert the sentinel invariant —
        // the row itself is never rendered.
        ActiveGraphSpec::Idea { placeholder_id } => {
            debug_assert!(!placeholder_id.is_empty());
        }
        ActiveGraphSpec::Bound(_) | ActiveGraphSpec::None => {}
    }
    let ensembles = db
        .list_ensembles_for_graph(&lp.lp.id)?
        .iter()
        .map(|details| ensemble_details_json(details, &lp.graph_edges))
        .collect::<Vec<_>>();

    // CB52 FR4: join nodes that belong to no ensemble — the state left
    // behind when an ensemble row was deleted (pre-fix: silently, via `ON
    // DELETE CASCADE` from a referenced node) while its member/join nodes
    // survived. Never an error here: the graph must stay inspectable so the
    // operator can repair it. Never auto-repaired (C1).
    let mut orphan_join_warnings = Vec::new();
    for node in db.list_orphan_join_nodes_for_graph(&lp.lp.id)? {
        orphan_join_warnings.push(orphan_join_warning_json(&node));
    }
    for spec in &lp.specs {
        for node in db.list_orphan_join_nodes_for_spec(&spec.spec.id)? {
            orphan_join_warnings.push(orphan_join_warning_json(&node));
        }
    }

    let mut out = serde_json::json!({
        "id": lp.lp.id,
        "name": lp.lp.name,
        "description": lp.lp.description,
        "workdir": lp.lp.workdir,
        "status": lp.lp.status.as_str(),
        "trigger": graph_trigger_json(&lp.lp),
        "created_at": lp.lp.created_at.to_rfc3339(),
        "started_at": lp.lp.started_at.map(|value| value.to_rfc3339()),
        "completed_at": lp.lp.completed_at.map(|value| value.to_rfc3339()),
        "autorun_at": lp.lp.autorun_at.map(|value| value.to_rfc3339()),
        "archived": lp.lp.archived,
        "graph": {
            "nodes": lp.graph_nodes.iter().map(|node| graph_node_json(node, &lp.graph_edges)).collect::<Vec<_>>(),
            "edges": lp.graph_edges.iter().map(graph_edge_json).collect::<Vec<_>>(),
            "ensembles": ensembles,
        },
        "specs": specs,
        "on_completed": lp.lp.hooks.get(&crate::domain::graphs::GraphHookEvent::OnCompleted)
            .and_then(|hooks| hooks.first())
            .map(graph_completion_hook_json),
        "hooks": lp.lp.hooks.iter().map(|(event, hooks)| {
            serde_json::json!({
                "event": event.as_str(),
                "hooks": hooks.iter().map(graph_completion_hook_json).collect::<Vec<_>>(),
            })
        }).collect::<Vec<_>>(),
        "completion_hook_runs": lp
            .completion_hook_runs
            .iter()
            .map(graph_completion_hook_run_json)
            .collect::<Vec<_>>(),
        "orphan_join_warnings": orphan_join_warnings,
    });
    // CB30: when the graph draws from a queue, say so and name it; when it
    // runs a loose idea, say that instead of leaving an empty `specs` to be
    // misread as "nothing running".
    match &ctx {
        ActiveGraphSpec::QueueMember {
            queue_id,
            queue_name,
            ..
        } => {
            out["queue"] = serde_json::json!({ "id": queue_id, "name": queue_name });
        }
        ActiveGraphSpec::Bound(_) | ActiveGraphSpec::Idea { .. } | ActiveGraphSpec::None => {
            if let Some(queue_id) = lp.lp.active_run_queue_id.as_deref() {
                if let Some(queue) = db.get_queue(queue_id)? {
                    out["queue"] = serde_json::json!({ "id": queue.id, "name": queue.name });
                }
            }
        }
    }
    if matches!(ctx, ActiveGraphSpec::Idea { .. }) && out.get("queue").is_none() {
        out["mode"] = serde_json::json!("idea");
    }
    Ok(out)
}

/// Serialize an ensemble (F1) as the one unit `graph_get`/`graph_update_ensemble`
/// address — members in position order alongside the join's own config, so a
/// client can render/edit it without reconstructing it from the underlying
/// nodes/edges itself. `edges` is the owning graph's full edge list, from
/// which every entry source (not just the primary `entry_from_node`) is
/// derived — an ensemble entered from several places shows all of them.
pub(crate) fn ensemble_details_json(
    details: &crate::domain::graphs::EnsembleDetails,
    edges: &[GraphEdge],
) -> serde_json::Value {
    let ensemble = &details.ensemble;
    let entry_sources = crate::db::ensembles::ensemble_entry_sources(
        &details.members,
        &ensemble.join_node_id,
        edges,
    );
    serde_json::json!({
        "id": ensemble.id,
        "name": ensemble.name,
        "kind": ensemble.kind.as_str(),
        "commit_rights": ensemble.commit_rights,
        "prompt_template": ensemble.prompt_template,
        "join_node_id": ensemble.join_node_id,
        "entry_from_node": ensemble.entry_from_node,
        "entry_condition": ensemble.entry_condition.as_str(),
        "entry_sources": entry_sources.iter().map(|(node_id, condition)| serde_json::json!({
            "node_id": node_id,
            "condition": condition.as_str(),
        })).collect::<Vec<_>>(),
        "min_pass": ensemble.min_pass,
        "straggler_timeout_minutes": ensemble.straggler_timeout_minutes,
        "effective_straggler_timeout_minutes": ensemble.effective_straggler_timeout_minutes(),
        "quorum_grace_minutes": ensemble.quorum_grace_minutes,
        "timeout_minutes": ensemble.timeout_minutes,
        "infra_retry_limit": ensemble.infra_retry_limit,
        "infra_crash_max_seconds": ensemble.infra_crash_max_seconds,
        "infra_backoff_seconds": ensemble.infra_backoff_seconds,
        "on_pass_to": ensemble.on_pass_to,
        "on_fail_to": ensemble.on_fail_to,
        "round_robin_index": ensemble.round_robin_index,
        "created_at": ensemble.created_at.to_rfc3339(),
        "members": details.members.iter().map(|member| serde_json::json!({
            "node_id": member.node_id,
            "position": member.position,
            "platform": member.platform,
            "model": member.model,
            "prompt_override": member.prompt_override,
            // Which prompt this member actually renders — "override" (its own
            // prompt_override) or "shared" (the ensemble's prompt_template) —
            // so a client can tell the two apart without diffing member config
            // against the ensemble row itself.
            "prompt_source": if member.prompt_override.is_some() { "override" } else { "shared" },
            "infra_retry_limit_override": member.infra_retry_limit,
            "infra_crash_max_seconds_override": member.infra_crash_max_seconds,
            "infra_backoff_seconds_override": member.infra_backoff_seconds,
        })).collect::<Vec<_>>(),
    })
}

pub(crate) fn graph_completion_hook_json(
    hook: &crate::domain::graphs::GraphCompletionHook,
) -> serde_json::Value {
    serde_json::json!({
        "platform": hook.platform,
        "model": hook.model,
        "effort": hook.effort,
        "prompt": hook.prompt,
        "command": hook.command,
        "target_session_id": hook.target_session_id,
        "target_session_name": hook.target_session_name,
        "timeout_minutes": hook.timeout_minutes,
        "target_graph_id": hook.target_graph_id,
        "queue_id": hook.queue_id,
        "workdir_override": hook.workdir_override,
        "idea": hook.idea,
    })
}

pub(crate) fn graph_completion_hook_run_json(
    run: &crate::domain::graphs::GraphCompletionHookRun,
) -> serde_json::Value {
    serde_json::json!({
        "id": run.id,
        "graph_id": run.graph_id,
        "event": run.event.as_str(),
        "hook_index": run.hook_index,
        "status": run.status.as_str(),
        "output": run.output,
        "summary": run.summary,
        "started_at": run.started_at.to_rfc3339(),
        "completed_at": run.completed_at.map(|value| value.to_rfc3339()),
    })
}

pub(crate) fn graph_spec_details_json(
    db: &Database,
    spec: &crate::domain::graphs::GraphSpecDetails,
    graph_status: GraphStatus,
) -> anyhow::Result<serde_json::Value> {
    let runs = db.list_graph_runs_for_spec(&spec.spec.id)?;
    let current_run = runs
        .iter()
        .rev()
        .find(|run| run.status == GraphRunStatus::Running)
        .or_else(|| runs.last());
    let blocker = runs.last().and_then(graph_run_blocker);
    let resume_actions =
        if graph_status == GraphStatus::Paused && spec.spec.status == GraphSpecStatus::Running {
            vec!["retry_current_node", "skip_next_spec"]
        } else {
            Vec::new()
        };
    let runs = runs.iter().map(graph_run_json).collect::<Vec<_>>();
    let ensembles = db
        .list_ensembles_for_spec(&spec.spec.id)?
        .iter()
        .map(|details| ensemble_details_json(details, &spec.edges))
        .collect::<Vec<_>>();

    Ok(serde_json::json!({
        "id": spec.spec.id,
        "graph_id": spec.spec.graph_id,
        "name": spec.spec.name,
        "description": spec.spec.description,
        "position": spec.spec.position,
        "parallelizable": spec.spec.parallelizable,
        "status": spec.spec.status.as_str(),
        "current_node": current_run.map(|run| run.node_id.clone()),
        "blocked": blocker.is_some(),
        "blocker": blocker,
        "resume_actions": resume_actions,
        // The baseline `{{spec_start_head}}` resolved to for this spec's
        // current attempt (B10) — lets debugging see exactly which HEAD a
        // check node's commit-detection compared against.
        "spec_start_head": spec.spec.spec_start_head,
        // The HEAD this attempt's own `commit_rights: true` node last left
        // behind (C15), if any — `{{spec_committed_head}}` in a check node.
        // `null` means no node this run trusts to commit has moved HEAD yet.
        "spec_committed_head": spec.spec.spec_committed_head,
        "spec_start_dirty": spec.spec.spec_start_dirty,
        "spec_end_dirty": spec.spec.spec_end_dirty,
        "spec_end_dirty_paths": spec.spec.spec_end_dirty_paths,
        "started_at": spec.spec.started_at.map(|value| value.to_rfc3339()),
        "completed_at": spec.spec.completed_at.map(|value| value.to_rfc3339()),
        "nodes": spec.nodes.iter().map(|node| graph_node_json(node, &spec.edges)).collect::<Vec<_>>(),
        "edges": spec.edges.iter().map(graph_edge_json).collect::<Vec<_>>(),
        "ensembles": ensembles,
        "runs": runs,
    }))
}

pub(crate) fn graph_node_json(node: &GraphNode, edges: &[GraphEdge]) -> serde_json::Value {
    serde_json::json!({
        "id": node.id,
        "spec_id": node.spec_id,
        "graph_id": node.graph_id,
        "name": node.name,
        "kind": node.kind.display_str(),
        "config": node.config,
        "position": node.position,
        "created_at": node.created_at.to_rfc3339(),
        "routes": router_routes_json(node, edges),
        "prompt_source": agent_prompt_source_json(node),
        "infra_config": infra_config_json(node),
    })
}

/// CM30/FR3: an agent node's effective infra-retry config plus where each
/// field came from — `"node"` (this node's own config key, which for an
/// ensemble member may be a baked-in member/ensemble override — see the
/// ensemble's own `infra_retry_limit`/members[].infra_retry_limit_override
/// fields for which), `"platform"`, or `"default"`. `None` for every other
/// node kind.
pub(crate) fn infra_config_json(node: &GraphNode) -> Option<serde_json::Value> {
    if node.kind != GraphNodeKind::Agent {
        return None;
    }
    let resolved = crate::graph_engine::resolve_node_infra_config(node);
    Some(serde_json::json!({
        "infra_retry_limit": {
            "value": resolved.retry_limit.value,
            "source": resolved.retry_limit.source,
        },
        "infra_crash_max_seconds": {
            "value": resolved.crash_max_seconds.value,
            "source": resolved.crash_max_seconds.source,
        },
        "infra_backoff_seconds": {
            "value": resolved.backoff_seconds.value,
            "source": resolved.backoff_seconds.source,
        },
    }))
}

/// For an agent node, which branch of `resolve_node_prompt_template`'s
/// precedence it will actually run on — `"explicit"`, `"preset"`, or
/// `"default_fallback"` (see `graph_engine::agent_prompt_source`) — so
/// `graph_get` makes a node quietly running on the bare fallback template
/// (no `prompt_template`, no `prompt_preset`) visible without requiring a
/// run first. `None` for every other node kind, same convention as
/// [`router_routes_json`].
pub(crate) fn agent_prompt_source_json(node: &GraphNode) -> Option<&'static str> {
    if node.kind != GraphNodeKind::Agent {
        return None;
    }
    Some(crate::graph_engine::agent_prompt_source(&node.config))
}

/// For a router node, every declared route alongside the edge (if any) that
/// currently serves it — `graph_get`'s view into "which edge serves each
/// route". `None` for every other node kind (or if the router's `config`
/// somehow fails to parse — `graph_get` should never fail outright over a
/// display concern).
pub(crate) fn router_routes_json(
    node: &GraphNode,
    edges: &[GraphEdge],
) -> Option<serde_json::Value> {
    if node.kind != GraphNodeKind::Router {
        return None;
    }
    let (routes, fallback) = parse_router_routes(&node.config).ok()?;
    Some(serde_json::json!(routes
        .iter()
        .map(|route| {
            let serving_edge = edges.iter().find(|edge| {
                edge.from_node == node.id
                    && edge.condition.route_label() == Some(route.label.as_str())
            });
            serde_json::json!({
                "label": route.label,
                "description": route.description,
                "fallback": route.label == fallback,
                "edge_id": serving_edge.map(|edge| edge.id.clone()),
                "to_node": serving_edge.map(|edge| edge.to_node.clone()),
            })
        })
        .collect::<Vec<_>>()))
}

pub(crate) fn graph_edge_json(edge: &GraphEdge) -> serde_json::Value {
    serde_json::json!({
        "id": edge.id,
        "spec_id": edge.spec_id,
        "graph_id": edge.graph_id,
        "from_node": edge.from_node,
        "to_node": edge.to_node,
        "condition": edge.condition.as_str(),
        "route": edge.condition.route_label(),
    })
}
