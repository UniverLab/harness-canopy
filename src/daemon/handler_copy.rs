//! MCP handler tools split out of `daemon::handler` (node/ensemble copy planners and tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_edges::{graph_target_parts, resolve_copy_target};
use crate::daemon::handler_ensemble_core::validate_ensemble_members;
use crate::daemon::handler_ensemble_plan::{
    build_ensemble_unit, BuiltEnsembleUnit, EnsembleUnitSpec,
};
use crate::daemon::handler_formatting::{build_json_result, internal_error};
use crate::daemon::handler_nodes::validate_node_config;
use crate::daemon::handler_validate::{
    validate_edge_condition, validate_node_exists, validate_node_not_ensemble_owned,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{
    Ensemble, EnsembleKind, EnsembleMemberSpec, GraphEdge, GraphEdgeCondition, GraphNode,
    GraphNodeKind,
};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_copy, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_copy_node",
        description = "Duplicate a graph node's CONFIG (never its runtime state) into a graph — the source's own by default, or a different spec/graph for a cross-graph copy. Optional overrides: name, and config_overrides shallow-merged over the copied config (e.g. swap prompt_template, platform, model, timeout_minutes). Optional wiring: entry_from_node/entry_condition (incoming edge) and on_pass_to/on_fail_to (outgoing edges). All ids are new. An unwired copy is valid and is reported as such — no edges are assumed."
    )]
    pub(crate) async fn graph_copy_node(
        &self,
        Parameters(params): Parameters<GraphCopyNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let plan = match plan_node_copy(&self.db, &params) {
            Ok(plan) => plan,
            Err(e) => return Ok(error_result(&e)),
        };

        self.db
            .insert_node_with_edges(&plan.node, &plan.edges)
            .map_err(internal_error)?;

        // CM2: auto-wire a `Error` edge to the graph's infra node if set.
        if let Some(ref lid) = plan.node.graph_id {
            if let Ok(Some(lp)) = self.db.get_graph(lid) {
                if let Some(ref infra_node_id) = lp.infra_node_id {
                    let already_has_error = plan.edges.iter().any(|e| {
                        e.from_node == plan.node.id && e.condition == GraphEdgeCondition::Error
                    });
                    if !already_has_error {
                        let edge = GraphEdge {
                            id: uuid::Uuid::new_v4().to_string(),
                            spec_id: plan.node.spec_id.clone(),
                            graph_id: Some(lid.clone()),
                            from_node: plan.node.id.clone(),
                            to_node: infra_node_id.clone(),
                            condition: GraphEdgeCondition::Error,
                        };
                        let _ = self.db.insert_graph_edge(&edge);
                    }
                }
            }
        }

        let wired = !plan.edges.is_empty();
        let mut mapping = serde_json::Map::new();
        mapping.insert(plan.source_id.clone(), serde_json::json!(plan.node.id));
        let note = node_copy_note(&plan.source_id, &plan.node.id, wired);
        Ok(build_json_result(&serde_json::json!({
            "node_id": plan.node.id,
            "mapping": mapping,
            "wired": wired,
            "wiring": plan.wiring,
            "copied_runtime_state": false,
            "note": note,
        })))
    }

    #[tool(
        name = "graph_copy_ensemble",
        description = "Duplicate a whole ensemble unit (members + quorum + shared prompt) — CONFIG only, never runtime state — in one call. Optional overrides: name, prompt_template (e.g. swap a proposer prompt for a review prompt), members (2–8 parallel, 2–16 cascade/round_robin replacement), min_pass, timeout_minutes, straggler_timeout_minutes, and wiring (from_node/condition entry, on_pass_to/on_fail_to exit). Wiring defaults to the source's; for a cross-graph copy pass wiring that exists in the target graph. Every id is new; the response returns the full old→new id mapping and the wiring actually applied."
    )]
    pub(crate) async fn graph_copy_ensemble(
        &self,
        Parameters(params): Parameters<GraphCopyEnsembleParams>,
    ) -> Result<CallToolResult, McpError> {
        let plan = match plan_ensemble_copy(&self.db, &params) {
            Ok(plan) => plan,
            Err(e) => return Ok(error_result(&e)),
        };
        let built = &plan.built;

        self.db
            .insert_ensemble_unit(
                &built.ensemble,
                &built.members,
                &built.member_nodes,
                &built.join_node,
                &built.edges,
            )
            .map_err(internal_error)?;

        // Full old→new id mapping. Member nodes map by position only when the
        // members weren't replaced (otherwise there's no 1:1 correspondence).
        let mut mapping = serde_json::Map::new();
        mapping.insert(
            plan.source_ensemble_id.clone(),
            serde_json::json!(built.ensemble.id),
        );
        mapping.insert(
            plan.source_join_node_id.clone(),
            serde_json::json!(built.join_node.id),
        );
        if !plan.members_replaced {
            for (old_id, new_node) in plan
                .source_member_node_ids
                .iter()
                .zip(built.member_nodes.iter())
            {
                mapping.insert(old_id.clone(), serde_json::json!(new_node.id));
            }
        }
        let new_member_node_ids: Vec<&str> = built
            .member_nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect();

        Ok(build_json_result(&serde_json::json!({
            "ensemble_id": built.ensemble.id,
            "join_node_id": built.join_node.id,
            "mapping": mapping,
            "members_replaced": plan.members_replaced,
            "member_node_ids": new_member_node_ids,
            "wiring": {
                "entry_from_node": plan.entry_from_node,
                "entry_condition": plan.entry_condition.as_str(),
                "on_pass_to": plan.on_pass_to,
                "on_fail_to": plan.on_fail_to,
            },
            "copied_runtime_state": false,
            "note": format!(
                "Copied ensemble '{}' as '{}' — wired from '{}' to '{}'{}.",
                plan.source_ensemble_id,
                built.ensemble.id,
                plan.entry_from_node,
                plan.on_pass_to,
                plan.on_fail_to.as_deref().map(|f| format!(" (fail → '{f}')")).unwrap_or_default(),
            ),
        })))
    }
}

