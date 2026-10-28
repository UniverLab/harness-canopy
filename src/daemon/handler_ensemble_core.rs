//! MCP handler tools split out of `daemon::handler` (ensemble builders and add/delete tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_edges::{resolve_graph_target, GraphTarget};
use crate::daemon::handler_formatting::{build_id_result, internal_error};
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_edge_condition, validate_node_not_ensemble_owned,
    validate_non_empty, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{EnsembleKind, EnsembleMemberSpec};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[allow(unused_imports)]
pub(crate) use crate::daemon::handler_ensemble_plan::{
    build_ensemble_unit, resolve_chained_exit_ensemble, BuiltEnsembleUnit, EnsembleExitTarget,
    EnsembleUnitSpec,
};

#[tool_router(router = tool_router_ensemble_core, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_add_ensemble",
        description = "Create an ensemble in ONE call: N (2–8 parallel, 2–16 cascade/round_robin) agent-node members sharing one prompt by default, plus the quorum that waits for all of them, consolidates their outputs (attributed per member), and routes onward. Set `commit_rights: true` to make this ensemble the graph's designated committer (B37) — whichever member the running strategy dispatches becomes the committer for that run. Members differ by platform/model, and each may set its own prompt_override to review the same input from a different angle instead of sharing the template. Each member may also set its own timeout_minutes, overriding the ensemble's shared value for that member only. Each member (and the ensemble as a whole) may also set `infra_retry_limit`/`infra_crash_max_seconds`/`infra_backoff_seconds`, overriding the platform's config.toml value (then the engine default 2/60/30) for the infra-crash retry budget — member wins over ensemble. (Formerly called 'fusion' — retired to avoid colliding with OpenRouter's fusion technology.) on_pass_to/on_fail_to accept a node id or another ensemble's id (chained: the quorum fans out to every member of that ensemble, no intermediate node). Entry rewiring, extra entry sources, and deletion are graph_update_ensemble/graph_delete_ensemble. Pass `node: <name>` to instantiate an ensemble node document (see node_list, e.g. ensemble-proposers): it supplies the shared prompt, the member count and the ensemble shape, while `members` must still be passed with a platform for each one. Pass `crew` to bind an existing crew's ordered members instead of a literal `members` list (mutually exclusive with `members`/blueprint members); later crew edits reach this ensemble at its next dispatch. Pass `mutex` (e.g. [\"cpu\"]) to hold named shared resources for every member run."
    )]
    pub(crate) async fn graph_add_ensemble(
        &self,
        Parameters(params): Parameters<GraphAddEnsembleParams>,
    ) -> Result<CallToolResult, McpError> {
        let canopy_dir = crate::domain::prompts::canopy_dir();
        let graphs = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir).graphs;
        let caps = crate::domain::graphs::MemberCaps::from_graphs(&graphs);

        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Ensemble name") {
            return Ok(error_result(&e));
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
            return Ok(error_result(
                "Pass node or blueprint/crew, never both: an ensemble node document supplies the prompt and members on its own.",
            ));
        }
        let ensemble_node = match node_name {
            Some(node_name) => {
                match crate::daemon::handler_node_lib::resolve_ensemble_node_document(node_name) {
                    Ok(document) => Some(document),
                    Err(e) => return Ok(error_result(&e)),
                }
            }
            None => None,
        };
        let ensemble_node_config = ensemble_node.as_ref().map(|(_, config)| config);
        let blueprint = match blueprint_name {
            Some(blueprint_name) => match self
                .db
                .get_ensemble_blueprint_by_name(blueprint_name)
                .map_err(internal_error)?
            {
                Some(bp) => Some(bp),
                None => {
                    return Ok(error_result(&format!(
                        "Unknown ensemble blueprint '{blueprint_name}'."
                    )))
                }
            },
            None => None,
        };

        let prompt_template: String = match params
            .prompt_template
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(explicit) => explicit.to_string(),
            None => match (&blueprint, ensemble_node_config) {
                (Some(bp), _) => bp.prompt_template.clone(),
                (None, Some(config)) => config.prompt_template.clone(),
                (None, None) => return Ok(error_result(
                    "Provide prompt_template, or a blueprint or node document that supplies one.",
                )),
            },
        };
        let prompt_template = prompt_template.as_str();

        let kind = match params
            .kind
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some("parallel") => EnsembleKind::Parallel,
            Some("cascade") => EnsembleKind::Cascade,
            Some("round_robin") => EnsembleKind::RoundRobin,
            Some(other) => {
                return Ok(error_result(&format!(
                "Invalid ensemble kind '{other}'. Must be one of: parallel, cascade, round_robin."
            )))
            }
            None => match ensemble_node_config {
                Some(config) => match config.strategy() {
                    Ok(kind) => kind,
                    Err(e) => return Ok(error_result(&e)),
                },
                None => EnsembleKind::Parallel,
            },
        };

        // CM49: a crew binding is mutually exclusive with a literal member
        // list (explicit or blueprint-supplied). When bound, the crew's
        // ordered members seed the unit; the binding is stamped after insert.
        let crew_name = params
            .crew
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if crew_name.is_some()
            && (params.members.is_some()
                || blueprint.as_ref().is_some_and(|bp| !bp.members.is_empty()))
        {
            return Ok(error_result("pass crew or members, never both."));
        }
        let members: Vec<EnsembleMemberSpec> = match crew_name {
            Some(name) => match self.db.crew_get(name).map_err(internal_error)? {
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
                    return Ok(error_result(&format!(
                        "crew '{name}' not found. Create it with crew_create first."
                    )))
                }
            },
            None => match resolve_ensemble_members(
                params.members.as_ref(),
                ensemble_node.as_ref(),
                blueprint.as_ref(),
                kind,
                caps,
            ) {
                Ok(members) => members,
                Err(e) => return Ok(error_result(&e)),
            },
        };
        // Blueprint-supplied members bypass `validate_ensemble_members` above
        // (they are already `EnsembleMemberSpec`), so enforce the requested
        // kind's bounds here for both paths — a 12-member blueprint used
        // with kind parallel is refused exactly like 12 explicit members.
        {
            let (min, max) = crate::domain::graphs::member_bounds(kind, caps);
            if members.len() < min || members.len() > max {
                return Ok(error_result(&crate::domain::graphs::member_count_message(
                    kind,
                    caps,
                    members.len(),
                )));
            }
        }
        let condition = match validate_edge_condition(params.condition.trim()) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(&e)),
        };

        let target = match resolve_graph_target(
            &self.db,
            params.spec_id.as_deref(),
            params.graph_id.as_deref(),
        ) {
            Ok(target) => target,
            Err(e) => return Ok(error_result(&e)),
        };
        let existing_nodes = match &target {
            GraphTarget::Spec(spec_id) => {
                self.db.list_graph_nodes(spec_id).map_err(internal_error)?
            }
            GraphTarget::Graph(graph_id) => self
                .db
                .list_graph_nodes_for_graph(graph_id)
                .map_err(internal_error)?,
        };
        let node_exists = |id: &str| existing_nodes.iter().any(|node| node.id == id);

        let from_node = match resolve_prefix_or_error(
            &self.db,
            params.from_node.trim(),
            Database::resolve_graph_node_id_by_prefix,
            "node",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        if !node_exists(&from_node) {
            return Ok(error_result(&format!(
                "Graph node '{from_node}' not found in the target graph."
            )));
        }
        if let Err(e) = validate_node_not_ensemble_owned(&self.db, &from_node) {
            return Ok(error_result(&format!(
                "Cannot wire an ensemble's entry from an ensemble-owned node: {e} To chain ensembles, create this one from a plain node and then route the upstream ensemble's exit into it (graph_update_ensemble on_pass_to/on_fail_to accept an ensemble id)."
            )));
        }

        let (owner_spec_id, owner_graph_id): (Option<String>, Option<String>) = match &target {
            GraphTarget::Spec(spec_id) => (Some(spec_id.clone()), None),
            GraphTarget::Graph(graph_id) => (None, Some(graph_id.clone())),
        };
        // An exit target is a plain node — or, to chain ensembles with no
        // intermediate node, another ensemble's id (the quorum fans out to
        // every member of that ensemble).
        let on_pass_target = match resolve_chained_exit_ensemble(
            &self.db,
            params.on_pass_to.trim(),
            "",
            owner_spec_id.as_deref(),
            owner_graph_id.as_deref(),
        ) {
            Ok(Some(target)) => target,
            Ok(None) => {
                let on_pass_to = match resolve_prefix_or_error(
                    &self.db,
                    params.on_pass_to.trim(),
                    Database::resolve_graph_node_id_by_prefix,
                    "node",
                ) {
                    Ok(id) => id,
                    Err(e) => return Ok(e),
                };
                if let Err(e) = validate_non_empty(&on_pass_to, "on_pass_to") {
                    return Ok(error_result(&e));
                }
                if !node_exists(&on_pass_to) {
                    return Ok(error_result(&format!(
                        "Graph node '{on_pass_to}' not found in the target graph."
                    )));
                }
                if let Err(e) = validate_node_not_ensemble_owned(&self.db, &on_pass_to) {
                    return Ok(error_result(&format!(
                            "Cannot wire an ensemble's exit directly into another ensemble's members/quorum: {e} To route into the whole ensemble, pass its ensemble id as on_pass_to instead of a member/quorum node id."
                        )));
                }
                EnsembleExitTarget::Node(on_pass_to)
            }
            Err(e) => return Ok(error_result(&e)),
        };

        let on_fail_target: Option<EnsembleExitTarget> = match &params.on_fail_to {
            Some(raw) => {
                match resolve_chained_exit_ensemble(
                    &self.db,
                    raw.trim(),
                    "",
                    owner_spec_id.as_deref(),
                    owner_graph_id.as_deref(),
                ) {
                    Ok(Some(target)) => Some(target),
                    Ok(None) => {
                        match resolve_prefix_or_error(
                            &self.db,
                            raw.trim(),
                            Database::resolve_graph_node_id_by_prefix,
                            "node",
                        ) {
                            Ok(id) => {
                                if !node_exists(&id) {
                                    return Ok(error_result(&format!(
                                        "Graph node '{id}' not found in the target graph."
                                    )));
                                }
                                if let Err(e) = validate_node_not_ensemble_owned(&self.db, &id) {
                                    return Ok(error_result(&format!(
                                    "Cannot wire an ensemble's exit directly into another ensemble's members/quorum: {e} To route into the whole ensemble, pass its ensemble id as on_fail_to instead of a member/quorum node id."
                                    )));
                                }
                                Some(EnsembleExitTarget::Node(id))
                            }
                            Err(e) => return Ok(e),
                        }
                    }
                    Err(e) => return Ok(error_result(&e)),
                }
            }
            None => None,
        };

        let mutex = params
            .mutex
            .clone()
            .or_else(|| ensemble_node_config.and_then(|config| config.mutex.clone()))
            .unwrap_or_default();
        let min_pass = params
            .min_pass
            .or_else(|| ensemble_node_config.and_then(|config| config.min_pass))
            .or_else(|| blueprint.as_ref().and_then(|bp| bp.min_pass))
            .unwrap_or(members.len() as i64);
        if let Err(e) =
            crate::domain::validation::validate_ensemble_min_pass(name, min_pass, members.len())
        {
            return Ok(error_result(&e));
        }
        if let Err(e) = crate::domain::validation::validate_mutex_list(
            &format!("Ensemble '{name}'"),
            Some(mutex.as_slice()),
        ) {
            return Ok(error_result(&e));
        }
        let timeout_minutes = default_member_timeout(
            &graphs,
            params
                .timeout_minutes
                .or_else(|| ensemble_node_config.and_then(|config| config.timeout_minutes)),
        );
        if timeout_minutes < 0 {
            return Ok(error_result("timeout_minutes must not be negative."));
        }
        let straggler_timeout_minutes = params
            .straggler_timeout_minutes
            .or_else(|| ensemble_node_config.and_then(|config| config.straggler_timeout_minutes));
        if let Some(straggler) = straggler_timeout_minutes {
            if straggler < 0 {
                return Ok(error_result(
                    "straggler_timeout_minutes must not be negative.",
                ));
            }
            // CB67/FR3: a parallel-quorum concept — meaningless once members
            // run one at a time.
            if kind != EnsembleKind::Parallel {
                return Ok(error_result(&format!(
                    "straggler_timeout_minutes has no meaning for a {} ensemble (it only bounds how long a parallel quorum waits for stragglers); omit it, or set kind: \"parallel\".",
                    kind.as_str()
                )));
            }
        }
        let quorum_grace_minutes = params
            .quorum_grace_minutes
            .or_else(|| ensemble_node_config.and_then(|config| config.quorum_grace_minutes))
            .or_else(|| blueprint.as_ref().and_then(|bp| bp.quorum_grace_minutes));
        if let Some(grace) = quorum_grace_minutes {
            if grace < 0 {
                return Ok(error_result("quorum_grace_minutes must not be negative."));
            }
            // CB67/FR3: applies only once a parallel quorum is met.
            if kind != EnsembleKind::Parallel {
                return Ok(error_result(&format!(
                    "quorum_grace_minutes has no meaning for a {} ensemble (it only applies once a parallel quorum is met); omit it, or set kind: \"parallel\".",
                    kind.as_str()
                )));
            }
        }
        let infra_retry_limit = params
            .infra_retry_limit
            .or_else(|| ensemble_node_config.and_then(|config| config.infra_retry_limit));
        let infra_crash_max_seconds = params
            .infra_crash_max_seconds
            .or_else(|| ensemble_node_config.and_then(|config| config.infra_crash_max_seconds));
        let infra_backoff_seconds = params
            .infra_backoff_seconds
            .or_else(|| ensemble_node_config.and_then(|config| config.infra_backoff_seconds));
        for (field_name, value) in [
            ("infra_retry_limit", infra_retry_limit),
            ("infra_crash_max_seconds", infra_crash_max_seconds),
            ("infra_backoff_seconds", infra_backoff_seconds),
        ] {
            if value.is_some_and(|v| v < 0) {
                return Ok(error_result(&format!("{field_name} must not be negative.")));
            }
        }

        let (spec_id, graph_id) = match &target {
            GraphTarget::Spec(spec_id) => (Some(spec_id.clone()), None),
            GraphTarget::Graph(graph_id) => (None, Some(graph_id.clone())),
        };
        let start_position = existing_nodes
            .last()
            .map(|node| node.position + 1)
            .unwrap_or(1);

        let on_pass_row = on_pass_target.row_target();
        let on_pass_fan_out = match &on_pass_target {
            EnsembleExitTarget::Node(_) => None,
            EnsembleExitTarget::Ensemble { .. } => Some(on_pass_target.edge_targets()),
        };
        let (on_fail_row, on_fail_fan_out): (Option<String>, Option<Vec<String>>) =
            match &on_fail_target {
                None => (None, None),
                Some(EnsembleExitTarget::Node(id)) => (Some(id.clone()), None),
                Some(target) => (Some(target.row_target()), Some(target.edge_targets())),
            };
        let built = build_ensemble_unit(&EnsembleUnitSpec {
            spec_id,
            graph_id,
            name,
            prompt_template,
            members: &members,
            entry_from_node: &from_node,
            entry_condition: condition,
            on_pass_to: &on_pass_row,
            on_pass_fan_out,
            on_fail_to: on_fail_row.as_deref(),
            on_fail_fan_out,
            min_pass,
            timeout_minutes,
            straggler_timeout_minutes,
            quorum_grace_minutes,
            infra_retry_limit,
            infra_crash_max_seconds,
            infra_backoff_seconds,
            start_position,
            kind,
            commit_rights: params
                .commit_rights
                .or_else(|| ensemble_node_config.and_then(|config| config.commit_rights))
                .unwrap_or(false),
            mutex,
        });

        // The shared prompt and every member override are refused here when
        // they carry a placeholder nothing can bind — the write that used to
        // be accepted and then fail member by member at spawn time. The new
        // member and quorum nodes are already bindable: at spawn they are part
        // of the graph's node names.
        {
            let scope = crate::daemon::handler_templates::TemplateScope::load(
                &self.db,
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
                return Ok(error_result(&e));
            }
            for (index, member) in members.iter().enumerate() {
                if let Some(template) = member.2.as_deref() {
                    let member_owner =
                        format!("Ensemble '{}' member {} prompt override", name, index + 1);
                    if let Err(e) = crate::daemon::handler_templates::check_template_bindings(
                        &member_owner,
                        template,
                        crate::graph_engine::AGENT_PROMPT_BINDINGS,
                        &node_names,
                        &ensembles,
                    ) {
                        return Ok(error_result(&e));
                    }
                }
            }
        }

        self.db
            .insert_ensemble_unit(
                &built.ensemble,
                &built.members,
                &built.member_nodes,
                &built.join_node,
                &built.edges,
            )
            .map_err(internal_error)?;

        if let Some(name) = crew_name {
            if let Err(e) = self.db.bind_ensemble_to_crew(&built.ensemble.id, name) {
                return Ok(error_result(&e.to_string()));
            }
        }

        Ok(build_id_result(&built.ensemble.id, "ensemble_id"))
    }

    #[tool(
        name = "graph_delete_ensemble",
        description = "Delete an ensemble — its members, its quorum, and every edge naming any of them — as one operation. Refused while the owning graph is running, like the other topology tools. Refused while another ensemble's exit is chained into it (rewire that exit with graph_update_ensemble first)."
    )]
    pub(crate) async fn graph_delete_ensemble(
        &self,
        Parameters(params): Parameters<GraphDeleteEnsembleParams>,
    ) -> Result<CallToolResult, McpError> {
        let ensemble_id = match resolve_prefix_or_error(
            &self.db,
            params.ensemble_id.trim(),
            Database::resolve_ensemble_id_by_prefix,
            "ensemble",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let Some(details) = self
            .db
            .get_ensemble_details(&ensemble_id)
            .map_err(internal_error)?
        else {
            return Ok(error_result(&format!(
                "Ensemble '{ensemble_id}' not found."
            )));
        };
        if let Err(e) = validate_topology_mutation_allowed(
            &self.db,
            details.ensemble.spec_id.as_deref(),
            details.ensemble.graph_id.as_deref(),
        ) {
            return Ok(error_result(&e));
        }
        // Another ensemble chained into this one holds exit edges from its
        // quorum to these members and names this quorum in its row — deleting
        // here would strand both. Rewire there first.
        let siblings = match (&details.ensemble.spec_id, &details.ensemble.graph_id) {
            (Some(spec_id), None) => self
                .db
                .list_ensembles_for_spec(spec_id)
                .map_err(internal_error)?,
            (None, Some(graph_id)) => self
                .db
                .list_ensembles_for_graph(graph_id)
                .map_err(internal_error)?,
            _ => Vec::new(),
        };
        let join_node_id = details.ensemble.join_node_id.as_str();
        if let Some(blocker) = siblings.iter().find(|sibling| {
            sibling.ensemble.id != ensemble_id
                && (sibling.ensemble.on_pass_to.as_str() == join_node_id
                    || sibling.ensemble.on_fail_to.as_deref() == Some(join_node_id))
        }) {
            return Ok(error_result(&format!(
                "Ensemble '{ensemble_id}' cannot be deleted while ensemble '{}' ('{}') routes its exit into it; rewire that exit with graph_update_ensemble first.",
                blocker.ensemble.id, blocker.ensemble.name
            )));
        }

        match self.db.delete_ensemble_unit(&ensemble_id) {
            Ok(deleted) => Ok(success_result(&format!(
                "Ensemble '{}' deleted: {} member(s), its quorum '{}', and {} edge(s) naming them.",
                deleted.ensemble_id,
                deleted.member_node_ids.len(),
                deleted.join_node_id,
                deleted.deleted_edge_count,
            ))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }
}

pub(crate) fn default_member_timeout(
    graphs: &crate::domain::canopy_config::GraphsConfig,
    requested: Option<i64>,
) -> i64 {
    requested.unwrap_or(graphs.straggler_timeout_minutes as i64)
}

/// Resolve `graph_add_ensemble`'s members from the caller's list, an
/// ensemble node document (which carries no harnesses), or an ensemble
/// blueprint. A document's members match the caller's one-for-one: each
/// caller entry supplies the platform the document deliberately omits, and
/// the document fills in the per-member prompt override, timeout and infra
/// budget the caller left out.
fn resolve_ensemble_members(
    explicit: Option<&Vec<EnsembleMemberParams>>,
    ensemble_node: Option<&(String, crate::domain::nodes::EnsembleNodeConfig)>,
    blueprint: Option<&crate::domain::blueprints::EnsembleBlueprint>,
    kind: EnsembleKind,
    caps: crate::domain::graphs::MemberCaps,
) -> Result<Vec<EnsembleMemberSpec>, String> {
    match (explicit, ensemble_node) {
        (Some(explicit), Some((document_name, config))) => {
            let merged = merge_node_members(document_name, config, explicit)?;
            validate_ensemble_members(&merged, kind, caps)
        }
        (Some(explicit), None) => validate_ensemble_members(explicit, kind, caps),
        (None, Some((document_name, config))) => Err(format!(
            "Ensemble node document '{document_name}' carries no member harnesses; pass members with a non-empty 'platform' for each of its {} members (and 'model' when the platform needs one).",
            config.members.len()
        )),
        (None, None) => match blueprint {
            Some(bp) => Ok(bp.members.clone()),
            None => Err(
                "Provide members, or a blueprint or node document that supplies them."
                    .to_string(),
            ),
        },
    }
}

/// Merge the caller's member list with a document's member templates: the
/// caller's keys win where set, the document fills in the rest.
fn merge_node_members(
    document_name: &str,
    config: &crate::domain::nodes::EnsembleNodeConfig,
    explicit: &[EnsembleMemberParams],
) -> Result<Vec<EnsembleMemberParams>, String> {
    if explicit.len() != config.members.len() {
        return Err(format!(
            "Ensemble node document '{document_name}' defines {} members; pass exactly {} entries in 'members' with a platform for each, got {}.",
            config.members.len(),
            config.members.len(),
            explicit.len()
        ));
    }
    Ok(explicit
        .iter()
        .zip(&config.members)
        .map(|(caller, template)| EnsembleMemberParams {
            platform: caller.platform.clone(),
            model: caller.model.clone(),
            prompt_override: caller
                .prompt_override
                .clone()
                .or_else(|| template.prompt_override.clone()),
            timeout_minutes: caller.timeout_minutes.or(template.timeout_minutes),
            infra_retry_limit: caller.infra_retry_limit.or(template.infra_retry_limit),
            infra_crash_max_seconds: caller
                .infra_crash_max_seconds
                .or(template.infra_crash_max_seconds),
            infra_backoff_seconds: caller
                .infra_backoff_seconds
                .or(template.infra_backoff_seconds),
        })
        .collect())
}

pub(crate) fn validate_ensemble_members(
    members: &[EnsembleMemberParams],
    kind: EnsembleKind,
    caps: crate::domain::graphs::MemberCaps,
) -> Result<Vec<EnsembleMemberSpec>, String> {
    let (min, max) = crate::domain::graphs::member_bounds(kind, caps);
    if members.len() < min || members.len() > max {
        return Err(crate::domain::graphs::member_count_message(
            kind,
            caps,
            members.len(),
        ));
    }
    members
        .iter()
        .map(|member| {
            let platform = member.platform.trim();
            if platform.is_empty() {
                return Err("Ensemble member 'platform' must not be empty.".to_string());
            }
            if let Some(msg) = crate::daemon::handler_helpers::interactive_only_rejection(platform)
            {
                return Err(msg);
            }
            let model = member
                .model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let prompt_override = member
                .prompt_override
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            if let Some(timeout_minutes) = member.timeout_minutes {
                if timeout_minutes < 0 {
                    return Err(
                        "Ensemble member 'timeout_minutes' must not be negative.".to_string()
                    );
                }
            }
            for (field_name, value) in [
                ("infra_retry_limit", member.infra_retry_limit),
                ("infra_crash_max_seconds", member.infra_crash_max_seconds),
                ("infra_backoff_seconds", member.infra_backoff_seconds),
            ] {
                if value.is_some_and(|v| v < 0) {
                    return Err(format!(
                        "Ensemble member '{field_name}' must not be negative."
                    ));
                }
            }
            Ok((
                platform.to_string(),
                model,
                prompt_override,
                member.timeout_minutes,
                member.infra_retry_limit,
                member.infra_crash_max_seconds,
                member.infra_backoff_seconds,
            ))
        })
        .collect()
}

/// This member's effective prompt: its own `prompt_override` if it has one,
/// else the ensemble's shared `prompt_template`.
pub(crate) fn effective_member_prompt<'a>(
    prompt_override: Option<&'a str>,
    shared: &'a str,
) -> &'a str {
    prompt_override.unwrap_or(shared)
}

