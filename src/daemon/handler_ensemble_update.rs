//! MCP handler tools split out of `daemon::handler` (ensemble update tool).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_ensemble_core::{
    effective_member_prompt, member_node_config, validate_ensemble_members,
};
use crate::daemon::handler_ensemble_plan::{
    build_resized_member_row, check_resulting_min_pass, parse_ensemble_kind,
    refuse_ensemble_kind_change, reject_crew_update_combination, reject_empty_ensemble_update,
    validate_ensemble_infra_updates, EnsembleMemberDefaults,
};
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_helpers::record_config_change_if_running;
use crate::daemon::handler_validate::{resolve_prefix_or_error, validate_non_empty};
use crate::daemon::helpers::{error_result, success_result, EarlyReturn};
use crate::daemon::params::*;
use crate::db::ensembles::EnsembleJoinConfigPatch;
use crate::db::Database;
use crate::domain::graphs::{Ensemble, EnsembleDetails, EnsembleKind, GraphNode};
use crate::or_early_return;
use rmcp::model::CallToolResult;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_ensemble_update, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_update_ensemble_impl(
        &self,
        params: GraphUpdateEnsembleParams,
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

        if let Some(result) = crew_binding_update_result(&self.db, &ensemble_id, &details, &params)
        {
            return Ok(result);
        }

        let old_kind = details.ensemble.kind.as_str().to_string();
        let old_crew = details.ensemble.crew_name.clone();
        let (new_kind, effective_kind) = or_early_return!(validate_ensemble_update_preamble(
            &self.db, &params, &details
        ));

        if let Err(e) = refuse_ensemble_kind_change(&params, effective_kind) {
            return Ok(error_result(&e));
        }

        let () = or_early_return!(check_update_template_bindings(&self.db, &params, &details));

        let cleared_grace_fields_for_kind_change = or_early_return!(
            self.apply_ensemble_config_updates(&ensemble_id, &params, &mut details, new_kind)
                .await
        );

        let note = if cleared_grace_fields_for_kind_change {
            format!(
                " Cleared straggler_timeout_minutes/quorum_grace_minutes: not meaningful for a {} ensemble.",
                effective_kind.as_str()
            )
        } else {
            String::new()
        };
        if new_kind.is_some() {
            crate::daemon::handler_ensemble_events::record_ensemble_change_if_running(
                &self.db,
                &details.ensemble,
                &old_kind,
                effective_kind.as_str(),
                old_crew.as_deref(),
                details.ensemble.crew_name.as_deref(),
            );
        } else {
            record_ensemble_config_change(&self.db, &details.ensemble);
        }
        Ok(success_result(&format!(
            "Ensemble '{ensemble_id}' updated.{note}"
        )))
    }

    async fn apply_ensemble_config_updates(
        &self,
        ensemble_id: &str,
        params: &GraphUpdateEnsembleParams,
        details: &mut EnsembleDetails,
        new_kind: Option<EnsembleKind>,
    ) -> Result<bool, EarlyReturn> {
        // Refuse the combined rights before writing anything: a rejected
        // update must leave the ensemble exactly as it was.
        let next_commit_rights = params
            .commit_rights
            .unwrap_or(details.ensemble.commit_rights);
        let next_read_only = params.read_only.unwrap_or(details.ensemble.read_only);
        if next_commit_rights && next_read_only {
            return Err(EarlyReturn::tool_error(
                "Ensemble cannot set both 'read_only' and 'commit_rights' to true.",
            ));
        }
        let effective_kind = new_kind.unwrap_or(details.ensemble.kind);
        let cleared_grace_fields_for_kind_change = self
            .apply_ensemble_kind_change(ensemble_id, params, &mut *details, new_kind)
            .await?;
        // CM28: `commit_rights` is CONFIG, not topology — free to edit on a
        // running graph, same as prompt/quorum config, so deliberately not
        // gated by `validate_topology_mutation_allowed`.
        if let Some(commit_rights) = params.commit_rights {
            self.db
                .update_ensemble_commit_rights(ensemble_id, commit_rights)
                .map_err(internal_error)?;
            details.ensemble.commit_rights = commit_rights;
        }
        if let Some(read_only) = params.read_only {
            self.db
                .update_ensemble_read_only(ensemble_id, read_only)
                .map_err(internal_error)?;
            details.ensemble.read_only = read_only;
        }
        // `mutex` is CONFIG like `commit_rights` (read fresh at each member
        // dispatch) — editable on a running graph. `None` keeps, `[]` clears.
        if let Some(mutex) = params.mutex.as_deref() {
            self.db
                .update_ensemble_mutex(ensemble_id, mutex)
                .map_err(internal_error)?;
            details.ensemble.mutex = mutex.to_vec();
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
                ensemble_id,
                params,
                &mut *details,
                &owner_nodes,
                effective_kind,
            )
            .await?
        {
            Ok(()) => {}
            Err(e) => return Err(EarlyReturn::ok(e)),
        }
        match self
            .apply_ensemble_prompt_and_join_config(ensemble_id, params)
            .await?
        {
            Ok(()) => {}
            Err(e) => return Err(EarlyReturn::ok(e)),
        }
        match self
            .apply_ensemble_entry_wiring(ensemble_id, params, &mut *details, &owner_nodes)
            .await?
        {
            Ok(()) => {}
            Err(e) => return Err(EarlyReturn::ok(e)),
        }
        match self
            .apply_ensemble_exit_wiring(ensemble_id, params, &*details)
            .await?
        {
            Ok(()) => {}
            Err(e) => return Err(EarlyReturn::ok(e)),
        }
        Ok(cleared_grace_fields_for_kind_change)
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
            self.db
                .update_ensemble_kind(ensemble_id, Some(kind), None)
                .map_err(internal_error)?;
            details.ensemble.kind = kind;
            // The stored `round_robin_index` cursor is kept, not cleared, on
            // a kind change: switching away from round_robin and back resumes
            // the rotation where it left off. Other kinds ignore the cursor,
            // and an in-flight dispatch keeps its own pins regardless.

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
                            EnsembleJoinConfigPatch {
                                straggler_timeout_minutes: clear_straggler.then_some(None),
                                quorum_grace_minutes: clear_grace.then_some(None),
                                ..Default::default()
                            },
                        )
                        .map_err(internal_error)?;
                    clear_stale_kind_fields(&mut details.ensemble, clear_straggler, clear_grace);
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
        let caps =
            crate::domain::graphs::MemberCaps::from_graphs(
                &crate::domain::canopy_config::CanopyConfig::load(
                    &crate::domain::prompts::canopy_dir(),
                )
                .graphs,
            );
        let members = match validate_ensemble_members(new_members, effective_kind, caps) {
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
                    EnsembleJoinConfigPatch {
                        min_pass: params.min_pass,
                        straggler_timeout_minutes: params.straggler_timeout_minutes,
                        quorum_grace_minutes: params.quorum_grace_minutes,
                        timeout_minutes: params.timeout_minutes,
                        infra_retry_limit: params.infra_retry_limit,
                        infra_crash_max_seconds: params.infra_crash_max_seconds,
                        infra_backoff_seconds: params.infra_backoff_seconds,
                    },
                )
                .map_err(internal_error)?;
        }
        Ok(Ok(()))
    }
}