/// A planned single-node copy: the new node, its optional wiring edges, and a
/// report of the wiring actually applied. Pure of side effects — the caller
/// persists it with [`Database::insert_node_with_edges`]. Split out from
/// `graph_copy_node` so the copy/override/wiring logic is unit-testable with
/// just a `Database`.
#[derive(Debug)]
pub(crate) struct NodeCopyPlan {
    pub(crate) source_id: String,
    pub(crate) node: GraphNode,
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) wiring: serde_json::Map<String, serde_json::Value>,
}

pub(crate) fn plan_node_copy(
    db: &Database,
    params: &GraphCopyNodeParams,
) -> Result<NodeCopyPlan, String> {
    let (source_id, source) = resolve_node_copy_source(db, params.source_node_id.trim())?;

    let target = resolve_copy_target(
        db,
        params.spec_id.as_deref(),
        params.graph_id.as_deref(),
        &source.spec_id,
        &source.graph_id,
    )?;
    let (spec_id, graph_id, existing_nodes) = graph_target_parts(db, &target)?;

    let config = merge_node_copy_config(&source, params)?;

    let name = params
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(source.name.as_str())
        .to_string();

    let new_id = uuid::Uuid::new_v4().to_string();
    let next_position = existing_nodes
        .last()
        .map(|node| node.position + 1)
        .unwrap_or(1);
    let node = GraphNode {
        id: new_id.clone(),
        spec_id: spec_id.clone(),
        graph_id: graph_id.clone(),
        name,
        kind: source.kind,
        config,
        position: next_position,
        created_at: chrono::Utc::now(),
    };

    let wiring = plan_node_copy_wiring(params, &spec_id, &graph_id, &new_id, &existing_nodes)?;

    Ok(NodeCopyPlan {
        source_id,
        node,
        edges: wiring.edges,
        wiring: wiring.wiring,
    })
}

