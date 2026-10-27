//! Pure ensemble planning layer split out of `daemon::handler_ensemble_core`
//! (unit/member/exit-edge builders and chained-exit resolution; no `&self`,
//! no DB writes). Moved verbatim; behaviour unchanged.

use crate::daemon::handler_ensemble_core::{effective_member_prompt, member_node_config};
use crate::daemon::handler_specs_helpers::validate_at_least_one_bool;
use crate::daemon::helpers::error_result;
use crate::daemon::params::GraphUpdateEnsembleParams;
use crate::db::Database;
use crate::domain::graphs::EnsembleDetails;
use crate::domain::graphs::{
    Ensemble, EnsembleKind, EnsembleMember, EnsembleMemberSpec, GraphEdge, GraphEdgeCondition,
    GraphNode, GraphNodeKind,
};
use rmcp::model::CallToolResult;

/// Validated inputs for assembling one ensemble unit — see
/// [`build_ensemble_unit`]. All wiring targets (`entry_from_node`,
/// `on_pass_to`, `on_fail_to`) must already have been checked to exist in the
/// target graph by the caller.
pub(crate) struct EnsembleUnitSpec<'a> {
    pub(crate) spec_id: Option<String>,
    pub(crate) graph_id: Option<String>,
    pub(crate) name: &'a str,
    pub(crate) prompt_template: &'a str,
    pub(crate) members: &'a [EnsembleMemberSpec],
    pub(crate) entry_from_node: &'a str,
    pub(crate) entry_condition: GraphEdgeCondition,
    pub(crate) on_pass_to: &'a str,
    /// When the pass exit chains into another ensemble: that ensemble's
    /// member ids — the quorum fans out to all of them while `on_pass_to`
    /// holds the target's quorum id. `None` means a plain single-node exit.
    pub(crate) on_pass_fan_out: Option<Vec<String>>,
    pub(crate) on_fail_to: Option<&'a str>,
    /// Same as `on_pass_fan_out`, for the fail exit.
    pub(crate) on_fail_fan_out: Option<Vec<String>>,
    pub(crate) min_pass: i64,
    pub(crate) timeout_minutes: i64,
    pub(crate) straggler_timeout_minutes: Option<i64>,
    pub(crate) quorum_grace_minutes: Option<i64>,
    pub(crate) infra_retry_limit: Option<i64>,
    pub(crate) infra_crash_max_seconds: Option<i64>,
    pub(crate) infra_backoff_seconds: Option<i64>,
    pub(crate) start_position: i64,
    pub(crate) kind: EnsembleKind,
    pub(crate) commit_rights: bool,
    pub(crate) mutex: Vec<String>,
}

/// The concrete graph pieces of one ensemble unit, all with fresh ids: the
/// join node, member nodes, wiring edges (entry fan-out, member→join fan-in,
/// join exit routing), the ensemble row, and its member rows.
#[derive(Debug)]
pub(crate) struct BuiltEnsembleUnit {
    pub(crate) ensemble: Ensemble,
    pub(crate) members: Vec<EnsembleMember>,
    pub(crate) member_nodes: Vec<GraphNode>,
    pub(crate) join_node: GraphNode,
    pub(crate) edges: Vec<GraphEdge>,
}

/// Assemble an ensemble unit from validated inputs. Shared by
/// `graph_add_ensemble` and `graph_copy_ensemble` so the two can never drift in
/// how members, the join, and the wiring are laid out. Purely constructs
/// in-memory values (fresh ids, no runtime state) — persistence is the
/// caller's `insert_ensemble_unit`.
/// One member's contribution to an ensemble unit: its agent node, its
/// member row, and the two edges that wire it (entry fan-out in, fan-in to
/// the join). Built as one unit so the id-generation order inside
/// [`build_ensemble_unit`] stays node → incoming edge → fan-in edge, per
/// member.
pub(crate) struct BuiltEnsembleMember {
    pub(crate) node: GraphNode,
    pub(crate) member: EnsembleMember,
    pub(crate) edges: Vec<GraphEdge>,
}

