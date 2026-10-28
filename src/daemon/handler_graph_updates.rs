//! Section helpers for `graph_update_impl`, split out so the orchestrator
//! and its home module stay within the size gate. Behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_graphs::{build_graph_completion_hook, build_graph_trigger};
use crate::daemon::handler_helpers::record_config_change_if_running;
use crate::daemon::handler_validate::validate_absolute_dir;
use crate::daemon::helpers::error_result;
use crate::daemon::params::{GraphCompletionHookParams, GraphUpdateParams};
use crate::db::Database;
use crate::domain::graphs::{GraphCompletionHook, GraphHookEvent};
use crate::domain::models::Trigger;
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;

/// A validated `graph_edit` update: owned values plus params borrowed
/// for the string fields.
pub(crate) struct ParsedGraphUpdate<'a> {
    pub name: Option<&'a str>,
    pub description: Option<Option<&'a str>>,
    pub workdir: Option<&'a str>,
    pub trigger: Option<Option<Trigger>>,
    pub completion_hook: Option<Option<GraphCompletionHook>>,
    pub hooks: Option<std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>>,
    pub infra_node_id: Option<Option<String>>,
}

pub(crate) fn parse_graph_update_fields(
    params: &GraphUpdateParams,
) -> Result<ParsedGraphUpdate<'_>, CallToolResult> {
    let name = match params.name.as_deref().map(str::trim) {
        Some("") => return Err(error_result("Graph name must not be empty.")),
        Some(value) => Some(value),
        None => None,
    };
    let description = params.description.as_ref().map(|value| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
    });
    let workdir = match params.workdir.as_deref().map(str::trim) {
        Some("") => return Err(error_result("Graph workdir must not be empty.")),
        Some(value) => {
            if let Err(e) = validate_absolute_dir(value) {
                return Err(error_result(&e));
            }
            Some(value)
        }
        None => None,
    };
    // A provided `trigger` param (even kind = "manual") counts as an update.
    let trigger = if params.trigger.is_some() {
        match build_graph_trigger(&params.trigger) {
            Ok(trigger) => Some(trigger),
            Err(e) => return Err(error_result(&e)),
        }
    } else {
        None
    };
    // A provided `on_completed` param (even `null`, to clear it) counts
    // as an update — same `Option<Option<_>>` shape as `description`.
    let completion_hook = match &params.on_completed {
        None => None,
        Some(None) => Some(None),
        Some(Some(hook_params)) => match build_graph_completion_hook(hook_params) {
            Ok(hook) => Some(Some(hook)),
            Err(e) => return Err(error_result(&e)),
        },
    };
    Ok(ParsedGraphUpdate {
        name,
        description,
        workdir,
        trigger,
        completion_hook,
        hooks: None,
        infra_node_id: None,
    })
}

/// Parse the new event-keyed `hooks` parameter. Each key must be a
/// valid event name; each value is an ordered array of hook configs.
pub(crate) fn parse_graph_hooks_param(
    hooks: &Option<std::collections::BTreeMap<String, Vec<GraphCompletionHookParams>>>,
) -> Result<
    Option<std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>>,
    CallToolResult,
> {
    match hooks {
        None => Ok(None),
        Some(map) => {
            let mut parsed =
                std::collections::BTreeMap::<GraphHookEvent, Vec<GraphCompletionHook>>::new();
            for (event_name, hooks) in map {
                let Some(event) = GraphHookEvent::from_str(event_name) else {
                    return Err(error_result(&format!(
                        "Invalid hook event '{event_name}'. Must be one of: on_completed, on_failed, on_blocked, on_spec_completed."
                    )));
                };
                let mut hook_list = Vec::new();
                for hook_params in hooks {
                    match build_graph_completion_hook(hook_params) {
                        Ok(hook) => hook_list.push(hook),
                        Err(e) => return Err(error_result(&e)),
                    }
                }
                parsed.insert(event, hook_list);
            }
            Ok(Some(parsed))
        }
    }
}

/// CM46: hooks / infra wiring / allow_dirty_start edits on a RUNNING
/// graph are narratable config changes. A pure name/description edit is
/// not.
pub(crate) fn record_graph_update_narration(
    db: &Database,
    graph_id: &str,
    hooks_changed: bool,
    infra_node_changed: bool,
    allow_dirty_start: Option<bool>,
) {
    if hooks_changed || infra_node_changed || allow_dirty_start.is_some() {
        let graph_name = db
            .get_graph(graph_id)
            .ok()
            .flatten()
            .map(|graph| graph.name)
            .unwrap_or_else(|| graph_id.to_string());
        let change = if hooks_changed { "hook" } else { "metadata" };
        record_config_change_if_running(
            db,
            Some(graph_id),
            None,
            None,
            change,
            format!("Graph '{graph_name}' config updated."),
        );
    }
}

impl TaskTriggerHandler {
    /// Write the parsed update to the store and reconcile live trigger
    /// wiring. Returns `(hooks_changed, infra_node_changed)`.
    pub(crate) async fn persist_graph_update(
        &self,
        graph_id: &str,
        update: ParsedGraphUpdate<'_>,
        allow_dirty_start: Option<bool>,
        require_spec_type: Option<bool>,
    ) -> Result<(bool, bool), McpError> {
        self.db
            .update_graph_details(graph_id, update.name, update.description, update.workdir)
            .map_err(internal_error)?;
        if let Some(trigger) = update.trigger {
            self.db
                .update_graph_trigger(graph_id, trigger.as_ref())
                .map_err(internal_error)?;
            let _ = self.watcher_engine.stop_graph_watcher(graph_id).await;
            if let Ok(Some(lp)) = self.db.get_graph(graph_id) {
                self.activate_graph_trigger(&lp).await;
            }
        }
        let hooks_changed = update.hooks.is_some() || update.completion_hook.is_some();
        if let Some(hooks_map) = update.hooks {
            // Merge legacy `on_completed` into the hooks map if both are provided
            let mut final_hooks = hooks_map;
            if let Some(Some(hook)) = update.completion_hook {
                final_hooks
                    .entry(GraphHookEvent::OnCompleted)
                    .or_default()
                    .push(hook);
            } else if let Some(None) = update.completion_hook {
                // Explicitly clear on_completed when legacy param is null
                final_hooks.remove(&GraphHookEvent::OnCompleted);
            }
            self.db
                .update_graph_hooks(graph_id, &final_hooks)
                .map_err(internal_error)?;
        } else if let Some(hook) = update.completion_hook {
            // Legacy path: only update on_completed, preserve other events
            self.db
                .update_graph_completion_hook(graph_id, hook.as_ref())
                .map_err(internal_error)?;
        }
        let infra_node_changed = update.infra_node_id.is_some();
        if let Some(infra_node_id) = update.infra_node_id {
            self.db
                .update_graph_infra_node_id(graph_id, infra_node_id.as_deref())
                .map_err(internal_error)?;
        }
        if let Some(value) = allow_dirty_start {
            self.db
                .update_graph_allow_dirty_start(graph_id, value)
                .map_err(internal_error)?;
        }
        if let Some(value) = require_spec_type {
            self.db
                .update_graph_require_spec_type(graph_id, value)
                .map_err(internal_error)?;
        }
        Ok((hooks_changed, infra_node_changed))
    }
}
