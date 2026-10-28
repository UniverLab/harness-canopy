//! Section helpers for `graph_add_ensemble_impl`, split out so the
//! orchestrator stays within the size gate. Behaviour unchanged: every
//! helper performs the same reads and refuses with the same messages.

use crate::daemon::handler_edges::{resolve_graph_target, GraphTarget};
use crate::daemon::handler_ensemble_core::{default_member_timeout, resolve_ensemble_members};
use crate::daemon::handler_ensemble_plan::{
    resolve_chained_exit_ensemble, BuiltEnsembleUnit, EnsembleExitTarget,
};
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_node_not_ensemble_owned, validate_non_empty,
};
use crate::daemon::helpers::EarlyReturn;
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::blueprints::EnsembleBlueprint;
use crate::domain::canopy_config::GraphsConfig;
use crate::domain::graphs::{EnsembleKind, EnsembleMemberSpec, GraphNode, MemberCaps};
use crate::domain::nodes::EnsembleNodeConfig;

/// Where the ensemble's prompt, members and shape come from: explicit
/// params, a blueprint, or an ensemble node document.
pub(crate) struct EnsembleSources {
    pub name: String,
    pub blueprint: Option<EnsembleBlueprint>,
    pub ensemble_node: Option<(String, EnsembleNodeConfig)>,
}

pub(crate) fn resolve_ensemble_sources(
    db: &Database,
    params: &GraphAddEnsembleParams,
) -> Result<EnsembleSources, EarlyReturn> {
    let name = params.name.trim();
    if let Err(e) = validate_non_empty(name, "Ensemble name") {
        return Err(EarlyReturn::tool_error(&e));
    }
    let blueprint_name = params
        .blueprint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let node_name = params
        .node
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if node_name.is_some() && (blueprint_name.is_some() || params.crew.is_some()) {
        return Err(EarlyReturn::tool_error(
            "Pass node or blueprint/crew, never both: an ensemble node document supplies the prompt and members on its own.",
        ));
    }
    let ensemble_node = match node_name {
        Some(node_name) => {
            match crate::daemon::handler_node_lib::resolve_ensemble_node_document(node_name) {
                Ok(document) => Some(document),
                Err(e) => return Err(EarlyReturn::tool_error(&e)),
            }
        }
        None => None,
    };
    let blueprint = match blueprint_name {
        Some(blueprint_name) => match db
            .get_ensemble_blueprint_by_name(blueprint_name)
            .map_err(internal_error)?
        {
            Some(bp) => Some(bp),
            None => {
                return Err(EarlyReturn::tool_error(&format!(
                    "Unknown ensemble blueprint '{blueprint_name}'."
                )))
            }
        },
        None => None,
    };
    Ok(EnsembleSources {
        name: name.to_string(),
        blueprint,
        ensemble_node,
    })
}

pub(crate) fn resolve_shared_prompt(
    params: &GraphAddEnsembleParams,
    blueprint: Option<&EnsembleBlueprint>,
    node_config: Option<&EnsembleNodeConfig>,
) -> Result<String, EarlyReturn> {
    match params
        .prompt_template
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(explicit) => Ok(explicit.to_string()),
        None => match (blueprint, node_config) {
            (Some(bp), _) => Ok(bp.prompt_template.clone()),
            (None, Some(config)) => Ok(config.prompt_template.clone()),
            (None, None) => Err(EarlyReturn::tool_error(
                "Provide prompt_template, or a blueprint or node document that supplies one.",
            )),
        },
    }
}

pub(crate) fn resolve_ensemble_kind(
    params: &GraphAddEnsembleParams,
    node_config: Option<&EnsembleNodeConfig>,
) -> Result<EnsembleKind, EarlyReturn> {
    match params
        .kind
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some("parallel") => Ok(EnsembleKind::Parallel),
        Some("cascade") => Ok(EnsembleKind::Cascade),
        Some("round_robin") => Ok(EnsembleKind::RoundRobin),
        Some(other) => Err(EarlyReturn::tool_error(&format!(
            "Invalid ensemble kind '{other}'. Must be one of: parallel, cascade, round_robin."
        ))),
        None => match node_config {
            Some(config) => match config.strategy() {
                Ok(kind) => Ok(kind),
                Err(e) => Err(EarlyReturn::tool_error(&e)),
            },
            None => Ok(EnsembleKind::Parallel),
        },
    }
}