pub(crate) fn build_ensemble_unit(spec: &EnsembleUnitSpec) -> BuiltEnsembleUnit {
    let ensemble_id = uuid::Uuid::new_v4().to_string();
    let join_node_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now();
    let mut next_position = spec.start_position;

    let mut member_nodes = Vec::with_capacity(spec.members.len());
    let mut ensemble_members = Vec::with_capacity(spec.members.len());
    let mut edges = Vec::new();

    for (index, member_spec) in spec.members.iter().enumerate() {
        let built = build_ensemble_member_unit(
            spec,
            &ensemble_id,
            &join_node_id,
            now,
            index,
            next_position,
            member_spec,
        );
        member_nodes.push(built.node);
        ensemble_members.push(built.member);
        edges.extend(built.edges);
        next_position += 1;
    }

    let join_node = GraphNode {
        id: join_node_id.clone(),
        spec_id: spec.spec_id.clone(),
        graph_id: spec.graph_id.clone(),
        name: format!("{} (quorum)", spec.name),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": ensemble_id }),
        position: next_position,
        created_at: now,
    };

    edges.extend(build_ensemble_exit_edges(spec, &join_node_id));

    let ensemble = Ensemble {
        id: ensemble_id,
        spec_id: spec.spec_id.clone(),
        graph_id: spec.graph_id.clone(),
        name: spec.name.to_string(),
        prompt_template: spec.prompt_template.to_string(),
        join_node_id,
        entry_from_node: spec.entry_from_node.to_string(),
        entry_condition: spec.entry_condition.clone(),
        min_pass: spec.min_pass,
        straggler_timeout_minutes: spec.straggler_timeout_minutes,
        quorum_grace_minutes: spec.quorum_grace_minutes,
        timeout_minutes: spec.timeout_minutes,
        infra_retry_limit: spec.infra_retry_limit,
        infra_crash_max_seconds: spec.infra_crash_max_seconds,
        infra_backoff_seconds: spec.infra_backoff_seconds,
        on_pass_to: spec.on_pass_to.to_string(),
        on_fail_to: spec.on_fail_to.map(str::to_string),
        kind: spec.kind,
        round_robin_index: if spec.kind == EnsembleKind::RoundRobin {
            Some(0)
        } else {
            None
        },
        commit_rights: spec.commit_rights,
        mutex: spec.mutex.clone(),
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };

    BuiltEnsembleUnit {
        ensemble,
        members: ensemble_members,
        member_nodes,
        join_node,
        edges,
    }
}

/// One member unit: the agent node (named `{ensemble} [{index}]` and placed
/// at `position`), the entry fan-out edge from `entry_from_node`, the fan-in
/// edge to the join, and the member row — with the member's own
/// prompt/timeout/infra overrides falling back to the ensemble's shared
/// values exactly as before.
pub(crate) fn build_ensemble_member_unit(
    spec: &EnsembleUnitSpec,
    ensemble_id: &str,
    join_node_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    index: usize,
    position: i64,
    member_spec: &EnsembleMemberSpec,
) -> BuiltEnsembleMember {
    let (
        platform,
        model,
        prompt_override,
        member_timeout_minutes,
        member_infra_retry_limit,
        member_infra_crash_max_seconds,
        member_infra_backoff_seconds,
    ) = member_spec;
    let node_id = uuid::Uuid::new_v4().to_string();
    let effective_prompt =
        effective_member_prompt(prompt_override.as_deref(), spec.prompt_template);
    let effective_timeout_minutes = member_timeout_minutes.unwrap_or(spec.timeout_minutes);
    let effective_infra_retry_limit = member_infra_retry_limit.or(spec.infra_retry_limit);
    let effective_infra_crash_max_seconds =
        member_infra_crash_max_seconds.or(spec.infra_crash_max_seconds);
    let effective_infra_backoff_seconds =
        member_infra_backoff_seconds.or(spec.infra_backoff_seconds);

    let node = GraphNode {
        id: node_id.clone(),
        spec_id: spec.spec_id.clone(),
        graph_id: spec.graph_id.clone(),
        name: format!("{} [{}]", spec.name, index + 1),
        kind: GraphNodeKind::Agent,
        config: member_node_config(
            platform,
            model.as_deref(),
            effective_prompt,
            effective_timeout_minutes,
            effective_infra_retry_limit,
            effective_infra_crash_max_seconds,
            effective_infra_backoff_seconds,
        ),
        position,
        created_at: now,
    };

    let edges = vec![
        GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: spec.spec_id.clone(),
            graph_id: spec.graph_id.clone(),
            from_node: spec.entry_from_node.to_string(),
            to_node: node_id.clone(),
            condition: spec.entry_condition.clone(),
        },
        GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: spec.spec_id.clone(),
            graph_id: spec.graph_id.clone(),
            from_node: node_id.clone(),
            to_node: join_node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        },
    ];

    let member = EnsembleMember {
        ensemble_id: ensemble_id.to_string(),
        node_id,
        position: index as i64,
        platform: platform.clone(),
        model: model.clone(),
        prompt_override: prompt_override.clone(),
        timeout_minutes: *member_timeout_minutes,
        infra_retry_limit: *member_infra_retry_limit,
        infra_crash_max_seconds: *member_infra_crash_max_seconds,
        infra_backoff_seconds: *member_infra_backoff_seconds,
    };

    BuiltEnsembleMember {
        node,
        member,
        edges,
    }
}