/// Build a member agent node's `config` — its effective prompt (its own
/// override, or the shared ensemble prompt) plus this member's own
/// platform/model, the same shape `validate_node_config`'s `Agent` arm
/// expects.
pub(crate) fn member_node_config(
    platform: &str,
    model: Option<&str>,
    effective_prompt: &str,
    timeout_minutes: i64,
    infra_retry_limit: Option<i64>,
    infra_crash_max_seconds: Option<i64>,
    infra_backoff_seconds: Option<i64>,
) -> serde_json::Value {
    let mut config = serde_json::json!({
        "platform": platform,
        "model": model,
        "prompt_template": effective_prompt,
        "timeout_minutes": timeout_minutes,
    });
    if let serde_json::Value::Object(ref mut map) = config {
        if let Some(v) = infra_retry_limit {
            map.insert("infra_retry_limit".to_string(), serde_json::json!(v));
        }
        if let Some(v) = infra_crash_max_seconds {
            map.insert("infra_crash_max_seconds".to_string(), serde_json::json!(v));
        }
        if let Some(v) = infra_backoff_seconds {
            map.insert("infra_backoff_seconds".to_string(), serde_json::json!(v));
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_member_timeout_applies_setting_when_requested_is_none() {
        let graphs = crate::domain::canopy_config::GraphsConfig {
            straggler_timeout_minutes: 45,
            ..Default::default()
        };
        assert_eq!(default_member_timeout(&graphs, None), 45);
        assert_eq!(default_member_timeout(&graphs, Some(10)), 10);
    }
}
