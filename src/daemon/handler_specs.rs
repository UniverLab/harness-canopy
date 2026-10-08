//! MCP handler tools split out of `daemon::handler` (spec and blueprint tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::{TaskTriggerHandler, SPEC_LIST_DEFAULT_LIMIT};
use crate::daemon::handler_formatting::{
    build_id_result, build_spec_update_response, internal_error,
};
use crate::daemon::handler_run_exec::graph_run_blocker;
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_graph_exists, validate_non_empty, validate_position_conflict,
    validate_spec_deletable, validate_spec_exists, validate_spec_set_status_target,
    validate_spec_status, validate_spec_workdir,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{
    validate_spec_description_template, GraphRunStatus, GraphSpec, GraphSpecStatus,
    SpecAdminStatusOutcome,
};
use crate::domain::specs::{
    convert_legacy_spec_description, extract_spec_section, parse_spec_description,
    ParsedSpecDescription, SpecSection, SpecSectionResult,
};
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_specs, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_add_spec",
        description = "Add an ordered spec to an existing graph."
    )]
    pub(crate) async fn graph_add_spec(
        &self,
        Parameters(params): Parameters<GraphAddSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Graph spec name") {
            return Ok(error_result(&e));
        }
        let graph_id = match resolve_prefix_or_error(
            &self.db,
            params.graph_id.trim(),
            Database::resolve_graph_id_by_prefix,
            "graph",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        if let Err(e) = validate_graph_exists(&self.db, &graph_id) {
            return Ok(error_result(&e));
        }

        let existing_specs = self
            .db
            .list_graph_specs(&graph_id)
            .map_err(internal_error)?;
        if existing_specs
            .iter()
            .any(|spec| spec.position == params.position)
        {
            return Ok(error_result(&format!(
                "Graph '{graph_id}' already has a spec at position {}.",
                params.position
            )));
        }
        let Some(description) = params.description.as_deref().map(str::trim) else {
            return Ok(error_result(
                "Graph spec description is required and must follow the minimum template.",
            ));
        };
        if let Err(error) = validate_spec_description_template(description) {
            return Ok(error_result(&error));
        }

        let spec = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: Some(graph_id),
            name: name.to_string(),
            description: Some(description.to_string()),
            position: params.position,
            parallelizable: params.parallelizable,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        self.db.insert_graph_spec(&spec).map_err(internal_error)?;

        Ok(build_id_result(&spec.id, "spec_id"))
    }

    #[tool(
        name = "graph_update_spec",
        description = "Update an existing graph spec."
    )]
    pub(crate) async fn graph_update_spec(
        &self,
        Parameters(params): Parameters<GraphUpdateSpecParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let spec = match validate_spec_exists(&self.db, &spec_id) {
            Ok(spec) => spec,
            Err(e) => return Ok(error_result(&e)),
        };

        let name = match params.name.as_deref().map(str::trim) {
            Some("") => return Ok(error_result("Graph spec name must not be empty.")),
            Some(value) => Some(value),
            None => None,
        };
        let description = match params.description.as_deref().map(str::trim) {
            Some("") => {
                return Ok(error_result(
                    "Graph spec description must not be empty and must follow the template.",
                ))
            }
            Some(value) => {
                if let Err(error) = validate_spec_description_template(value) {
                    return Ok(error_result(&error));
                }
                Some(value)
            }
            None => None,
        };

        if let Some(position) = params.position {
            if let Err(e) =
                validate_position_conflict(&self.db, spec.graph_id.as_deref(), &spec_id, position)
            {
                return Ok(error_result(&e));
            }
        }

        if let Err(e) = validate_at_least_one_bool(
            &[
                name.is_some(),
                description.is_some(),
                params.position.is_some(),
                params.parallelizable.is_some(),
            ],
            "graph_update_spec",
        ) {
            return Ok(error_result(&e));
        }

        self.db
            .update_graph_spec_details(
                &spec_id,
                name,
                description,
                params.position,
                params.parallelizable,
            )
            .map_err(internal_error)?;

        Ok(build_spec_update_response(&spec_id))
    }

    #[tool(
        name = "graph_remove_spec",
        description = "Unbind a spec from a graph, making it a standalone backlog spec. The spec's execution history is preserved. Refuses if the spec is currently running."
    )]
    pub(crate) async fn graph_remove_spec(
        &self,
        Parameters(params): Parameters<GraphRemoveSpecParams>,
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
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        self.do_graph_remove_spec(&graph_id, &spec_id).await
    }

    #[tool(
        name = "spec_create",
        description = "Create a standalone spec (a backlog item) not yet assigned to any graph. Optionally tag it to a workdir for later filtering."
    )]
    pub(crate) async fn spec_create(
        &self,
        Parameters(params): Parameters<SpecCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Spec name") {
            return Ok(error_result(&e));
        }
        let description = params.description.trim();
        if let Err(e) = validate_non_empty(description, "Spec description") {
            return Ok(error_result(&e));
        }
        if let Err(error) = validate_spec_description_template(description) {
            return Ok(error_result(&error));
        }
        let workdir = match params.workdir.as_deref().map(str::trim) {
            Some("") | None => None,
            Some(value) => {
                if let Err(e) = validate_spec_workdir(value) {
                    return Ok(error_result(&e));
                }
                Some(value.to_string())
            }
        };

        let spec = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: None,
            name: name.to_string(),
            description: Some(description.to_string()),
            position: 0,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        self.db.insert_graph_spec(&spec).map_err(internal_error)?;

        Ok(build_id_result(&spec.id, "spec_id"))
    }

    #[tool(
        name = "spec_list",
        description = "List standalone/backlog specs, optionally filtered by workdir tag, status, or unassigned-only. \
         Supports limit/offset pagination; the default page fits the result budget. \
         Pass offset to page past it."
    )]
    pub(crate) async fn spec_list(
        &self,
        Parameters(params): Parameters<SpecListParams>,
    ) -> Result<CallToolResult, McpError> {
        let status = match params.status.as_deref().map(str::trim) {
            Some(value) => match validate_spec_status(value) {
                Ok(status) => Some(status),
                Err(e) => return Ok(error_result(&e)),
            },
            None => None,
        };

        let specs: Vec<_> = self
            .db
            .list_specs(
                params.workdir.as_deref(),
                status,
                params.unassigned_only.unwrap_or(false),
            )
            .map_err(internal_error)?
            .into_iter()
            // The blank-name row a spec-less / idea-driven run binds to its
            // graph to carry `{{spec_content}}` is engine bookkeeping, not work
            // — never a user-authored spec (`graph_add_spec` rejects an empty
            // name), so it must not surface here. Trimmed to match the
            // engine's bookkeeping sentinel (`is_no_spec_placeholder`).
            .filter(|s| !s.name.trim().is_empty())
            .collect();

        let include_descriptions = params.include_descriptions.unwrap_or(false);
        let limit = params
            .limit
            .unwrap_or(SPEC_LIST_DEFAULT_LIMIT)
            .clamp(1, 200) as usize;
        let offset = params.offset.unwrap_or(0) as usize;
        let total = specs.len();
        let visible: Vec<_> = specs.iter().skip(offset).take(limit).collect();
        let returned = visible.len();
        let remaining = total.saturating_sub(offset.saturating_add(returned));
        let truncated = remaining > 0;
        let omitted = remaining;

        let mut body = serde_json::json!({
            "specs": visible.iter().map(|s| spec_summary_json(s, include_descriptions)).collect::<Vec<_>>(),
            "total": total,
            "limit": limit,
            "offset": offset,
            "returned": returned,
        });
        if truncated {
            body["truncated"] = serde_json::json!(true);
            body["omitted"] = serde_json::json!(omitted);
        }

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    #[tool(
        name = "spec_update",
        description = "Update a standalone spec's name, description, and/or workdir tag."
    )]
    pub(crate) async fn spec_update(
        &self,
        Parameters(params): Parameters<SpecUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        if let Err(e) = validate_spec_exists(&self.db, &spec_id) {
            return Ok(error_result(&e));
        }

        let name = match params.name.as_deref().map(str::trim) {
            Some("") => return Ok(error_result("Spec name must not be empty.")),
            Some(value) => Some(value),
            None => None,
        };
        let description = match params.description.as_deref().map(str::trim) {
            Some("") => {
                return Ok(error_result(
                    "Spec description must not be empty and must follow the template.",
                ))
            }
            Some(value) => {
                if let Err(error) = validate_spec_description_template(value) {
                    return Ok(error_result(&error));
                }
                Some(value)
            }
            None => None,
        };
        let workdir = match &params.workdir {
            Some(Some(value)) => {
                let trimmed = value.trim();
                if let Err(e) = validate_spec_workdir(trimmed) {
                    return Ok(error_result(&e));
                }
                Some(Some(trimmed))
            }
            Some(None) => Some(None),
            None => None,
        };

        if let Err(e) = validate_at_least_one_bool(
            &[name.is_some(), description.is_some(), workdir.is_some()],
            "spec_update",
        ) {
            return Ok(error_result(&e));
        }

        self.db
            .update_spec_tag_details(&spec_id, name, description, workdir)
            .map_err(internal_error)?;

        Ok(build_spec_update_response(&spec_id))
    }

    #[tool(
        name = "spec_set_status",
        description = "Administratively transition a standalone spec's status (completed, skipped, or pending). Rejects if the spec is bound to a graph or has an active run."
    )]
    pub(crate) async fn spec_set_status(
        &self,
        Parameters(params): Parameters<SpecSetStatusParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        if let Err(e) = validate_spec_exists(&self.db, &spec_id) {
            return Ok(error_result(&e));
        }

        let status = match validate_spec_set_status_target(params.status.as_str()) {
            Ok(s) => s,
            Err(e) => return Ok(error_result(&e)),
        };

        let reason = params.reason.trim();
        if reason.is_empty() {
            return Ok(error_result("Reason must not be empty."));
        }

        match self.db.set_spec_admin_status(&spec_id, status, reason)
            .map_err(internal_error)? {
            SpecAdminStatusOutcome::Success => {
                Ok(success_result(&format!(
                    "Spec '{spec_id}' set to '{}' (admin): {reason}",
                    status.as_str()
                )))
            },
            SpecAdminStatusOutcome::NotFound => {
                Ok(error_result(&format!("Spec '{spec_id}' not found.")))
            },
            SpecAdminStatusOutcome::NotStandalone(graph_id) => {
                Ok(error_result(&format!(
                    "Spec '{spec_id}' is bound to graph '{graph_id}'; spec_set_status only \
                     administers standalone specs."
                )))
            },
            SpecAdminStatusOutcome::ActiveRun { graph_id, run_id } => {
                Ok(error_result(&format!(
                    "Spec '{spec_id}' is attached to an active run (graph '{graph_id}', run '{run_id}'); \
                     it cannot be administratively transitioned while running."
                )))
            },
        }
    }

    #[tool(
        name = "spec_delete",
        description = "Delete a standalone spec. Refuses to delete a spec that's bound to a graph."
    )]
    pub(crate) async fn spec_delete(
        &self,
        Parameters(params): Parameters<SpecDeleteParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let spec = match validate_spec_exists(&self.db, &spec_id) {
            Ok(spec) => spec,
            Err(e) => return Ok(error_result(&e)),
        };
        if let Err(e) = validate_spec_deletable(&spec) {
            return Ok(error_result(&e));
        }

        self.db
            .delete_graph_spec(&spec_id)
            .map_err(internal_error)?;

        Ok(success_result(&format!("Spec '{spec_id}' deleted.")))
    }

    #[tool(
        name = "spec_section_get",
        description = "Extract a single section from a spec by its canonical tag name. Returns the markdown body of the requested section."
    )]
    pub(crate) async fn spec_section_get(
        &self,
        Parameters(params): Parameters<SpecSectionGetParams>,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            Database::resolve_spec_id_by_prefix,
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let spec = match validate_spec_exists(&self.db, &spec_id) {
            Ok(spec) => spec,
            Err(e) => return Ok(error_result(&e)),
        };

        let section_name = params.section.trim();
        let Some(section) = SpecSection::from_tag(section_name) else {
            let valid = SpecSection::all()
                .iter()
                .map(|s| format!("<{}>", s.tag()))
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(error_result(&format!(
                "unknown section '{}'. Valid sections: {}",
                section_name, valid
            )));
        };

        let description = spec.description.as_deref().unwrap_or("");
        match extract_spec_section(description, section) {
            Ok(SpecSectionResult::Tagged { content, present }) => {
                let body = serde_json::json!({
                    "spec_id": spec_id,
                    "section": section.tag(),
                    "content": content,
                    "present": present,
                    "needs_conversion": false,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&body).unwrap_or_default(),
                )]))
            }
            Ok(SpecSectionResult::Legacy { content, present }) => {
                let body = serde_json::json!({
                    "spec_id": spec_id,
                    "section": section.tag(),
                    "content": content,
                    "present": present,
                    "needs_conversion": true,
                });
                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&body).unwrap_or_default(),
                )]))
            }
            Err(e) => Ok(error_result(&format!(
                "failed to extract section <{}> from spec '{}': {}",
                section.tag(),
                spec_id,
                e
            ))),
        }
    }

    #[tool(
        name = "spec_convert",
        description = "Convert all legacy heading-format specs to the tagged <spec> format. Skips already-tagged specs and running specs. Returns a structured report."
    )]
    pub(crate) async fn spec_convert(&self) -> Result<CallToolResult, McpError> {
        let all_specs = self
            .db
            .list_all_specs_for_conversion()
            .map_err(internal_error)?;

        let mut scanned = 0u64;
        let mut converted = 0u64;
        let mut already_tagged = 0u64;
        let mut deferred_running = 0u64;
        let mut invalid_legacy = 0u64;
        let mut converted_ids: Vec<String> = Vec::new();
        let mut deferred: Vec<serde_json::Value> = Vec::new();
        let mut errors: Vec<serde_json::Value> = Vec::new();

        for spec in &all_specs {
            scanned += 1;
            let description = spec.description.as_deref().unwrap_or("");

            if description.trim().is_empty() {
                continue;
            }

            match parse_spec_description(description) {
                Ok(ParsedSpecDescription::Tagged(_)) => {
                    already_tagged += 1;
                }
                Ok(ParsedSpecDescription::Legacy(_)) => {
                    if spec.status == GraphSpecStatus::Running {
                        deferred_running += 1;
                        deferred.push(serde_json::json!({
                            "spec_id": spec.id,
                            "reason": "spec is running; conversion deferred so a running graph cannot observe a mid-flight format change",
                        }));
                        continue;
                    }
                    match convert_legacy_spec_description(description) {
                        Ok(new_body) => {
                            match self
                                .db
                                .update_spec_description_if_not_running(&spec.id, &new_body)
                            {
                                Ok(true) => {
                                    converted += 1;
                                    converted_ids.push(spec.id.clone());
                                }
                                Ok(false) => {
                                    deferred_running += 1;
                                    deferred.push(serde_json::json!({
                                        "spec_id": spec.id,
                                        "reason": "spec became running before the write; conversion deferred",
                                    }));
                                }
                                Err(e) => {
                                    invalid_legacy += 1;
                                    errors.push(serde_json::json!({
                                        "spec_id": spec.id,
                                        "error": e.to_string(),
                                    }));
                                }
                            }
                        }
                        Err(e) => {
                            invalid_legacy += 1;
                            errors.push(serde_json::json!({
                                "spec_id": spec.id,
                                "error": e.to_string(),
                            }));
                        }
                    }
                }
                Err(_) => {
                    if spec.status == GraphSpecStatus::Running {
                        deferred_running += 1;
                        deferred.push(serde_json::json!({
                            "spec_id": spec.id,
                            "reason": "spec is running and body does not parse; left untouched for human repair after it stops running",
                        }));
                    } else {
                        invalid_legacy += 1;
                        errors.push(serde_json::json!({
                            "spec_id": spec.id,
                            "error": "unparseable spec body",
                        }));
                    }
                }
            }
        }

        let report = serde_json::json!({
            "scanned": scanned,
            "converted": converted,
            "already_tagged": already_tagged,
            "deferred_running": deferred_running,
            "invalid_legacy": invalid_legacy,
            "converted_ids": converted_ids,
            "deferred": deferred,
            "errors": errors,
        });

        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string_pretty(&report).unwrap_or_default(),
        )]))
    }
}