/// Resolves the copy's source node by id prefix, refusing quorum nodes and
/// ensemble-owned nodes before any target lookup — the same order the
/// planner has always enforced.
pub(crate) fn resolve_node_copy_source(
    db: &Database,
    raw_source_id: &str,
) -> Result<(String, GraphNode), String> {
    let source_id = db
        .resolve_graph_node_id_by_prefix(raw_source_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Node '{raw_source_id}' not found."))?;
    let source = validate_node_exists(db, &source_id)?;
    if source.kind == GraphNodeKind::Join {
        return Err(
            "Cannot copy a quorum node directly; copy its ensemble with graph_copy_ensemble."
                .to_string(),
        );
    }
    if let Err(e) = validate_node_not_ensemble_owned(db, &source_id) {
        return Err(format!(
            "Cannot copy an ensemble-owned node directly; copy the whole ensemble with graph_copy_ensemble instead. {e}"
        ));
    }
    Ok((source_id, source))
}

/// Copies the source config, shallow-merging any override keys over it
/// (including the non-object source case), then validates the result for
/// the source kind. Called after target resolution, as before.
pub(crate) fn merge_node_copy_config(
    source: &GraphNode,
    params: &GraphCopyNodeParams,
) -> Result<serde_json::Value, String> {
    let mut config = source.config.clone();
    if let Some(overrides) = &params.config_overrides {
        match config.as_object_mut() {
            Some(map) => {
                for (key, value) in overrides {
                    map.insert(key.clone(), value.clone());
                }
            }
            None => config = serde_json::Value::Object(overrides.clone()),
        }
    }
    validate_node_config(source.kind, &config)?;
    Ok(config)
}

/// The copy's edges plus the wiring report the response carries. Built and
/// validated against the target graph in the order the planner has always
/// used: incoming entry edge first, then outgoing pass, then outgoing fail.
pub(crate) struct NodeCopyWiring {
    pub(crate) edges: Vec<GraphEdge>,
    pub(crate) wiring: serde_json::Map<String, serde_json::Value>,
}

pub(crate) fn plan_node_copy_wiring(
    params: &GraphCopyNodeParams,
    spec_id: &Option<String>,
    graph_id: &Option<String>,
    new_id: &str,
    existing_nodes: &[GraphNode],
) -> Result<NodeCopyWiring, String> {
    let node_exists = |id: &str| existing_nodes.iter().any(|n| n.id == id);
    let mut edges = Vec::new();
    let mut wiring = serde_json::Map::new();

    if let Some(from) = params
        .entry_from_node
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !node_exists(from) {
            return Err(format!(
                "entry_from_node '{from}' not found in the target graph."
            ));
        }
        let condition = match params.entry_condition.as_deref() {
            Some(value) => validate_edge_condition(value)?,
            None => GraphEdgeCondition::Always,
        };
        wiring.insert("entry_from_node".into(), serde_json::json!(from));
        wiring.insert(
            "entry_condition".into(),
            serde_json::json!(condition.as_str()),
        );
        edges.push(GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: spec_id.clone(),
            graph_id: graph_id.clone(),
            from_node: from.to_string(),
            to_node: new_id.to_string(),
            condition,
        });
    }
    for (field, target_node, cond) in [
        ("on_pass_to", &params.on_pass_to, GraphEdgeCondition::Pass),
        ("on_fail_to", &params.on_fail_to, GraphEdgeCondition::Fail),
    ] {
        if let Some(to) = target_node
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if !node_exists(to) {
                return Err(format!("{field} '{to}' not found in the target graph."));
            }
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: spec_id.clone(),
                graph_id: graph_id.clone(),
                from_node: new_id.to_string(),
                to_node: to.to_string(),
                condition: cond,
            });
            wiring.insert(field.to_string(), serde_json::json!(to));
        }
    }

    Ok(NodeCopyWiring { edges, wiring })
}