/// The join's outgoing edges: one `Pass` edge per pass target (the single
/// `on_pass_to`, or one per member of the ensemble it chains into), then —
/// only when the unit declares a failure destination — one `Fail` edge per
/// fail target. The `ensembles` row still holds the single `on_pass_to`/
/// `on_fail_to` value (the node id, or the target's quorum id).
pub(crate) fn build_ensemble_exit_edges(
    spec: &EnsembleUnitSpec,
    join_node_id: &str,
) -> Vec<GraphEdge> {
    let mut edges = Vec::new();

    let pass_targets = spec
        .on_pass_fan_out
        .clone()
        .unwrap_or_else(|| vec![spec.on_pass_to.to_string()]);
    for to_node in &pass_targets {
        edges.push(GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: spec.spec_id.clone(),
            graph_id: spec.graph_id.clone(),
            from_node: join_node_id.to_string(),
            to_node: to_node.clone(),
            condition: GraphEdgeCondition::Pass,
        });
    }

    if let Some(on_fail_to) = &spec.on_fail_to {
        let fail_targets = spec
            .on_fail_fan_out
            .clone()
            .unwrap_or_else(|| vec![on_fail_to.to_string()]);
        for to_node in &fail_targets {
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: spec.spec_id.clone(),
                graph_id: spec.graph_id.clone(),
                from_node: join_node_id.to_string(),
                to_node: to_node.clone(),
                condition: GraphEdgeCondition::Fail,
            });
        }
    }

    edges
}

/// Where one quorum exit (pass or fail) points: a plain node, or a whole
/// other ensemble — the chained case, where the quorum fans out to every
/// member of the target with no intermediate node. The `ensembles` row keeps
/// naming the target in the same column either way (a node id, or the target
/// ensemble's quorum id when chained), so reads never need a new column.
pub(crate) enum EnsembleExitTarget {
    Node(String),
    Ensemble {
        join_node_id: String,
        member_ids: Vec<String>,
    },
}

impl EnsembleExitTarget {
    /// Edge targets for the fan-out: the single node, or every member of the
    /// chained ensemble.
    pub(crate) fn edge_targets(&self) -> Vec<String> {
        match self {
            EnsembleExitTarget::Node(id) => vec![id.clone()],
            EnsembleExitTarget::Ensemble { member_ids, .. } => member_ids.clone(),
        }
    }

    /// Value stored in the `ensembles` row's `on_pass_to`/`on_fail_to`
    /// column: the node id, or the chained ensemble's quorum id.
    pub(crate) fn row_target(&self) -> String {
        match self {
            EnsembleExitTarget::Node(id) => id.clone(),
            EnsembleExitTarget::Ensemble { join_node_id, .. } => join_node_id.clone(),
        }
    }
}

