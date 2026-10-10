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
        description = "Create an ensemble in ONE call: N (2–8 parallel, 2–16 cascade/round_robin) agent-node members sharing one prompt by default, plus the quorum that waits for all of them, consolidates their outputs (attributed per member), and routes onward. Set `commit_rights: true` to make this ensemble the graph's designated committer (B37) — whichever member the running strategy dispatches becomes the committer for that run. Members differ by platform/model, and each may set its own prompt_override to review the same input from a different angle instead of sharing the template. Each member may also set its own timeout_minutes, overriding the ensemble's shared value for that member only. Each member (and the ensemble as a whole) may also set `infra_retry_limit`/`infra_crash_max_seconds`/`infra_backoff_seconds`, overriding the platform's config.toml value (then the engine default 2/60/30) for the infra-crash retry budget — member wins over ensemble. (Formerly called 'fusion' — retired to avoid colliding with OpenRouter's fusion technology.) on_pass_to/on_fail_to accept a node id or another ensemble's id (chained: the quorum fans out to every member of that ensemble, no intermediate node). Entry rewiring, extra entry sources, and deletion are graph_update_ensemble/graph_delete_ensemble."
    )]
    pub(crate) async fn graph_add_ensemble(
        &self,
        Parameters(params): Parameters<GraphAddEnsembleParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Ensemble name") {
            return Ok(error_result(&e));
        }

        let blueprint_name = params
            .blueprint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
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
            None => match &blueprint {
                Some(bp) => bp.prompt_template.clone(),
                None => {
                    return Ok(error_result(
                        "Provide prompt_template, or a blueprint that supplies one.",
                    ))
                }
            },
        };
        let prompt_template = prompt_template.as_str();

        let kind = match params.kind.as_deref().map(str::trim).unwrap_or("parallel") {
            "parallel" => EnsembleKind::Parallel,
            "cascade" => EnsembleKind::Cascade,
            "round_robin" => EnsembleKind::RoundRobin,
            other => {
                return Ok(error_result(&format!(
                "Invalid ensemble kind '{other}'. Must be one of: parallel, cascade, round_robin."
            )))
            }
        };

        let members: Vec<EnsembleMemberSpec> = match &params.members {
            Some(explicit) => match validate_ensemble_members(explicit, kind) {
                Ok(members) => members,
                Err(e) => return Ok(error_result(&e)),
            },
            None => match &blueprint {
                Some(bp) => bp.members.clone(),
                None => {
                    return Ok(error_result(
                        "Provide members, or a blueprint that supplies them.",
                    ))
                }
            },
        };
        // Blueprint-supplied members bypass `validate_ensemble_members` above
        // (they are already `EnsembleMemberSpec`), so enforce the requested
        // kind's bounds here for both paths — a 12-member blueprint used
        // with kind parallel is refused exactly like 12 explicit members.
        {
            let (min, max) = crate::domain::graphs::member_bounds(kind);
            if members.len() < min || members.len() > max {
                return Ok(error_result(&format!(
                    "A {} ensemble must have {min}-{max} members, got {}.",
                    kind.as_str(),
                    members.len()
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

        let min_pass = params
            .min_pass
            .or_else(|| blueprint.as_ref().and_then(|bp| bp.min_pass))
            .unwrap_or(members.len() as i64);
        if let Err(e) =
            crate::domain::validation::validate_ensemble_min_pass(name, min_pass, members.len())
        {
            return Ok(error_result(&e));
        }
        let timeout_minutes = params
            .timeout_minutes
            .unwrap_or(DEFAULT_ENSEMBLE_MEMBER_TIMEOUT_MINUTES);
        if timeout_minutes < 0 {
            return Ok(error_result("timeout_minutes must not be negative."));
        }
        if let Some(straggler) = params.straggler_timeout_minutes {
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
        for (field_name, value) in [
            ("infra_retry_limit", params.infra_retry_limit),
            ("infra_crash_max_seconds", params.infra_crash_max_seconds),
            ("infra_backoff_seconds", params.infra_backoff_seconds),
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
            straggler_timeout_minutes: params.straggler_timeout_minutes,
            quorum_grace_minutes,
            infra_retry_limit: params.infra_retry_limit,
            infra_crash_max_seconds: params.infra_crash_max_seconds,
            infra_backoff_seconds: params.infra_backoff_seconds,
            start_position,
            kind,
            commit_rights: params.commit_rights.unwrap_or(false),
        });

        self.db
            .insert_ensemble_unit(
                &built.ensemble,
                &built.members,
                &built.member_nodes,
                &built.join_node,
                &built.edges,
            )
            .map_err(internal_error)?;

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

pub(crate) const DEFAULT_ENSEMBLE_MEMBER_TIMEOUT_MINUTES: i64 = 30;

pub(crate) fn validate_ensemble_members(
    members: &[EnsembleMemberParams],
    kind: EnsembleKind,
) -> Result<Vec<EnsembleMemberSpec>, String> {
    let (min, max) = crate::domain::graphs::member_bounds(kind);
    if members.len() < min || members.len() > max {
        return Err(format!(
            "A {} ensemble must have {min}-{max} members, got {}.",
            kind.as_str(),
            members.len()
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