/// The `note` line a `graph_copy_node` response carries. An unwired copy is a
/// valid outcome, but callers must never assume edges exist — so the note says
/// so explicitly and points at how to wire it.
pub(crate) fn node_copy_note(source_id: &str, new_id: &str, wired: bool) -> String {
    if wired {
        format!("Copied node config from '{source_id}' into a new node '{new_id}'.")
    } else {
        format!(
            "Unwired copy of '{source_id}': the new node '{new_id}' has NO incoming or \
             outgoing edges yet. Wire it with graph_add_edge, or pass \
             entry_from_node/on_pass_to/on_fail_to."
        )
    }
}

/// A planned ensemble copy: the fully-built new unit plus the metadata needed
/// to report the old→new id mapping and the applied wiring. Pure of side
/// effects — the caller persists `built` with [`Database::insert_ensemble_unit`].
#[derive(Debug)]
pub(crate) struct EnsembleCopyPlan {
    pub(crate) source_ensemble_id: String,
    pub(crate) source_join_node_id: String,
    /// Source member node ids in position order — paired with the new member
    /// nodes for the mapping when members were not replaced.
    pub(crate) source_member_node_ids: Vec<String>,
    pub(crate) members_replaced: bool,
    pub(crate) built: BuiltEnsembleUnit,
    pub(crate) entry_from_node: String,
    pub(crate) entry_condition: GraphEdgeCondition,
    pub(crate) on_pass_to: String,
    pub(crate) on_fail_to: Option<String>,
}

/// Everything the ensemble-copy planner resolves from the source unit and
/// the params before wiring and quorum validation: the source details, the
/// target placement, and the copy's own name/prompt/member list.
pub(crate) struct EnsembleCopyInputs {
    pub(crate) details: crate::domain::graphs::EnsembleDetails,
    pub(crate) spec_id: Option<String>,
    pub(crate) graph_id: Option<String>,
    pub(crate) existing_nodes: Vec<GraphNode>,
    pub(crate) name: String,
    pub(crate) prompt_template: String,
    pub(crate) members_replaced: bool,
    pub(crate) copy_kind: EnsembleKind,
    pub(crate) members: Vec<EnsembleMemberSpec>,
}

/// The copy's exit wiring — resolved from params over the source's own
/// routing, with both nodes validated against the target graph first.
pub(crate) struct EnsembleCopyWiring {
    pub(crate) entry_from_node: String,
    pub(crate) entry_condition: GraphEdgeCondition,
    pub(crate) on_pass_to: String,
    pub(crate) on_fail_to: Option<String>,
}

/// The quorum knobs the copy ends up with, each validated (bounds and
/// non-negativity) before the unit is built.
pub(crate) struct EnsembleQuorumOptions {
    pub(crate) min_pass: i64,
    pub(crate) timeout_minutes: i64,
    pub(crate) straggler_timeout_minutes: Option<i64>,
    pub(crate) quorum_grace_minutes: Option<i64>,
}

pub(crate) fn plan_ensemble_copy(
    db: &Database,
    params: &GraphCopyEnsembleParams,
) -> Result<EnsembleCopyPlan, String> {
    let inputs = resolve_ensemble_copy_inputs(db, params)?;
    let source = &inputs.details.ensemble;

    let wiring = resolve_ensemble_copy_wiring(db, params, source, &inputs.existing_nodes)?;
    let quorum = resolve_ensemble_quorum_options(params, source, inputs.members.len())?;

    let built = build_ensemble_copy_unit(&inputs, &wiring, &quorum);

    Ok(EnsembleCopyPlan {
        source_ensemble_id: source.id.clone(),
        source_join_node_id: source.join_node_id.clone(),
        source_member_node_ids: inputs
            .details
            .members
            .iter()
            .map(|m| m.node_id.clone())
            .collect(),
        members_replaced: inputs.members_replaced,
        built,
        entry_from_node: wiring.entry_from_node,
        entry_condition: wiring.entry_condition,
        on_pass_to: wiring.on_pass_to,
        on_fail_to: wiring.on_fail_to,
    })
}