/// If `raw` names an ensemble (by id or prefix), resolve it as a chained exit
/// target: the target must live in the same graph (`owner_spec_id`/
/// `owner_graph_id`) and must not be the source ensemble itself. Returns
/// `Ok(None)` when `raw` names no ensemble, so the caller falls through to
/// its plain-node handling — node ids and unknown ids both take that path.
pub(crate) fn resolve_chained_exit_ensemble(
    db: &Database,
    raw: &str,
    source_ensemble_id: &str,
    owner_spec_id: Option<&str>,
    owner_graph_id: Option<&str>,
) -> Result<Option<EnsembleExitTarget>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let Some(target_id) = db
        .resolve_ensemble_id_by_prefix(trimmed)
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let details = db
        .get_ensemble_details(&target_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("Ensemble '{target_id}' not found."))?;
    if !source_ensemble_id.is_empty() && details.ensemble.id == source_ensemble_id {
        return Err(
            "An ensemble's exit cannot target itself; pass a different ensemble id or a node id."
                .to_string(),
        );
    }
    if details.ensemble.spec_id.as_deref() != owner_spec_id
        || details.ensemble.graph_id.as_deref() != owner_graph_id
    {
        return Err(format!(
            "Ensemble '{}' ('{}') is not in this ensemble's graph; chained exits stay within one graph.",
            details.ensemble.id, details.ensemble.name
        ));
    }
    if details.members.is_empty() {
        return Err(format!(
            "Ensemble '{}' ('{}') has no members to route into.",
            details.ensemble.id, details.ensemble.name
        ));
    }
    Ok(Some(EnsembleExitTarget::Ensemble {
        join_node_id: details.ensemble.join_node_id.clone(),
        member_ids: details.members.iter().map(|m| m.node_id.clone()).collect(),
    }))
}

/// Every update field `graph_update_ensemble` accepts, as presence flags —
/// one list shared by the empty-update refusal and the crew-combination
/// refusal, so the two can never drift apart.
fn ensemble_update_field_flags(params: &GraphUpdateEnsembleParams) -> [bool; 20] {
    [
        params.kind.is_some(),
        params.prompt_template.is_some(),
        params.members.is_some(),
        params.min_pass.is_some(),
        params.straggler_timeout_minutes.is_some(),
        params.quorum_grace_minutes.is_some(),
        params.timeout_minutes.is_some(),
        params.infra_retry_limit.is_some(),
        params.infra_crash_max_seconds.is_some(),
        params.infra_backoff_seconds.is_some(),
        params.on_pass_to.is_some(),
        params.on_fail_to.is_some(),
        params.from_node.is_some(),
        params.condition.is_some(),
        params.add_entry_from.is_some(),
        params.add_entry_condition.is_some(),
        params.remove_entry_from.is_some(),
        params.commit_rights.is_some(),
        params.crew.is_some(),
        params.mutex.is_some(),
    ]
}

pub(crate) fn reject_empty_ensemble_update(
    params: &GraphUpdateEnsembleParams,
) -> Result<(), String> {
    validate_at_least_one_bool(
        &ensemble_update_field_flags(params),
        "graph_update_ensemble",
    )
}

/// CM49: binding or unbinding a crew is its own operation. The crew arm
/// returns early, so any other field passed alongside it would be silently
/// dropped — refuse the combination instead of half-applying it.
pub(crate) fn reject_crew_update_combination(
    params: &GraphUpdateEnsembleParams,
) -> Result<(), String> {
    let flags = ensemble_update_field_flags(params);
    let others = flags.iter().filter(|set| **set).count();
    if others > 1 {
        return Err(
            "crew binds or unbinds on its own and cannot be combined with other ensemble \
             updates in one call; send the crew change and the other updates separately."
                .to_string(),
        );
    }
    Ok(())
}

pub(crate) fn validate_ensemble_infra_updates(
    params: &GraphUpdateEnsembleParams,
) -> Result<(), String> {
    // CM30: validated here, before the member-resize/propagate branches
    // below read these values to bake them into member configs and write
    // them via `replace_ensemble_members`/`update_graph_node_details` — a
    // rejected value must never land in the DB first. (The join-config
    // block further down still owns straggler_timeout_minutes/
    // quorum_grace_minutes/timeout_minutes, which only ever get written
    // in that same block, so their late validation has no such gap.)
    for (field_name, value) in [
        ("infra_retry_limit", params.infra_retry_limit),
        ("infra_crash_max_seconds", params.infra_crash_max_seconds),
        ("infra_backoff_seconds", params.infra_backoff_seconds),
    ] {
        if let Some(Some(v)) = value {
            if v < 0 {
                return Err(format!("{field_name} must not be negative."));
            }
        }
    }
    Ok(())
}