/// The resolved member list plus the crew binding to stamp after insert.
pub(crate) struct EnsembleMemberList {
    pub members: Vec<EnsembleMemberSpec>,
    pub crew_name: Option<String>,
}

/// Everything the add orchestrator needs before wiring: the shared
/// prompt, the strategy, the members, the crew binding, and the
/// quorum/timeout/infra budgets.
pub(crate) struct EnsembleBuildPlan {
    pub prompt_template: String,
    pub kind: EnsembleKind,
    pub members: Vec<EnsembleMemberSpec>,
    pub crew_name: Option<String>,
    pub timeouts: EnsembleTimeouts,
}

pub(crate) fn resolve_ensemble_build_plan(
    db: &Database,
    params: &GraphAddEnsembleParams,
    sources: &EnsembleSources,
    graphs: &GraphsConfig,
    caps: MemberCaps,
) -> Result<EnsembleBuildPlan, EarlyReturn> {
    let node_config = sources.ensemble_node.as_ref().map(|(_, config)| config);
    let prompt_template = resolve_shared_prompt(params, sources.blueprint.as_ref(), node_config)?;
    let kind = resolve_ensemble_kind(params, node_config)?;
    let member_list = resolve_ensemble_member_list(
        db,
        params,
        sources.blueprint.as_ref(),
        sources.ensemble_node.as_ref(),
        kind,
        caps,
    )?;
    let timeouts = resolve_ensemble_timeouts(
        params,
        &sources.name,
        node_config,
        sources.blueprint.as_ref(),
        kind,
        graphs,
        member_list.members.len(),
    )?;
    Ok(EnsembleBuildPlan {
        prompt_template,
        kind,
        members: member_list.members,
        crew_name: member_list.crew_name,
        timeouts,
    })
}

/// The resolved wiring: entry node, both exits, owner ids, and the
/// next free position.
pub(crate) struct EnsembleWiring {
    pub from_node: String,
    pub on_pass_target: EnsembleExitTarget,
    pub on_fail_target: Option<EnsembleExitTarget>,
    pub spec_id: Option<String>,
    pub graph_id: Option<String>,
    pub start_position: i64,
}

pub(crate) fn resolve_ensemble_wiring(
    db: &Database,
    params: &GraphAddEnsembleParams,
) -> Result<EnsembleWiring, EarlyReturn> {
    let (target, existing_nodes) = resolve_wiring_target(db, params)?;
    let from_node = resolve_from_node(db, params, &existing_nodes)?;
    let (spec_id, graph_id) = match &target {
        GraphTarget::Spec(spec_id) => (Some(spec_id.clone()), None),
        GraphTarget::Graph(graph_id) => (None, Some(graph_id.clone())),
    };
    let on_pass_target = resolve_exit_target(
        db,
        params.on_pass_to.trim(),
        spec_id.as_deref(),
        graph_id.as_deref(),
        &existing_nodes,
        "on_pass_to",
        true,
    )?;
    let on_fail_target: Option<EnsembleExitTarget> = match &params.on_fail_to {
        Some(raw) => Some(resolve_exit_target(
            db,
            raw.trim(),
            spec_id.as_deref(),
            graph_id.as_deref(),
            &existing_nodes,
            "on_fail_to",
            false,
        )?),
        None => None,
    };
    let start_position = existing_nodes
        .last()
        .map(|node| node.position + 1)
        .unwrap_or(1);
    Ok(EnsembleWiring {
        from_node,
        on_pass_target,
        on_fail_target,
        spec_id,
        graph_id,
        start_position,
    })
}

