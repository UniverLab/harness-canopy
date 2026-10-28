//! MCP handler tools split out of `daemon::handler` (spec and blueprint tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::{TaskTriggerHandler, SPEC_LIST_DEFAULT_LIMIT};
use crate::daemon::handler_formatting::{
    build_id_result, build_spec_update_response, internal_error,
};
use crate::daemon::handler_specs_helpers::{
    convert_specs, spec_create_result, spec_summary_json, validate_at_least_one_bool,
};
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_non_empty, validate_position_conflict,
    validate_spec_deletable, validate_spec_exists, validate_spec_set_status_target,
    validate_spec_status, validate_spec_workdir,
};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{
    validate_spec_description_template, GraphSpec, GraphSpecStatus, SpecAdminStatusOutcome,
};
use crate::domain::spec_type::{parse_conventional_title, resolve_spec_naming};
use crate::domain::specs::{extract_spec_section, SpecSection, SpecSectionResult};
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_specs, vis = "pub(crate)")]
impl TaskTriggerHandler {
    pub(crate) async fn graph_add_spec_impl(
        &self,
        params: GraphAddSpecParams,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Graph spec name") {
            return Ok(error_result(&e));
        }
        let Ok(Some(graph)) = self.db.get_graph(&match resolve_prefix_or_error(
            &self.db,
            params.graph_id.trim(),
            Database::resolve_graph_id_by_prefix,
            "graph",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        }) else {
            return Ok(error_result(&format!(
                "Graph '{}' not found.",
                params.graph_id.trim()
            )));
        };
        let graph_id = graph.id.clone();
        // SPECCODE: a conventional title (`fix(tui): ...`) without an explicit
        // type is parsed — type and scope come from the title, the name is
        // stored as the subject alone; explicit type/scope beat the title.
        let naming = resolve_spec_naming(params.spec_type, params.spec_scope, name);
        if graph.require_spec_type && naming.spec_type.is_none() {
            return Ok(error_result(&format!(
                "Spec '{}' has no type: graph '{}' requires a spec type (one of feat, fix, \
                 docs, style, refactor, perf, test, build, ci, chore, revert) before it can \
                 accept specs or run, so check nodes get a deterministic commit message.",
                naming.name, graph.name
            )));
        }
        // SPECCODE: the code counts against the graph's workdir and is
        // never settable by callers.
        let code = match &naming.spec_type {
            Some(spec_type) => Some(
                self.db
                    .next_spec_code(&graph.workdir, *spec_type)
                    .map_err(internal_error)?,
            ),
            None => None,
        };

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
            name: naming.name.clone(),
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
            spec_type: naming.spec_type,
            spec_scope: naming.scope,
            spec_code: code,
        };

        self.db.insert_graph_spec(&spec).map_err(internal_error)?;

        Ok(build_id_result(&spec.id, "spec_id"))
    }

    pub(crate) async fn graph_update_spec_impl(
        &self,
        params: GraphUpdateSpecParams,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
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

        // SPECCODE: graph_edit mirrors spec_edit update's type/scope flow,
        // including the legacy-spec code assignment on first type.
        if let Some(spec_type) = params.spec_type {
            let code: Option<String> = if spec.spec_code.is_some() {
                None
            } else {
                let key = self
                    .db
                    .spec_code_counter_key(&spec)
                    .map_err(internal_error)?;
                Some(
                    self.db
                        .next_spec_code(&key, spec_type)
                        .map_err(internal_error)?,
                )
            };
            let scope = params
                .spec_scope
                .as_deref()
                .map(str::trim)
                .filter(|scope| !scope.is_empty())
                .map(str::to_string)
                .or_else(|| spec.spec_scope.clone());
            self.db
                .set_graph_spec_type(&spec_id, spec_type, scope.as_deref(), code.as_deref())
                .map_err(internal_error)?;
        } else if let Some(scope) = params.spec_scope.as_deref().map(str::trim) {
            if let Some(existing_type) = spec.spec_type {
                self.db
                    .set_graph_spec_type(&spec_id, existing_type, Some(scope), None)
                    .map_err(internal_error)?;
            }
        }

        if let Err(e) = validate_at_least_one_bool(
            &[
                name.is_some(),
                description.is_some(),
                params.position.is_some(),
                params.parallelizable.is_some(),
                params.spec_type.is_some(),
                params.spec_scope.is_some(),
            ],
            "graph_edit",
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

    pub(crate) async fn graph_remove_spec_impl(
        &self,
        params: GraphRemoveSpecParams,
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
        // Fetch the graph so the spec code is resolved within the graph's
        // workdir (requirement 6: codes resolved within the graph's workdir).
        let graph = match self.db.get_graph(&graph_id) {
            Ok(Some(g)) => g,
            Ok(None) => {
                return Ok(error_result(&format!(
                    "Graph '{}' not found.",
                    params.graph_id.trim()
                )))
            }
            Err(e) => return Err(internal_error(&e)),
        };
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, Some(&graph.workdir)),
            "spec",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        self.do_graph_remove_spec(&graph_id, &spec_id).await
    }

    pub(crate) async fn spec_edit_create_impl(
        &self,
        params: SpecCreateParams,
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

        // SPECCODE: a conventional title (`fix(tui): ...`) without an
        // explicit type is parsed — type and scope come from the title and
        // the name is stored as the subject alone; an explicit type/scope
        // beat the title. Untyped, non-conventional creates still succeed
        // (never backfilled) but answer with a `warning`.
        let naming = resolve_spec_naming(params.spec_type, params.spec_scope, name);
        // The code counts against the spec's own workdir tag (backlog specs
        // are not graph-bound yet), empty string when untagged.
        let code = match &naming.spec_type {
            Some(spec_type) => Some(
                self.db
                    .next_spec_code(workdir.as_deref().unwrap_or(""), *spec_type)
                    .map_err(internal_error)?,
            ),
            None => None,
        };
        let spec = GraphSpec {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id: None,
            name: naming.name.clone(),
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
            spec_type: naming.spec_type,
            spec_scope: naming.scope.clone(),
            spec_code: code,
        };
        self.db.insert_graph_spec(&spec).map_err(internal_error)?;
        Ok(spec_create_result(&spec.id, &naming))
    }

    pub(crate) async fn spec_list_impl(
        &self,
        params: SpecListParams,
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
                params.spec_type,
            )
            .map_err(internal_error)?
            .into_iter()
            // The blank-name row a spec-less / idea-driven run binds to its
            // graph to carry `{{spec_content}}` is engine bookkeeping, not work
            // — never a user-authored spec (`graph_edit` rejects an empty
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

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&body).unwrap_or_default(),
        )]))
    }

    /// The `spec` `get` action. No standalone `spec_get` tool ever existed;
    /// this is the row `spec` `list` would return for the one spec, with its
    /// full description.
    pub(crate) async fn spec_get_impl(
        &self,
        params: SpecGetParams,
    ) -> Result<CallToolResult, McpError> {
        let spec = match validate_spec_exists(&self.db, params.spec_id.trim()) {
            Ok(spec) => spec,
            Err(e) => return Ok(error_result(&e)),
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&spec_summary_json(&spec, true)).unwrap_or_default(),
        )]))
    }

    pub(crate) async fn spec_edit_update_impl(
        &self,
        params: SpecUpdateParams,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
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

        self.apply_spec_type_update(&spec, &spec_id, &params, name)?;

        let adopts_type_by_name = params.spec_type.is_none()
            && params.spec_scope.is_none()
            && spec.spec_type.is_none()
            && name.is_some_and(|n| parse_conventional_title(n).is_some());
        if let Err(e) = validate_at_least_one_bool(
            &[
                name.is_some(),
                description.is_some(),
                workdir.is_some(),
                params.spec_type.is_some(),
                params.spec_scope.is_some(),
                adopts_type_by_name,
            ],
            "spec_edit update",
        ) {
            return Ok(error_result(&e));
        }

        // A name-only update that adopted a conventional title on an untyped
        // spec already stored its subject in the adoption branch above; the
        // pass-through write below must not overwrite it with the full title
        // again (the commit message would repeat the type).
        self.db
            .update_spec_tag_details(
                &spec_id,
                if adopts_type_by_name { None } else { name },
                description,
                workdir,
            )
            .map_err(internal_error)?;

        Ok(build_spec_update_response(&spec_id))
    }

    // SPECCODE: type/scope flow like every other updatable field. A type
    // set on a legacy spec (no code yet) assigns its immutable code at
    // this moment; a later type change keeps the existing code (never
    // renumbered). Where the code counts: the bound graph's workdir,
    // else the spec's own workdir tag, both else "".
    fn apply_spec_type_update(
        &self,
        spec: &GraphSpec,
        spec_id: &str,
        params: &SpecUpdateParams,
        name: Option<&str>,
    ) -> Result<(), McpError> {
        if let Some(spec_type) = params.spec_type {
            let code: Option<String> = if spec.spec_code.is_some() {
                None
            } else {
                let key = self
                    .db
                    .spec_code_counter_key(spec)
                    .map_err(internal_error)?;
                Some(
                    self.db
                        .next_spec_code(&key, spec_type)
                        .map_err(internal_error)?,
                )
            };
            let scope: Option<String> = match params.spec_scope.as_ref().and_then(|s| s.as_deref())
            {
                Some(scope) if !scope.trim().is_empty() => Some(scope.trim().to_string()),
                _ => spec.spec_scope.as_ref().cloned(),
            };
            self.db
                .set_graph_spec_type(spec_id, spec_type, scope.as_deref(), code.as_deref())
                .map_err(internal_error)?;
        } else if let Some(scope) = &params.spec_scope {
            // Scope-only tweak: needs the existing type to keep the shape.
            if let Some(existing_type) = spec.spec_type {
                self.db
                    .set_graph_spec_type(spec_id, existing_type, scope.as_deref(), None)
                    .map_err(internal_error)?;
            }
        } else if let Some(new_name) = name {
            // A conventional title adopted through a name-only update on an
            // untyped spec: the title's type and scope land with it, and the
            // stored name becomes the subject alone.
            if spec.spec_type.is_none() {
                let naming = resolve_spec_naming(None, None, new_name);
                if let Some(spec_type) = naming.spec_type {
                    let key = self
                        .db
                        .spec_code_counter_key(spec)
                        .map_err(internal_error)?;
                    let code = self
                        .db
                        .next_spec_code(&key, spec_type)
                        .map_err(internal_error)?;
                    self.db
                        .set_graph_spec_type(
                            spec_id,
                            spec_type,
                            naming.scope.as_deref(),
                            Some(&code),
                        )
                        .map_err(internal_error)?;
                    self.db
                        .update_spec_tag_details(spec_id, Some(&naming.name), None, None)
                        .map_err(internal_error)?;
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn spec_edit_set_status_impl(
        &self,
        params: SpecSetStatusParams,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
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
                    "Spec '{spec_id}' is bound to graph '{graph_id}'; spec_edit set_status only \
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

    pub(crate) async fn spec_edit_delete_impl(
        &self,
        params: SpecDeleteParams,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
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

    pub(crate) async fn spec_section_impl(
        &self,
        params: SpecSectionGetParams,
    ) -> Result<CallToolResult, McpError> {
        let spec_id = match resolve_prefix_or_error(
            &self.db,
            params.spec_id.trim(),
            |db, id| db.resolve_spec_id_by_prefix(id, None),
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
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
                Ok(CallToolResult::success(vec![ContentBlock::text(
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

        let report = convert_specs(&self.db, &all_specs)?;

        let report_json = serde_json::json!({
            "scanned": report.scanned,
            "converted": report.converted,
            "already_tagged": report.already_tagged,
            "deferred_running": report.deferred_running,
            "invalid_legacy": report.invalid_legacy,
            "converted_ids": report.converted_ids,
            "deferred": report.deferred,
            "errors": report.errors,
        });

        Ok(CallToolResult::success(vec![ContentBlock::text(
            serde_json::to_string_pretty(&report_json).unwrap_or_default(),
        )]))
    }
}