pub(crate) fn check_resulting_min_pass(
    params: &GraphUpdateEnsembleParams,
    details: &crate::domain::graphs::EnsembleDetails,
) -> Result<(), String> {
    // ── CB40 / FR1–FR4: refuse a write whose RESULTING (min_pass, member count)
    // pair is one `graph_import` would reject. Pure arithmetic on values already in
    // hand; runs before any row is written, so a refusal changes nothing (FR4).
    // Only guards calls that actually touch `members` or `min_pass` (FR2) — an
    // unrelated edit to a legacy-invalid ensemble is left for `graph_preflight`.
    if params.members.is_some() || params.min_pass.is_some() {
        let resulting_member_count = params
            .members
            .as_ref()
            .map(|m| m.len())
            .unwrap_or(details.members.len());
        let resulting_min_pass = params.min_pass.unwrap_or(details.ensemble.min_pass);
        crate::domain::validation::validate_ensemble_min_pass(
            &details.ensemble.name,
            resulting_min_pass,
            resulting_member_count,
        )?;
    }
    Ok(())
}

pub(crate) fn refuse_ensemble_kind_change(
    params: &GraphUpdateEnsembleParams,
    effective_kind: EnsembleKind,
) -> Result<(), String> {
    // CM15 / FR5: the ensemble's execution strategy (`kind`) selects the
    // fan-out path (`execute_ensemble` match on `ensemble.kind`) from the
    // per-run snapshot and is not re-read mid-run. Its members, shared
    // prompt and quorum config ARE read fresh per dispatch, so those stay
    // editable on a running graph — repairing a dead member mid-run is the
    // whole point. `kind` is refused here; entry and exit wiring (also
    // topology, also snapshot-pinned) are refused further down, each next
    // to the code that applies it. Every refusal names the graph's state.
    //
    // A `kind` change itself is refused later in the tool body, once the
    // running-graph topology check has run — this helper only guards the
    // kind-dependent scalar fields below, so it takes the *effective* kind
    // (the value that would result after the update) rather than the raw
    // `params.kind` Option.

    // CB67/FR3: validated here, before any write below, same principle
    // as the CM30 infra-field validation above — a rejected value must
    // never land in the DB first. An explicit `null` (`Some(None)`) to
    // CLEAR either field is allowed on any kind; only an explicit
    // non-null value on a cascade/round_robin ensemble is rejected.
    if effective_kind != EnsembleKind::Parallel {
        if matches!(params.straggler_timeout_minutes, Some(Some(_))) {
            return Err(format!(
                "straggler_timeout_minutes has no meaning for a {} ensemble (it only bounds how long a parallel quorum waits for stragglers); omit it, pass null to clear it, or keep/set kind: \"parallel\".",
                effective_kind.as_str()
            ));
        }
        if matches!(params.quorum_grace_minutes, Some(Some(_))) {
            return Err(format!(
                "quorum_grace_minutes has no meaning for a {} ensemble (it only applies once a parallel quorum is met); omit it, pass null to clear it, or keep/set kind: \"parallel\".",
                effective_kind.as_str()
            ));
        }
    }
    Ok(())
}

pub(crate) fn parse_ensemble_kind(
    params: &GraphUpdateEnsembleParams,
) -> Result<Option<EnsembleKind>, CallToolResult> {
    match params.kind.as_deref().map(str::trim) {
        Some("parallel") => Ok(Some(EnsembleKind::Parallel)),
        Some("cascade") => Ok(Some(EnsembleKind::Cascade)),
        Some("round_robin") => Ok(Some(EnsembleKind::RoundRobin)),
        Some(other) => Err(error_result(&format!(
            "Invalid ensemble kind '{other}'. Must be one of: parallel, cascade, round_robin."
        ))),
        None => Ok(None),
    }
}

