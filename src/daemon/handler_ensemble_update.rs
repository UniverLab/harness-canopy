//! MCP handler tools split out of `daemon::handler` (ensemble update tool).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_ensemble_core::{
    effective_member_prompt, member_node_config, validate_ensemble_members,
};
use crate::daemon::handler_ensemble_plan::{
    build_resized_member_row, check_resulting_min_pass, parse_ensemble_kind,
    refuse_ensemble_kind_change, reject_empty_ensemble_update, validate_ensemble_infra_updates,
    EnsembleMemberDefaults,
};
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_non_empty, validate_topology_mutation_allowed,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{EnsembleDetails, EnsembleKind, GraphNode};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_ensemble_update, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_update_ensemble",
        description = "Update an ensemble's shared prompt (propagated to every member without its own prompt_override), member list (platform/model/prompt_override/timeout_minutes/infra_retry_limit/infra_crash_max_seconds/infra_backoff_seconds — added/removed/replaced by position), quorum config (min_pass, straggler_timeout_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds — clearable with null like straggler_timeout_minutes), entry wiring (from_node/condition replaces every entry; add_entry_from/remove_entry_from add or detach one entry source so several nodes can enter with no relay), and/or exit wiring (on_pass_to/on_fail_to take a node id or another ensemble's id to chain quorums with no intermediate node) — all in one call, without touching individual member nodes directly. `commit_rights` sets whether this ensemble is the graph's designated committer (B37); editable on a running graph, same as prompt/quorum config."
    )]
    pub(crate) async fn graph_update_ensemble(
        &self,
        Parameters(params): Parameters<GraphUpdateEnsembleParams>,
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
        let Some(mut details) = self
            .db
            .get_ensemble_details(&ensemble_id)
            .map_err(internal_error)?
        else {
            return Ok(error_result(&format!(
                "Ensemble '{ensemble_id}' not found."
            )));
        };

        if let Err(e) = reject_empty_ensemble_update(&params) {
            return Ok(error_result(&e));
        }

        if let Err(e) = validate_ensemble_infra_updates(&params) {
            return Ok(error_result(&e));
        }

        if let Err(e) = check_resulting_min_pass(&params, &details) {
            return Ok(error_result(&e));
        }

        if params.kind.is_some() {
            if let Err(e) = validate_topology_mutation_allowed(
                &self.db,
                details.ensemble.spec_id.as_deref(),
                details.ensemble.graph_id.as_deref(),
            ) {
                return Ok(error_result(&e));
            }
        }

        if let Some(prompt_template) = &params.prompt_template {
            if let Err(e) = validate_non_empty(prompt_template.trim(), "Ensemble prompt_template") {
                return Ok(error_result(&e));
            }
        }

        let new_kind = match parse_ensemble_kind(&params) {
            Ok(kind) => kind,
            Err(e) => return Ok(e),
        };
        let effective_kind = new_kind.unwrap_or(details.ensemble.kind);

        // FR3: a kind change may tighten the member-count ceiling (only
        // switching TO parallel, max 8, can). Refuse before any write so a
        // 12-member round_robin cannot silently become an over-cap parallel
        // ensemble that only misbehaves at dispatch time. The minimum is
        // enforced at creation, so it is not re-checked here.
        let (min_for_effective, max_for_effective) =
            crate::domain::graphs::member_bounds(effective_kind);
        if details.members.len() > max_for_effective {
            return Ok(error_result(&format!(
                "A {} ensemble must have {min_for_effective}-{max_for_effective} members, got {}.",
                effective_kind.as_str(),
                details.members.len()
            )));
        }

        if let Err(e) = refuse_ensemble_kind_change(&params, effective_kind) {
            return Ok(error_result(&e));
        }

        let cleared_grace_fields_for_kind_change = self
            .apply_ensemble_kind_change(&ensemble_id, &params, &mut details, new_kind)
            .await?;

        // CM28: `commit_rights` is CONFIG, not topology — free to edit on a
        // running graph, same as prompt/quorum config, so deliberately not
        // gated by `validate_topology_mutation_allowed`.
        if let Some(commit_rights) = params.commit_rights {
            self.db
                .update_ensemble_commit_rights(&ensemble_id, commit_rights)
                .map_err(internal_error)?;
            details.ensemble.commit_rights = commit_rights;
        }

        let owner_nodes = match (&details.ensemble.spec_id, &details.ensemble.graph_id) {
            (Some(spec_id), None) => self.db.list_graph_nodes(spec_id).map_err(internal_error)?,
            (None, Some(graph_id)) => self
                .db
                .list_graph_nodes_for_graph(graph_id)
                .map_err(internal_error)?,
            _ => Vec::new(),
        };

        match self
            .apply_ensemble_member_updates(
                &ensemble_id,
                &params,
                &mut details,
                &owner_nodes,
                effective_kind,
            )
            .await?
        {
            Ok(()) => {}
            Err(e) => return Ok(e),
        }

        match self
            .apply_ensemble_prompt_and_join_config(&ensemble_id, &params)
            .await?
        {
            Ok(()) => {}
            Err(e) => return Ok(e),
        }

        match self
            .apply_ensemble_entry_wiring(&ensemble_id, &params, &mut details, &owner_nodes)
            .await?
        {
            Ok(()) => {}
            Err(e) => return Ok(e),
        }

        match self
            .apply_ensemble_exit_wiring(&ensemble_id, &params, &details)
            .await?
        {
            Ok(()) => {}
            Err(e) => return Ok(e),
        }

        let note = if cleared_grace_fields_for_kind_change {
            format!(
                " Cleared straggler_timeout_minutes/quorum_grace_minutes: not meaningful for a {} ensemble.",
                effective_kind.as_str()
            )
        } else {
            String::new()
        };
        Ok(success_result(&format!(
            "Ensemble '{ensemble_id}' updated.{note}"
        )))
    }

    async fn apply_ensemble_kind_change(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
        details: &mut EnsembleDetails,
        new_kind: Option<EnsembleKind>,
    ) -> Result<bool, McpError> {
        let mut cleared_grace_fields_for_kind_change = false;
        if let Some(kind) = new_kind {
            let rri = if kind == EnsembleKind::RoundRobin {
                Some(Some(details.ensemble.round_robin_index.unwrap_or(0)))
            } else {
                Some(None)
            };
            self.db
                .update_ensemble_kind(ensemble_id, Some(kind), rri)
                .map_err(internal_error)?;
            details.ensemble.kind = kind;
            if kind == EnsembleKind::RoundRobin {
                details.ensemble.round_robin_index =
                    Some(details.ensemble.round_robin_index.unwrap_or(0));
            } else {
                details.ensemble.round_robin_index = None;
            }

            // CB67/FR3: switching to a sequential kind retires any stored
            // straggler_timeout_minutes/quorum_grace_minutes from this
            // ensemble's parallel days — left in place they would silently
            // reappear if someone switches back to parallel later, no longer
            // matching what this call's caller sees today. The reject-block
            // above already returned on an explicit non-null attempt, so from
            // here every path leaves both fields None: an untouched field is
            // auto-cleared here, an explicit null is cleared by the
            // join-config block below like any other explicit clear.
            if kind != EnsembleKind::Parallel
                && (details.ensemble.straggler_timeout_minutes.is_some()
                    || details.ensemble.quorum_grace_minutes.is_some())
            {
                let clear_straggler = params.straggler_timeout_minutes.is_none()
                    && details.ensemble.straggler_timeout_minutes.is_some();
                let clear_grace = params.quorum_grace_minutes.is_none()
                    && details.ensemble.quorum_grace_minutes.is_some();
                if clear_straggler || clear_grace {
                    self.db
                        .update_ensemble_join_config(
                            ensemble_id,
                            None,
                            clear_straggler.then_some(None),
                            clear_grace.then_some(None),
                            None,
                            None,
                            None,
                            None,
                        )
                        .map_err(internal_error)?;
                    if clear_straggler {
                        details.ensemble.straggler_timeout_minutes = None;
                    }
                    if clear_grace {
                        details.ensemble.quorum_grace_minutes = None;
                    }
                }
                cleared_grace_fields_for_kind_change = true;
            }
        }
        Ok(cleared_grace_fields_for_kind_change)
    }

    async fn apply_ensemble_member_updates(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
        details: &mut EnsembleDetails,
        owner_nodes: &[GraphNode],
        effective_kind: EnsembleKind,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        if let Some(new_members) = &params.members {
            return self
                .replace_ensemble_member_set(
                    ensemble_id,
                    params,
                    details,
                    owner_nodes,
                    new_members,
                    effective_kind,
                )
                .await;
        }
        if params.prompt_template.is_some()
            || params.timeout_minutes.is_some()
            || params.infra_retry_limit.is_some()
            || params.infra_crash_max_seconds.is_some()
            || params.infra_backoff_seconds.is_some()
        {
            self.propagate_ensemble_shared_config(params, details)
                .await?;
        }
        Ok(Ok(()))
    }

    async fn replace_ensemble_member_set(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
        details: &mut EnsembleDetails,
        owner_nodes: &[GraphNode],
        new_members: &[EnsembleMemberParams],
        effective_kind: EnsembleKind,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        let members = match validate_ensemble_members(new_members, effective_kind) {
            Ok(members) => members,
            Err(e) => return Ok(Err(error_result(&e))),
        };
        let defaults = EnsembleMemberDefaults::from_update(params, details);
        let old_members = details.members.clone();
        let start_position = owner_nodes
            .last()
            .map(|node| node.position + 1)
            .unwrap_or(1);
        let mut resulting_members = Vec::with_capacity(members.len());
        let mut resulting_nodes = Vec::with_capacity(members.len());
        for (index, spec) in members.iter().enumerate() {
            let (member, node) = build_resized_member_row(
                &details.ensemble,
                &defaults,
                spec,
                index,
                &old_members,
                owner_nodes,
                start_position,
            );
            resulting_members.push(member);
            resulting_nodes.push(node);
        }
        self.db
            .replace_ensemble_members(ensemble_id, &resulting_members, &resulting_nodes)
            .map_err(internal_error)?;

        *details = self
            .db
            .get_ensemble_details(ensemble_id)
            .map_err(internal_error)?
            .ok_or_else(|| {
                internal_error(format!("Ensemble '{ensemble_id}' vanished mid-update."))
            })?;
        Ok(Ok(()))
    }
    async fn propagate_ensemble_shared_config(
        &self,
        params: &GraphUpdateEnsembleParams,
        details: &EnsembleDetails,
    ) -> Result<(), McpError> {
        // Prompt and/or shared timeout changed without a member-list
        // resize: propagate onto every existing member's config as-is,
        // except a member with its own prompt_override keeps rendering
        // that instead of the (possibly just-changed) shared prompt.
        let defaults = EnsembleMemberDefaults::from_update(params, details);
        for member in &details.members {
            let effective_prompt = effective_member_prompt(
                member.prompt_override.as_deref(),
                defaults.prompt_template,
            );
            let effective_timeout_minutes =
                member.timeout_minutes.unwrap_or(defaults.timeout_minutes);
            let effective_infra_retry_limit =
                member.infra_retry_limit.or(defaults.infra_retry_limit);
            let effective_infra_crash_max_seconds = member
                .infra_crash_max_seconds
                .or(defaults.infra_crash_max_seconds);
            let effective_infra_backoff_seconds = member
                .infra_backoff_seconds
                .or(defaults.infra_backoff_seconds);
            let config = member_node_config(
                &member.platform,
                member.model.as_deref(),
                effective_prompt,
                effective_timeout_minutes,
                effective_infra_retry_limit,
                effective_infra_crash_max_seconds,
                effective_infra_backoff_seconds,
            );
            self.db
                .update_graph_node_details(&member.node_id, None, None, Some(&config), None)
                .map_err(internal_error)?;
        }
        Ok(())
    }

    async fn apply_ensemble_prompt_and_join_config(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
    ) -> Result<Result<(), CallToolResult>, McpError> {
        if let Some(prompt_template) = params.prompt_template.as_deref() {
            self.db
                .update_ensemble_prompt(ensemble_id, prompt_template.trim())
                .map_err(internal_error)?;
        }

        // ── join config: min_pass / straggler_timeout_minutes / quorum_grace_minutes / timeout_minutes / infra_retry_* ──
        if params.min_pass.is_some()
            || params.timeout_minutes.is_some()
            || params.straggler_timeout_minutes.is_some()
            || params.quorum_grace_minutes.is_some()
            || params.infra_retry_limit.is_some()
            || params.infra_crash_max_seconds.is_some()
            || params.infra_backoff_seconds.is_some()
        {
            if let Some(Some(straggler)) = params.straggler_timeout_minutes {
                if straggler < 0 {
                    return Ok(Err(error_result(
                        "straggler_timeout_minutes must not be negative.",
                    )));
                }
            }
            if let Some(Some(grace)) = params.quorum_grace_minutes {
                if grace < 0 {
                    return Ok(Err(error_result(
                        "quorum_grace_minutes must not be negative.",
                    )));
                }
            }
            if let Some(timeout_minutes) = params.timeout_minutes {
                if timeout_minutes < 0 {
                    return Ok(Err(error_result("timeout_minutes must not be negative.")));
                }
            }
            // infra_retry_limit/infra_crash_max_seconds/infra_backoff_seconds
            // are already validated above, before the member-resize/propagate
            // branches that read them.
            self.db
                .update_ensemble_join_config(
                    ensemble_id,
                    params.min_pass,
                    params.straggler_timeout_minutes,
                    params.quorum_grace_minutes,
                    params.timeout_minutes,
                    params.infra_retry_limit,
                    params.infra_crash_max_seconds,
                    params.infra_backoff_seconds,
                )
                .map_err(internal_error)?;
        }
        Ok(Ok(()))
    }
}
