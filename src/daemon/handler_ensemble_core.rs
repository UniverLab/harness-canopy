//! MCP handler tools split out of `daemon::handler` (ensemble builders and add/delete tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_ensemble_add::{
    check_ensemble_templates, exit_row_targets, resolve_ensemble_build_plan,
    resolve_ensemble_sources, resolve_ensemble_wiring,
};
use crate::daemon::handler_formatting::{build_id_result, internal_error};
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_edge_condition, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{EnsembleKind, EnsembleMemberSpec};
use crate::or_early_return;
use rmcp::model::CallToolResult;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[allow(unused_imports)]
pub(crate) use crate::daemon::handler_ensemble_plan::{
    build_ensemble_unit, resolve_chained_exit_ensemble, BuiltEnsembleUnit, EnsembleExitTarget,
    EnsembleUnitSpec,
};

#[tool_router(router = tool_router_ensemble_core, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_add_ensemble_impl(
        &self,
        params: GraphAddEnsembleParams,
    ) -> Result<CallToolResult, McpError> {
        let canopy_dir = crate::domain::prompts::canopy_dir();
        let graphs = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir).graphs;
        let caps = crate::domain::graphs::MemberCaps::from_graphs(&graphs);
        let sources = or_early_return!(resolve_ensemble_sources(&self.db, &params));
        let ensemble_node_config = sources.ensemble_node.as_ref().map(|(_, config)| config);
        let plan = or_early_return!(resolve_ensemble_build_plan(
            &self.db, &params, &sources, &graphs, caps
        ));
        let condition = match validate_edge_condition(params.condition.trim()) {
            Ok(c) => c,
            Err(e) => return Ok(error_result(&e)),
        };
        let wiring = or_early_return!(resolve_ensemble_wiring(&self.db, &params));
        let exits = exit_row_targets(&wiring.on_pass_target, &wiring.on_fail_target);
        let built = build_ensemble_unit(&EnsembleUnitSpec {
            spec_id: wiring.spec_id,
            graph_id: wiring.graph_id,
            name: sources.name.as_str(),
            prompt_template: &plan.prompt_template,
            members: &plan.members,
            entry_from_node: &wiring.from_node,
            entry_condition: condition,
            on_pass_to: &exits.on_pass_row,
            on_pass_fan_out: exits.on_pass_fan_out,
            on_fail_to: exits.on_fail_row.as_deref(),
            on_fail_fan_out: exits.on_fail_fan_out,
            min_pass: plan.timeouts.min_pass,
            timeout_minutes: plan.timeouts.timeout_minutes,
            straggler_timeout_minutes: plan.timeouts.straggler_timeout_minutes,
            quorum_grace_minutes: plan.timeouts.quorum_grace_minutes,
            infra_retry_limit: plan.timeouts.infra_retry_limit,
            infra_crash_max_seconds: plan.timeouts.infra_crash_max_seconds,
            infra_backoff_seconds: plan.timeouts.infra_backoff_seconds,
            start_position: wiring.start_position,
            kind: plan.kind,
            commit_rights: params
                .commit_rights
                .or_else(|| ensemble_node_config.and_then(|config| config.commit_rights))
                .unwrap_or(false),
            mutex: plan.timeouts.mutex,
        });
        let () = or_early_return!(check_ensemble_templates(
            &self.db,
            &built,
            &plan.members,
            &sources.name,
            &plan.prompt_template
        ));
        self.db
            .insert_ensemble_unit(
                &built.ensemble,
                &built.members,
                &built.member_nodes,
                &built.join_node,
                &built.edges,
            )
            .map_err(internal_error)?;
        if let Some(name) = plan.crew_name {
            if let Err(e) = self.db.bind_ensemble_to_crew(&built.ensemble.id, &name) {
                return Ok(error_result(&e.to_string()));
            }
        }
        Ok(build_id_result(&built.ensemble.id, "ensemble_id"))
    }

    pub(crate) async fn graph_delete_ensemble_impl(
        &self,
        params: GraphDeleteEnsembleParams,
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
                "Ensemble '{ensemble_id}' cannot be deleted while ensemble '{}' ('{}') routes its exit into it; rewire that exit with graph_ensemble first.",
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

/// Resolve `graph_ensemble`'s members from the caller's list, an
/// ensemble node document (which carries no harnesses), or an ensemble
/// blueprint. A document's members match the caller's one-for-one: each
/// caller entry supplies the platform the document deliberately omits, and
/// the document fills in the per-member prompt override, timeout and infra
/// budget the caller left out.
pub(crate) fn resolve_ensemble_members(
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