pub(crate) fn resolve_ensemble_member_list(
    db: &Database,
    params: &GraphAddEnsembleParams,
    blueprint: Option<&EnsembleBlueprint>,
    ensemble_node: Option<&(String, EnsembleNodeConfig)>,
    kind: EnsembleKind,
    caps: MemberCaps,
) -> Result<EnsembleMemberList, EarlyReturn> {
    // CM49: a crew binding is mutually exclusive with a literal member
    // list (explicit or blueprint-supplied). When bound, the crew's
    // ordered members seed the unit; the binding is stamped after insert.
    let crew_name = params
        .crew
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if crew_name.is_some()
        && (params.members.is_some() || blueprint.is_some_and(|bp| !bp.members.is_empty()))
    {
        return Err(EarlyReturn::tool_error("pass crew or members, never both."));
    }
    let members: Vec<EnsembleMemberSpec> = match crew_name {
        Some(name) => match db.crew_get(name).map_err(internal_error)? {
            Some((_crew, crew_members)) => crew_members
                .iter()
                .map(|member| {
                    (
                        member.platform.clone(),
                        member.model.clone(),
                        None,
                        member.timeout_minutes,
                        member.infra_retry_limit,
                        member.infra_crash_max_seconds,
                        member.infra_backoff_seconds,
                    )
                })
                .collect(),
            None => {
                return Err(EarlyReturn::tool_error(&format!(
                    "crew '{name}' not found. Create it with `crew create` first."
                )))
            }
        },
        None => match resolve_ensemble_members(
            params.members.as_ref(),
            ensemble_node,
            blueprint,
            kind,
            caps,
        ) {
            Ok(members) => members,
            Err(e) => return Err(EarlyReturn::tool_error(&e)),
        },
    };
    // Blueprint-supplied members bypass `validate_ensemble_members` above
    // (they are already `EnsembleMemberSpec`), so enforce the requested
    // kind's bounds here for both paths — a 12-member blueprint used
    // with kind parallel is refused exactly like 12 explicit members.
    let (min, max) = crate::domain::graphs::member_bounds(kind, caps);
    if members.len() < min || members.len() > max {
        return Err(EarlyReturn::tool_error(
            &crate::domain::graphs::member_count_message(kind, caps, members.len()),
        ));
    }
    Ok(EnsembleMemberList {
        members,
        crew_name: crew_name.map(str::to_string),
    })
}

/// The target graph and its current nodes for membership checks.
pub(crate) fn resolve_wiring_target(
    db: &Database,
    params: &GraphAddEnsembleParams,
) -> Result<(GraphTarget, Vec<GraphNode>), EarlyReturn> {
    let target =
        match resolve_graph_target(db, params.spec_id.as_deref(), params.graph_id.as_deref()) {
            Ok(target) => target,
            Err(e) => return Err(EarlyReturn::tool_error(&e)),
        };
    let existing_nodes = match &target {
        GraphTarget::Spec(spec_id) => db.list_graph_nodes(spec_id).map_err(internal_error)?,
        GraphTarget::Graph(graph_id) => db
            .list_graph_nodes_for_graph(graph_id)
            .map_err(internal_error)?,
    };
    Ok((target, existing_nodes))
}

pub(crate) fn resolve_from_node(
    db: &Database,
    params: &GraphAddEnsembleParams,
    existing_nodes: &[GraphNode],
) -> Result<String, EarlyReturn> {
    let from_node = resolve_prefix_or_error(
        db,
        params.from_node.trim(),
        Database::resolve_graph_node_id_by_prefix,
        "node",
    )
    .map_err(EarlyReturn::ok)?;
    if !existing_nodes.iter().any(|node| node.id == from_node) {
        return Err(EarlyReturn::tool_error(&format!(
            "Graph node '{from_node}' not found in the target graph."
        )));
    }
    if let Err(e) = validate_node_not_ensemble_owned(db, &from_node) {
        return Err(EarlyReturn::tool_error(&format!(
            "Cannot wire an ensemble's entry from an ensemble-owned node: {e} To chain ensembles, create this one from a plain node and then route the upstream ensemble's exit into it (graph_ensemble update: on_pass_to/on_fail_to accept an ensemble id)."
        )));
    }
    Ok(from_node)
}