pub(crate) fn validate_at_least_one_bool(updates: &[bool], field_name: &str) -> Result<(), String> {
    if updates.iter().all(|&b| !b) {
        Err(format!(
            "{field_name} requires at least one field to update."
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn spec_summary_json(spec: &GraphSpec, include_descriptions: bool) -> serde_json::Value {
    if include_descriptions {
        let mut obj = serde_json::json!({
            "id": spec.id,
            "graph_id": spec.graph_id,
            "name": spec.name,
            "description": spec.description,
            "workdir": spec.workdir,
            "position": spec.position,
            "parallelizable": spec.parallelizable,
            "status": spec.status.as_str(),
        });
        if let Some(via) = &spec.completed_via {
            obj["completed_via"] = serde_json::json!(via);
        }
        obj
    } else {
        serde_json::json!({
            "id": spec.id,
            "graph_id": spec.graph_id,
            "name": spec.name,
            "status": spec.status.as_str(),
            "workdir": spec.workdir,
        })
    }
}

pub(crate) struct SpecRunInfo {
    pub(crate) name: String,
    pub(crate) current_node: Option<String>,
    pub(crate) blocker: Option<String>,
    /// C19: how many separate graph executions this spec has failed with a
    /// genuine verdict — the persisted counter behind the cross-run attempt
    /// budget. Surfaced so an operator can see a spec approaching the limit
    /// before it actually blocks the graph, not just after.
    pub(crate) cross_run_attempts: i64,
}

pub(crate) fn build_spec_run_info(
    db: &Database,
    spec: &GraphSpec,
) -> Result<SpecRunInfo, McpError> {
    let runs = db
        .list_graph_runs_for_spec(&spec.id)
        .map_err(internal_error)?;
    let current_node = runs
        .iter()
        .rev()
        .find(|run| run.status == GraphRunStatus::Running)
        .or_else(|| runs.last())
        .map(|run| run.node_id.clone());
    let blocker = runs.last().and_then(graph_run_blocker);
    let cross_run_attempts = db
        .get_graph_spec_cross_run_attempts(&spec.id)
        .map_err(internal_error)?;
    Ok(SpecRunInfo {
        name: spec.name.clone(),
        current_node,
        blocker,
        cross_run_attempts,
    })
}