/// CM49: binding/unbinding a crew is its own operation — never mixed
/// with a literal member list or any other field. Returns `Some` result
/// when `params.crew` is set (`Some(name)` binds, `null` unbinds and
/// keeps the current rows as literal members). A crew bind is a live
/// change: the engine re-reads the member set at each dispatch, so it is
/// allowed on a running graph; a crew whose size is outside the
/// ensemble kind's bounds is refused by `bind_ensemble_to_crew` with the
/// usual member-count message and changes nothing.
fn crew_binding_update_result(
    db: &Database,
    ensemble_id: &str,
    details: &EnsembleDetails,
    params: &GraphUpdateEnsembleParams,
) -> Option<CallToolResult> {
    let crew = params.crew.as_ref()?;
    if let Err(e) = reject_crew_update_combination(params) {
        return Some(error_result(&e));
    }
    let kind = details.ensemble.kind.as_str().to_string();
    let old_crew = details.ensemble.crew_name.clone();
    Some(match crew {
        Some(name) => match db.bind_ensemble_to_crew(ensemble_id, name) {
            Ok(_) => {
                crate::daemon::handler_ensemble_events::record_ensemble_change_if_running(
                    db,
                    &details.ensemble,
                    &kind,
                    &kind,
                    old_crew.as_deref(),
                    Some(name.as_str()),
                );
                success_result(&format!("Ensemble '{ensemble_id}' bound to crew '{name}'."))
            }
            Err(e) => error_result(&e.to_string()),
        },
        None => match db.unbind_ensemble_crew(ensemble_id) {
            Ok(()) => {
                crate::daemon::handler_ensemble_events::record_ensemble_change_if_running(
                    db,
                    &details.ensemble,
                    &kind,
                    &kind,
                    old_crew.as_deref(),
                    None,
                );
                success_result(&format!(
                    "Ensemble '{ensemble_id}' unbound; its member rows are literal now."
                ))
            }
            Err(e) => error_result(&e.to_string()),
        },
    })
}