/// An exit target is a plain node — or, to chain ensembles with no
/// intermediate node, another ensemble's id (the quorum fans out to
/// every member of that ensemble). Shared by `on_pass_to` (required)
/// and `on_fail_to` (optional); `exit_param` names the caller in
/// refusal messages.
pub(crate) fn resolve_exit_target(
    db: &Database,
    raw: &str,
    owner_spec_id: Option<&str>,
    owner_graph_id: Option<&str>,
    existing_nodes: &[GraphNode],
    exit_param: &str,
    require_non_empty: bool,
) -> Result<EnsembleExitTarget, EarlyReturn> {
    match resolve_chained_exit_ensemble(db, raw.trim(), "", owner_spec_id, owner_graph_id) {
        Ok(Some(target)) => Ok(target),
        Ok(None) => {
            let id = resolve_prefix_or_error(
                db,
                raw.trim(),
                Database::resolve_graph_node_id_by_prefix,
                "node",
            )
            .map_err(EarlyReturn::ok)?;
            if require_non_empty {
                if let Err(e) = validate_non_empty(&id, exit_param) {
                    return Err(EarlyReturn::tool_error(&e));
                }
            }
            if !existing_nodes.iter().any(|node| node.id == id) {
                return Err(EarlyReturn::tool_error(&format!(
                    "Graph node '{id}' not found in the target graph."
                )));
            }
            if let Err(e) = validate_node_not_ensemble_owned(db, &id) {
                return Err(EarlyReturn::tool_error(&format!(
                    "Cannot wire an ensemble's exit directly into another ensemble's members/quorum: {e} To route into the whole ensemble, pass its ensemble id as {exit_param} instead of a member/quorum node id."
                )));
            }
            Ok(EnsembleExitTarget::Node(id))
        }
        Err(e) => Err(EarlyReturn::tool_error(&e)),
    }
}

/// Row targets and fan-out edge lists for both quorum exits.
pub(crate) struct ExitRowTargets {
    pub on_pass_row: String,
    pub on_pass_fan_out: Option<Vec<String>>,
    pub on_fail_row: Option<String>,
    pub on_fail_fan_out: Option<Vec<String>>,
}

pub(crate) fn exit_row_targets(
    on_pass_target: &EnsembleExitTarget,
    on_fail_target: &Option<EnsembleExitTarget>,
) -> ExitRowTargets {
    let on_pass_fan_out = match on_pass_target {
        EnsembleExitTarget::Node(_) => None,
        EnsembleExitTarget::Ensemble { .. } => Some(on_pass_target.edge_targets()),
    };
    let (on_fail_row, on_fail_fan_out): (Option<String>, Option<Vec<String>>) = match on_fail_target
    {
        None => (None, None),
        Some(EnsembleExitTarget::Node(id)) => (Some(id.clone()), None),
        Some(target) => (Some(target.row_target()), Some(target.edge_targets())),
    };
    ExitRowTargets {
        on_pass_row: on_pass_target.row_target(),
        on_pass_fan_out,
        on_fail_row,
        on_fail_fan_out,
    }
}