/// Source unit + target placement + the copy's own name, prompt and member
/// list (either the params' explicit members, or the source's own members
/// mapped through unchanged).
pub(crate) fn resolve_ensemble_copy_inputs(
    db: &Database,
    params: &GraphCopyEnsembleParams,
) -> Result<EnsembleCopyInputs, String> {
    let source_id = db
        .resolve_ensemble_id_by_prefix(params.source_ensemble_id.trim())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Ensemble '{}' not found.", params.source_ensemble_id.trim()))?;
    let details = db
        .get_ensemble_details(&source_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Ensemble '{source_id}' not found."))?;
    let source = &details.ensemble;

    let target = resolve_copy_target(
        db,
        params.spec_id.as_deref(),
        params.graph_id.as_deref(),
        &source.spec_id,
        &source.graph_id,
    )?;
    let (spec_id, graph_id, existing_nodes) = graph_target_parts(db, &target)?;

    let name = params
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{} (copy)", source.name));
    let prompt_template = params
        .prompt_template
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(source.prompt_template.as_str())
        .to_string();

    let copy_kind = source.kind;
    let (members, members_replaced) = copy_ensemble_members(&details, params, copy_kind)?;

    Ok(EnsembleCopyInputs {
        details,
        spec_id,
        graph_id,
        existing_nodes,
        name,
        prompt_template,
        members_replaced,
        copy_kind,
        members,
    })
}

/// The copy's member list: explicit params validated for the copy kind, or
/// the source's own members mapped through unchanged. Returns the members
/// plus whether they were replaced.
pub(crate) fn copy_ensemble_members(
    details: &crate::domain::graphs::EnsembleDetails,
    params: &GraphCopyEnsembleParams,
    copy_kind: EnsembleKind,
) -> Result<(Vec<EnsembleMemberSpec>, bool), String> {
    let members_replaced = params.members.is_some();
    let members: Vec<EnsembleMemberSpec> = match &params.members {
        Some(explicit) => validate_ensemble_members(explicit, copy_kind)?,
        None => details
            .members
            .iter()
            .map(|m| {
                (
                    m.platform.clone(),
                    m.model.clone(),
                    m.prompt_override.clone(),
                    m.timeout_minutes,
                    m.infra_retry_limit,
                    m.infra_crash_max_seconds,
                    m.infra_backoff_seconds,
                )
            })
            .collect(),
    };
    Ok((members, members_replaced))
}

/// Entry/pass/fail routing for the copy: params win over the source's own
/// routing, and every target node must exist in the *target* graph
/// (respecting that graph's own validation for a cross-graph copy) and must
/// not be ensemble-owned — the same rules graph_add_ensemble enforces.
pub(crate) fn resolve_ensemble_copy_wiring(
    db: &Database,
    params: &GraphCopyEnsembleParams,
    source: &Ensemble,
    existing_nodes: &[GraphNode],
) -> Result<EnsembleCopyWiring, String> {
    let entry_from_node = params
        .from_node
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(source.entry_from_node.as_str())
        .to_string();
    let entry_condition = match params
        .condition
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(value) => validate_edge_condition(value)?,
        None => source.entry_condition.clone(),
    };
    let on_pass_to = params
        .on_pass_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(source.on_pass_to.as_str())
        .to_string();
    let on_fail_to = match params
        .on_fail_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(value) => Some(value.to_string()),
        None => source.on_fail_to.clone(),
    };

    for (label, id) in [("from_node", &entry_from_node), ("on_pass_to", &on_pass_to)] {
        validate_ensemble_copy_wiring(db, existing_nodes, label, id)?;
    }
    if let Some(fail) = &on_fail_to {
        validate_ensemble_copy_wiring(db, existing_nodes, "on_fail_to", fail)?;
    }

    Ok(EnsembleCopyWiring {
        entry_from_node,
        entry_condition,
        on_pass_to,
        on_fail_to,
    })
}