/// Static validations before any write, plus the parsed kind change and
/// the kind it results in.
fn validate_ensemble_update_preamble(
    _db: &Database,
    params: &GraphUpdateEnsembleParams,
    details: &EnsembleDetails,
) -> Result<(Option<EnsembleKind>, EnsembleKind), EarlyReturn> {
    if let Err(e) = validate_ensemble_infra_updates(params) {
        return Err(EarlyReturn::tool_error(&e));
    }
    if let Err(e) = crate::domain::validation::validate_mutex_list(
        &format!("Ensemble '{}'", details.ensemble.name),
        params.mutex.as_deref(),
    ) {
        return Err(EarlyReturn::tool_error(&e));
    }
    if let Err(e) = check_resulting_min_pass(params, details) {
        return Err(EarlyReturn::tool_error(&e));
    }
    // A kind change is a live change: the engine re-reads the ensemble's
    // kind from the database at each dispatch, so it is allowed on a
    // running graph — no `validate_topology_mutation_allowed` gate here.
    // Entry/exit wiring stays topology-pinned and keeps its own gates.
    if let Some(prompt_template) = &params.prompt_template {
        if let Err(e) = validate_non_empty(prompt_template.trim(), "Ensemble prompt_template") {
            return Err(EarlyReturn::tool_error(&e));
        }
    }
    let new_kind = match parse_ensemble_kind(params) {
        Ok(kind) => kind,
        Err(e) => return Err(EarlyReturn::ok(e)),
    };
    let effective_kind = new_kind.unwrap_or(details.ensemble.kind);
    // FR5: a kind or crew change may land outside the new kind's member
    // bounds (switching to a kind with fewer members than the ensemble
    // holds, or binding a crew of the wrong size). Refuse before any write
    // with the usual member-count message so a rejected change leaves the
    // ensemble exactly as it was. This checks the RESULTING member count
    // (params.members when the call also resizes, else the stored count)
    // against the effective kind's full bounds — a combined shrink plus
    // kind switch is accepted, only an out-of-bounds result is refused.
    // A members-only resize to an out-of-bounds count is refused below in
    // `replace_ensemble_member_set` with the same message form.
    let resulting_member_count = params
        .members
        .as_ref()
        .map(|m| m.len())
        .unwrap_or(details.members.len());
    let caps = crate::domain::graphs::MemberCaps::from_graphs(
        &crate::domain::canopy_config::CanopyConfig::load(&crate::domain::prompts::canopy_dir())
            .graphs,
    );
    let (min_for_effective, max_for_effective) =
        crate::domain::graphs::member_bounds(effective_kind, caps);
    if resulting_member_count < min_for_effective || resulting_member_count > max_for_effective {
        return Err(EarlyReturn::tool_error(
            &crate::domain::graphs::member_count_message(
                effective_kind,
                caps,
                resulting_member_count,
            ),
        ));
    }
    Ok((new_kind, effective_kind))
}