/// Quorum, timeout and infra budgets: explicit params win, then the node
/// document's, then the blueprint's, then the engine default.
pub(crate) struct EnsembleTimeouts {
    pub mutex: Vec<String>,
    pub min_pass: i64,
    pub timeout_minutes: i64,
    pub straggler_timeout_minutes: Option<i64>,
    pub quorum_grace_minutes: Option<i64>,
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

pub(crate) fn resolve_ensemble_timeouts(
    params: &GraphAddEnsembleParams,
    name: &str,
    node_config: Option<&EnsembleNodeConfig>,
    blueprint: Option<&EnsembleBlueprint>,
    kind: EnsembleKind,
    graphs: &GraphsConfig,
    members_len: usize,
) -> Result<EnsembleTimeouts, EarlyReturn> {
    let mutex = params
        .mutex
        .clone()
        .or_else(|| node_config.and_then(|config| config.mutex.clone()))
        .unwrap_or_default();
    let min_pass = params
        .min_pass
        .or_else(|| node_config.and_then(|config| config.min_pass))
        .or_else(|| blueprint.and_then(|bp| bp.min_pass))
        .unwrap_or(members_len as i64);
    if let Err(e) =
        crate::domain::validation::validate_ensemble_min_pass(name, min_pass, members_len)
    {
        return Err(EarlyReturn::tool_error(&e));
    }
    if let Err(e) = crate::domain::validation::validate_mutex_list(
        &format!("Ensemble '{name}'"),
        Some(mutex.as_slice()),
    ) {
        return Err(EarlyReturn::tool_error(&e));
    }
    let timeout_minutes = default_member_timeout(
        graphs,
        params
            .timeout_minutes
            .or_else(|| node_config.and_then(|config| config.timeout_minutes)),
    );
    if timeout_minutes < 0 {
        return Err(EarlyReturn::tool_error(
            "timeout_minutes must not be negative.",
        ));
    }
    let straggler_timeout_minutes = params
        .straggler_timeout_minutes
        .or_else(|| node_config.and_then(|config| config.straggler_timeout_minutes));
    refuse_sequential_grace_field(
        kind,
        "straggler_timeout_minutes",
        straggler_timeout_minutes,
        "it only bounds how long a parallel quorum waits for stragglers",
    )?;
    let quorum_grace_minutes = params
        .quorum_grace_minutes
        .or_else(|| node_config.and_then(|config| config.quorum_grace_minutes))
        .or_else(|| blueprint.and_then(|bp| bp.quorum_grace_minutes));
    refuse_sequential_grace_field(
        kind,
        "quorum_grace_minutes",
        quorum_grace_minutes,
        "it only applies once a parallel quorum is met",
    )?;
    let infra_retry_limit = params
        .infra_retry_limit
        .or_else(|| node_config.and_then(|config| config.infra_retry_limit));
    let infra_crash_max_seconds = params
        .infra_crash_max_seconds
        .or_else(|| node_config.and_then(|config| config.infra_crash_max_seconds));
    let infra_backoff_seconds = params
        .infra_backoff_seconds
        .or_else(|| node_config.and_then(|config| config.infra_backoff_seconds));
    for (field_name, value) in [
        ("infra_retry_limit", infra_retry_limit),
        ("infra_crash_max_seconds", infra_crash_max_seconds),
        ("infra_backoff_seconds", infra_backoff_seconds),
    ] {
        if value.is_some_and(|v| v < 0) {
            return Err(EarlyReturn::tool_error(&format!(
                "{field_name} must not be negative."
            )));
        }
    }
    Ok(EnsembleTimeouts {
        mutex,
        min_pass,
        timeout_minutes,
        straggler_timeout_minutes,
        quorum_grace_minutes,
        infra_retry_limit,
        infra_crash_max_seconds,
        infra_backoff_seconds,
    })
}

/// CB67/FR3: a parallel-quorum concept — meaningless once members
/// run one at a time.
fn refuse_sequential_grace_field(
    kind: EnsembleKind,
    field_name: &str,
    value: Option<i64>,
    meaning: &str,
) -> Result<(), EarlyReturn> {
    let Some(field_value) = value else {
        return Ok(());
    };
    if field_value < 0 {
        return Err(EarlyReturn::tool_error(&format!(
            "{field_name} must not be negative."
        )));
    }
    if kind != EnsembleKind::Parallel {
        return Err(EarlyReturn::tool_error(&format!(
            "{field_name} has no meaning for a {} ensemble ({meaning}); omit it, or set kind: \"parallel\".",
            kind.as_str()
        )));
    }
    Ok(())
}

/// The shared prompt and every member override are refused here when
/// they carry a placeholder nothing can bind — the write that used to
/// be accepted and then fail member by member at spawn time. The new
/// member and quorum nodes are already bindable: at spawn they are part
/// of the graph's node names.
pub(crate) fn check_ensemble_templates(
    db: &Database,
    built: &BuiltEnsembleUnit,
    members: &[EnsembleMemberSpec],
    name: &str,
    prompt_template: &str,
) -> Result<(), EarlyReturn> {
    let scope = crate::daemon::handler_templates::TemplateScope::load(
        db,
        built.ensemble.spec_id.as_deref(),
        built.ensemble.graph_id.as_deref(),
    )
    .map_err(internal_error)?;
    let mut node_names = scope.nodes;
    node_names.extend(built.member_nodes.iter().map(|n| n.name.clone()));
    node_names.push(built.join_node.name.clone());
    let mut ensembles = scope.ensembles;
    ensembles.push((
        built.ensemble.name.clone(),
        Some(built.join_node.name.clone()),
    ));
    let owner = format!("Ensemble '{}' shared prompt template", built.ensemble.name);
    if let Err(e) = crate::daemon::handler_templates::check_template_bindings(
        &owner,
        prompt_template,
        crate::graph_engine::AGENT_PROMPT_BINDINGS,
        &node_names,
        &ensembles,
    ) {
        return Err(EarlyReturn::tool_error(&e));
    }
    for (index, member) in members.iter().enumerate() {
        if let Some(template) = member.2.as_deref() {
            let member_owner = format!("Ensemble '{name}' member {} prompt override", index + 1);
            if let Err(e) = crate::daemon::handler_templates::check_template_bindings(
                &member_owner,
                template,
                crate::graph_engine::AGENT_PROMPT_BINDINGS,
                &node_names,
                &ensembles,
            ) {
                return Err(EarlyReturn::tool_error(&e));
            }
        }
    }
    Ok(())
}