/// Shared values baked into every resized member row when `members` is
/// replaced: the (possibly just-updated) prompt/timeout/infra defaults.
/// Bundled so the per-row builder stays within the argument limit.
pub(crate) struct EnsembleMemberDefaults<'a> {
    pub(crate) prompt_template: &'a str,
    pub(crate) timeout_minutes: i64,
    pub(crate) infra_retry_limit: Option<i64>,
    pub(crate) infra_crash_max_seconds: Option<i64>,
    pub(crate) infra_backoff_seconds: Option<i64>,
}

impl<'a> EnsembleMemberDefaults<'a> {
    pub(crate) fn from_update(
        params: &'a GraphUpdateEnsembleParams,
        details: &'a EnsembleDetails,
    ) -> Self {
        Self {
            prompt_template: params
                .prompt_template
                .as_deref()
                .unwrap_or(&details.ensemble.prompt_template),
            timeout_minutes: params
                .timeout_minutes
                .unwrap_or(details.ensemble.timeout_minutes),
            infra_retry_limit: match params.infra_retry_limit {
                Some(inner) => inner,
                None => details.ensemble.infra_retry_limit,
            },
            infra_crash_max_seconds: match params.infra_crash_max_seconds {
                Some(inner) => inner,
                None => details.ensemble.infra_crash_max_seconds,
            },
            infra_backoff_seconds: match params.infra_backoff_seconds {
                Some(inner) => inner,
                None => details.ensemble.infra_backoff_seconds,
            },
        }
    }
}

/// One resized member row: the agent node plus its member record. Reuses the
/// existing node (position, name, creation time) when the index survives the
/// resize, mints a fresh id/position otherwise — exactly as before.
pub(crate) fn build_resized_member_row(
    ensemble: &Ensemble,
    defaults: &EnsembleMemberDefaults,
    spec: &EnsembleMemberSpec,
    index: usize,
    old_members: &[EnsembleMember],
    owner_nodes: &[GraphNode],
    start_position: i64,
) -> (EnsembleMember, GraphNode) {
    let (
        platform,
        model,
        prompt_override,
        member_timeout_minutes,
        member_infra_retry_limit,
        member_infra_crash_max_seconds,
        member_infra_backoff_seconds,
    ) = spec;
    let effective_prompt =
        effective_member_prompt(prompt_override.as_deref(), defaults.prompt_template);
    let effective_timeout_minutes = member_timeout_minutes.unwrap_or(defaults.timeout_minutes);
    let effective_infra_retry_limit = member_infra_retry_limit.or(defaults.infra_retry_limit);
    let effective_infra_crash_max_seconds =
        member_infra_crash_max_seconds.or(defaults.infra_crash_max_seconds);
    let effective_infra_backoff_seconds =
        member_infra_backoff_seconds.or(defaults.infra_backoff_seconds);
    let node_id = old_members
        .get(index)
        .map(|member| member.node_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let existing_node = owner_nodes.iter().find(|node| node.id == node_id);
    let position = match existing_node {
        Some(node) => node.position,
        None => start_position + (index - old_members.len()) as i64,
    };
    let node = GraphNode {
        id: node_id.clone(),
        spec_id: ensemble.spec_id.clone(),
        graph_id: ensemble.graph_id.clone(),
        name: existing_node
            .map(|node| node.name.clone())
            .unwrap_or_else(|| format!("{} [{}]", ensemble.name, index + 1)),
        kind: GraphNodeKind::Agent,
        config: member_node_config(
            platform,
            model.as_deref(),
            effective_prompt,
            effective_timeout_minutes,
            effective_infra_retry_limit,
            effective_infra_crash_max_seconds,
            effective_infra_backoff_seconds,
        ),
        position,
        created_at: existing_node
            .map(|node| node.created_at)
            .unwrap_or_else(chrono::Utc::now),
    };
    let member = EnsembleMember {
        ensemble_id: ensemble.id.clone(),
        node_id,
        position: index as i64,
        platform: platform.clone(),
        model: model.clone(),
        prompt_override: prompt_override.clone(),
        timeout_minutes: *member_timeout_minutes,
        infra_retry_limit: *member_infra_retry_limit,
        infra_crash_max_seconds: *member_infra_crash_max_seconds,
        infra_backoff_seconds: *member_infra_backoff_seconds,
    };
    (member, node)
}