/// Validates one quorum exit target against the target graph: it must exist
/// there and must not be ensemble-owned — the same rules
/// graph_add_ensemble enforces.
pub(crate) fn validate_ensemble_copy_wiring(
    db: &Database,
    existing_nodes: &[GraphNode],
    label: &str,
    id: &str,
) -> Result<(), String> {
    if !existing_nodes.iter().any(|n| n.id == id) {
        if label == "on_fail_to" {
            return Err(format!(
                "Wiring node '{id}' (on_fail_to) not found in the target graph."
            ));
        }
        return Err(format!(
            "Wiring node '{id}' ({label}) not found in the target graph. Pass {label} \
             that exists there (required for a cross-graph copy)."
        ));
    }
    if let Err(e) = validate_node_not_ensemble_owned(db, id) {
        return Err(format!(
            "Cannot wire an ensemble to an ensemble-owned node ({label}): {e}"
        ));
    }
    Ok(())
}

/// Assembles the copy's new ensemble unit from resolved inputs, wiring and
/// quorum options. `commit_rights` is never copied (see CM28 note inline).
pub(crate) fn build_ensemble_copy_unit(
    inputs: &EnsembleCopyInputs,
    wiring: &EnsembleCopyWiring,
    quorum: &EnsembleQuorumOptions,
) -> BuiltEnsembleUnit {
    let source = &inputs.details.ensemble;
    let start_position = inputs
        .existing_nodes
        .last()
        .map(|node| node.position + 1)
        .unwrap_or(1);
    build_ensemble_unit(&EnsembleUnitSpec {
        spec_id: inputs.spec_id.clone(),
        graph_id: inputs.graph_id.clone(),
        name: &inputs.name,
        prompt_template: &inputs.prompt_template,
        members: &inputs.members,
        entry_from_node: &wiring.entry_from_node,
        entry_condition: wiring.entry_condition.clone(),
        on_pass_to: &wiring.on_pass_to,
        on_pass_fan_out: None,
        on_fail_to: wiring.on_fail_to.as_deref(),
        on_fail_fan_out: None,
        min_pass: quorum.min_pass,
        timeout_minutes: quorum.timeout_minutes,
        straggler_timeout_minutes: quorum.straggler_timeout_minutes,
        quorum_grace_minutes: quorum.quorum_grace_minutes,
        infra_retry_limit: source.infra_retry_limit,
        infra_crash_max_seconds: source.infra_crash_max_seconds,
        infra_backoff_seconds: source.infra_backoff_seconds,
        start_position,
        kind: inputs.copy_kind,
        // CM28: never copied from the source — graph_copy_ensemble is out of
        // scope for commit rights, and silently duplicating the designation
        // would create two committers by accident. Set explicitly afterward
        // via graph_update_ensemble if the copy should also commit.
        commit_rights: false,
    })
}

/// Quorum limits for the copy — `min_pass` within the member count, and no
/// negative timeout/straggler/grace values, in that order.
pub(crate) fn resolve_ensemble_quorum_options(
    params: &GraphCopyEnsembleParams,
    source: &Ensemble,
    member_count: usize,
) -> Result<EnsembleQuorumOptions, String> {
    let min_pass = params.min_pass.unwrap_or(source.min_pass);
    if min_pass < 1 || min_pass > member_count as i64 {
        return Err(format!(
            "min_pass must be between 1 and {} (the member count), got {min_pass}.",
            member_count
        ));
    }
    let timeout_minutes = params.timeout_minutes.unwrap_or(source.timeout_minutes);
    if timeout_minutes < 0 {
        return Err("timeout_minutes must not be negative.".to_string());
    }
    let straggler_timeout_minutes = params
        .straggler_timeout_minutes
        .or(source.straggler_timeout_minutes);
    if let Some(straggler) = straggler_timeout_minutes {
        if straggler < 0 {
            return Err("straggler_timeout_minutes must not be negative.".to_string());
        }
    }
    let quorum_grace_minutes = params.quorum_grace_minutes.or(source.quorum_grace_minutes);
    if let Some(grace) = quorum_grace_minutes {
        if grace < 0 {
            return Err("quorum_grace_minutes must not be negative.".to_string());
        }
    }

    Ok(EnsembleQuorumOptions {
        min_pass,
        timeout_minutes,
        straggler_timeout_minutes,
        quorum_grace_minutes,
    })
}
