//! `spec_edit update` implementation, split out of `handler_specs` to keep
//! that file under the repo's 800-code-line budget.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{build_spec_update_response, internal_error};
use crate::daemon::handler_specs_helpers::validate_at_least_one_bool;
use crate::daemon::handler_specs_helpers::{
    normalize_spec_intent, spec_review_enabled, INTENT_REQUIRED,
};
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_spec_exists, validate_spec_workdir,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::db::Database;
use crate::domain::graphs::{validate_spec_description_template, GraphSpec};
use crate::domain::spec_type::{check_subject, resolve_spec_naming};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

/// The validated `spec_edit update` writes, borrowed from the params where
/// possible. See [`TaskTriggerHandler::spec_edit_update_impl`].
struct SpecUpdateInputs<'a> {
    name: Option<&'a str>,
    description: Option<&'a str>,
    workdir: Option<Option<&'a str>>,
    update_intent: Option<Option<String>>,
}

impl TaskTriggerHandler {
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
        let mut spec = match validate_spec_exists(&self.db, &spec_id) {
            Ok(spec) => spec,
            Err(e) => return Ok(error_result(&e)),
        };
        let review_enabled = spec_review_enabled()?;
        let inputs = match Self::parse_spec_update_inputs(&params, &spec, review_enabled) {
            Ok(inputs) => inputs,
            Err(e) => return Ok(e),
        };

        // SPECCODE: a conventional title in `name` is parsed exactly like
        // `create`; the stored name is the subject alone; a type change
        // through the name never re-codes a spec that already has a code.
        let store_name = match Self::apply_spec_update_naming(
            &self.db,
            &spec_id,
            &mut spec,
            inputs.name,
            params.spec_type,
            &params.spec_scope,
        ) {
            Ok(Ok(store_name)) => store_name,
            Ok(Err(e)) => return Ok(e),
            Err(e) => return Err(e),
        };

        // SPECCODE: a first workdir set on an unbound spec with no runs
        // re-codes it under the new workdir's counter — a spec created
        // without a workdir drew its code from the empty-workdir counter,
        // which must not repeat a code the project already owns.
        self.apply_workdir_recoding(&mut spec, inputs.workdir.as_ref())?;

        if let Err(e) = validate_at_least_one_bool(
            &[
                inputs.name.is_some(),
                inputs.description.is_some(),
                inputs.workdir.is_some(),
                params.spec_type.is_some(),
                params.spec_scope.is_some(),
                inputs.update_intent.is_some(),
            ],
            "spec_edit update",
        ) {
            return Ok(error_result(&e));
        }

        self.db
            .update_spec_tag_details(
                &spec_id,
                store_name.as_deref(),
                inputs.description,
                inputs.workdir,
            )
            .map_err(internal_error)?;
        if let Some(intent) = &inputs.update_intent {
            self.db
                .set_graph_spec_intent(&spec_id, intent.as_deref())
                .map_err(internal_error)?;
        }
        if store_name.is_some()
            || inputs.description.is_some()
            || inputs.workdir.is_some()
            || inputs.update_intent.is_some()
            || params.spec_type.is_some()
            || params.spec_scope.is_some()
        {
            crate::daemon::handler_specs_helpers::mark_spec_dirty_for_review(&self.db, &spec_id)?;
        }