/// Same write-time binding check as `graph_ensemble`, against the
/// resulting shared prompt and member overrides: the scope already
/// holds this ensemble (with its quorum name, so a self-reference gets
/// the FR2 message) and its current member and quorum nodes.
fn check_update_template_bindings(
    db: &Database,
    params: &GraphUpdateEnsembleParams,
    details: &EnsembleDetails,
) -> Result<(), EarlyReturn> {
    if params.prompt_template.is_none() && params.members.is_none() {
        return Ok(());
    }
    let scope = crate::daemon::handler_templates::TemplateScope::load(
        db,
        details.ensemble.spec_id.as_deref(),
        details.ensemble.graph_id.as_deref(),
    )
    .map_err(internal_error)?;
    let shared = params
        .prompt_template
        .as_deref()
        .unwrap_or(&details.ensemble.prompt_template);
    let owner = format!(
        "Ensemble '{}' shared prompt template",
        details.ensemble.name
    );
    if let Err(e) = crate::daemon::handler_templates::check_template_bindings(
        &owner,
        shared,
        crate::graph_engine::AGENT_PROMPT_BINDINGS,
        &scope.nodes,
        &scope.ensembles,
    ) {
        return Err(EarlyReturn::tool_error(&e));
    }
    // The resulting overrides: the replacement set when members are
    // resized, else the stored ones (a shared-prompt change still has
    // to leave every override member spawnable).
    let overrides: Vec<(usize, &str)> = match &params.members {
        Some(new_members) => new_members
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.prompt_override.as_deref().map(|t| (i, t)))
            .collect(),
        None => details
            .members
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.prompt_override.as_deref().map(|t| (i, t)))
            .collect(),
    };
    for (index, template) in overrides {
        let member_owner = format!(
            "Ensemble '{}' member {} prompt override",
            details.ensemble.name,
            index + 1
        );
        if let Err(e) = crate::daemon::handler_templates::check_template_bindings(
            &member_owner,
            template,
            crate::graph_engine::AGENT_PROMPT_BINDINGS,
            &scope.nodes,
            &scope.ensembles,
        ) {
            return Err(EarlyReturn::tool_error(&e));
        }
    }
    Ok(())
}

/// CM46: an ensemble edit on a running graph is narratable history.
/// A spec-owned ensemble carries no graph_id of its own — resolve the
/// owning graph through its spec, the way node edits do, or the edit
/// on a running graph's spec-owned ensemble would go unrecorded.
fn record_ensemble_config_change(db: &Database, ensemble: &Ensemble) {
    let owning_graph_id = ensemble.graph_id.clone().or_else(|| {
        ensemble.spec_id.as_deref().and_then(|sid| {
            db.get_graph_spec(sid)
                .ok()
                .flatten()
                .and_then(|spec| spec.graph_id)
        })
    });
    record_config_change_if_running(
        db,
        owning_graph_id.as_deref(),
        ensemble.spec_id.as_deref(),
        None,
        "ensemble",
        format!("Ensemble '{}' config updated.", ensemble.name),
    );
}

fn clear_stale_kind_fields(ensemble: &mut Ensemble, clear_straggler: bool, clear_grace: bool) {
    if clear_straggler {
        ensemble.straggler_timeout_minutes = None;
    }
    if clear_grace {
        ensemble.quorum_grace_minutes = None;
    }
}