        Ok(build_spec_update_response(&spec_id))
    }

    /// SPECCODE: re-code a spec whose workdir tag is being set or changed.
    /// Skipped — exactly like today — when the spec is bound to a graph, has
    /// any run, has no code or no type, or the counter key under the new
    /// workdir is the same one its code was issued under (covers the `""`
    /// empty-workdir case). Otherwise the spec takes the next number of its
    /// type from the new workdir's counter; counters only ever increment, so
    /// the old number is never reused.
    fn apply_workdir_recoding(
        &self,
        spec: &mut GraphSpec,
        workdir: Option<&Option<&str>>,
    ) -> Result<(), McpError> {
        let Some(Some(new_workdir)) = workdir else {
            return Ok(());
        };
        if spec.graph_id.is_some() {
            return Ok(());
        }
        if !self
            .db
            .list_graph_runs_for_spec(&spec.id)
            .map_err(internal_error)?
            .is_empty()
        {
            return Ok(());
        }
        let (Some(spec_type), Some(_)) = (spec.spec_type, spec.spec_code.as_ref()) else {
            return Ok(());
        };
        let old_key = self
            .db
            .spec_code_counter_key(spec)
            .map_err(internal_error)?;
        if old_key == *new_workdir {
            return Ok(());
        }
        let code = self
            .db
            .next_spec_code(new_workdir, spec_type)
            .map_err(internal_error)?;
        self.db
            .set_graph_spec_type(&spec.id, spec_type, spec.spec_scope.as_deref(), Some(&code))
            .map_err(internal_error)?;
        spec.spec_code = Some(code);
        spec.workdir = Some(new_workdir.to_string());
        Ok(())
    }

    /// The validated `spec_edit update` writes: trimmed name/description,
    /// workdir triple-state, and the review-gated intent.
    fn parse_spec_update_inputs<'a>(
        params: &'a SpecUpdateParams,
        spec: &GraphSpec,
        review_enabled: bool,
    ) -> Result<SpecUpdateInputs<'a>, CallToolResult> {
        let name = match params.name.as_deref().map(str::trim) {
            Some("") => return Err(error_result("Spec name must not be empty.")),
            Some(value) => Some(value),
            None => None,
        };
        let description = match params.description.as_deref().map(str::trim) {
            Some("") => {
                return Err(error_result(
                    "Spec description must not be empty and must follow the template.",
                ))
            }
            Some(value) => {
                if let Err(error) = validate_spec_description_template(value) {
                    return Err(error_result(&error));
                }
                Some(value)
            }
            None => None,
        };
        let workdir = match &params.workdir {
            Some(Some(value)) => {
                let trimmed = value.trim();
                if let Err(e) = validate_spec_workdir(trimmed) {
                    return Err(error_result(&e));
                }
                Some(Some(trimmed))
            }
            Some(None) => Some(None),
            None => None,
        };
        let update_intent = match params.intent.as_deref() {
            Some(raw) => match normalize_spec_intent(Some(raw), review_enabled) {
                Ok(intent) => Some(intent),
                Err(e) => return Err(error_result(&e)),
            },
            None if review_enabled && spec.intent.is_none() => {
                return Err(error_result(INTENT_REQUIRED))
            }
            None => None,
        };
        Ok(SpecUpdateInputs {
            name,
            description,
            workdir,
            update_intent,
        })
    }

    /// SPECCODE: resolve a `spec_edit update` name through the shared
    /// conventional-title parse, validate the stored subject, and write a
    /// newly-assigned type/scope/code. Returns the subject to store (`None`
    /// when no name was given). An explicit `spec_scope` clear beats the
    /// title's scope; a type change never re-codes a spec that has a code.
    fn apply_spec_update_naming(
        db: &Database,
        spec_id: &str,
        spec: &mut GraphSpec,
        name: Option<&str>,
        explicit_type: Option<crate::domain::graphs::SpecType>,
        explicit_scope: &Option<Option<String>>,
    ) -> Result<Result<Option<String>, CallToolResult>, McpError> {
        let naming = name.map(|raw| {
            resolve_spec_naming(
                explicit_type,
                explicit_scope.clone().unwrap_or_default(),
                raw,
            )
        });
        let effective_type = naming
            .as_ref()
            .and_then(|naming| naming.spec_type)
            .or(explicit_type)
            .or(spec.spec_type);
        let effective_scope = match &explicit_scope {
            Some(scope) => scope.clone(),
            None => naming
                .as_ref()
                .and_then(|naming| naming.scope.clone())
                .or_else(|| spec.spec_scope.clone()),
        };
        let effective_name: &str = naming
            .as_ref()
            .map(|naming| naming.name.as_str())
            .or(name)
            .unwrap_or(&spec.name);
        if let Err(error) = check_subject(effective_type, effective_name) {
            return Ok(Err(error_result(&error)));
        }
        let code: Option<String> = if spec.spec_code.is_none() {
            match effective_type {
                Some(spec_type) if spec.spec_type.is_none() => {
                    let key = db.spec_code_counter_key(spec).map_err(internal_error)?;
                    Some(db.next_spec_code(&key, spec_type).map_err(internal_error)?)
                }
                _ => None,
            }
        } else {
            None
        };
        if effective_type != spec.spec_type || effective_scope != spec.spec_scope || code.is_some()
        {
            if let Some(spec_type) = effective_type {
                db.set_graph_spec_type(
                    spec_id,
                    spec_type,
                    effective_scope.as_deref(),
                    code.as_deref(),
                )
                .map_err(internal_error)?;
                spec.spec_type = Some(spec_type);
                spec.spec_scope = effective_scope;
                if let Some(code) = &code {
                    spec.spec_code = Some(code.clone());
                }
            }
        }
        Ok(Ok(naming.map(|naming| naming.name)))
    }
}
