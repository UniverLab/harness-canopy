//! MCP handler tools split out of `daemon::handler` (graph lifecycle tools and hook builders).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{
    build_graph_update_response, build_id_result, internal_error,
};
use crate::daemon::handler_specs::validate_at_least_one_bool;
use crate::daemon::handler_validate::{
    resolve_prefix_or_error, validate_absolute_dir, validate_graph_exists, validate_non_empty,
};
use crate::daemon::helpers::error_result;
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphCompletionHook, GraphHookEvent, GraphStatus};
use crate::domain::models::Trigger;
use crate::graph_engine::GRAPH_HOOK_BINDINGS;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_graphs, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "graph_create",
        description = "Create a graph container for a background graph of specs and nodes. \
        Hooks are not retroactive — a hook registered after its event has already happened does not fire."
    )]
    pub(crate) async fn graph_create(
        &self,
        Parameters(params): Parameters<GraphCreateParams>,
    ) -> Result<CallToolResult, McpError> {
        let name = params.name.trim();
        if let Err(e) = validate_non_empty(name, "Graph name") {
            return Ok(error_result(&e));
        }
        let workdir = params.workdir.trim();
        if let Err(e) = validate_non_empty(workdir, "Graph workdir") {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_absolute_dir(workdir) {
            return Ok(error_result(&e));
        }

        let trigger = match build_graph_trigger(&params.trigger) {
            Ok(trigger) => trigger,
            Err(e) => return Ok(error_result(&e)),
        };

        let lp = Graph {
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: params
                .infra_node_id
                .filter(|value| !value.trim().is_empty()),
            allow_dirty_start: params.allow_dirty_start,
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_string(),
            description: params.description.filter(|value| !value.trim().is_empty()),
            workdir: workdir.to_string(),
            status: GraphStatus::Draft,
            trigger,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        };

        self.db.insert_graph(&lp).map_err(internal_error)?;
        if crate::domain::project::should_auto_register(std::path::Path::new(workdir)) {
            if let Err(error) = self.db.register_project_path(std::path::Path::new(workdir)) {
                tracing::debug!("Could not register graph project at {workdir}: {error}");
            }
        }
        self.activate_graph_trigger(&lp).await;

        Ok(build_id_result(&lp.id, "graph_id"))
    }

    #[tool(
        name = "graph_update",
        description = "Update graph metadata such as name, description, or workdir. \
        Hooks are not retroactive — a hook registered after its event has already happened does not fire."
    )]
    pub(crate) async fn graph_update(
        &self,
        Parameters(params): Parameters<GraphUpdateParams>,
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
        if let Err(e) = validate_non_empty(&graph_id, "Graph ID") {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_graph_exists(&self.db, &graph_id) {
            return Ok(error_result(&e));
        }

        let name = match params.name.as_deref().map(str::trim) {
            Some("") => return Ok(error_result("Graph name must not be empty.")),
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
            Some("") => return Ok(error_result("Graph workdir must not be empty.")),
            Some(value) => {
                if let Err(e) = validate_absolute_dir(value) {
                    return Ok(error_result(&e));
                }
                Some(value)
            }
            None => None,
        };

        // A provided `trigger` param (even kind = "manual") counts as an update.
        let new_trigger = if params.trigger.is_some() {
            match build_graph_trigger(&params.trigger) {
                Ok(trigger) => Some(trigger),
                Err(e) => return Ok(error_result(&e)),
            }
        } else {
            None
        };

        // A provided `on_completed` param (even `null`, to clear it) counts
        // as an update — same `Option<Option<_>>` shape as `description`.
        let new_completion_hook = match &params.on_completed {
            None => None,
            Some(None) => Some(None),
            Some(Some(hook_params)) => match build_graph_completion_hook(hook_params) {
                Ok(hook) => Some(Some(hook)),
                Err(e) => return Ok(error_result(&e)),
            },
        };

        // Parse the new event-keyed `hooks` parameter. Each key must be a
        // valid event name; each value is an ordered array of hook configs.
        let new_hooks = match &params.hooks {
            None => None,
            Some(map) => {
                let mut parsed =
                    std::collections::BTreeMap::<GraphHookEvent, Vec<GraphCompletionHook>>::new();
                for (event_name, hooks) in map {
                    let Some(event) = GraphHookEvent::from_str(event_name) else {
                        return Ok(error_result(&format!(
                            "Invalid hook event '{}'. Must be one of: on_completed, on_failed, on_blocked, on_spec_completed.",
                            event_name
                        )));
                    };
                    let mut hook_list = Vec::new();
                    for hook_params in hooks {
                        match build_graph_completion_hook(hook_params) {
                            Ok(hook) => hook_list.push(hook),
                            Err(e) => return Ok(error_result(&e)),
                        }
                    }
                    parsed.insert(event, hook_list);
                }
                Some(parsed)
            }
        };

        let new_infra_node_id = match &params.infra_node_id {
            None => None,
            Some(None) => Some(None),
            Some(Some(value)) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    Some(None)
                } else {
                    Some(Some(trimmed.to_string()))
                }
            }
        };

        if let Err(e) = validate_at_least_one_bool(
            &[
                name.is_some(),
                description.is_some(),
                workdir.is_some(),
                new_trigger.is_some(),
                new_completion_hook.is_some(),
                new_hooks.is_some(),
                new_infra_node_id.is_some(),
                params.allow_dirty_start.is_some(),
            ],
            "graph_update",
        ) {
            return Ok(error_result(&e));
        }

        self.db
            .update_graph_details(&graph_id, name, description, workdir)
            .map_err(internal_error)?;

        if let Some(trigger) = new_trigger {
            self.db
                .update_graph_trigger(&graph_id, trigger.as_ref())
                .map_err(internal_error)?;
            let _ = self.watcher_engine.stop_graph_watcher(&graph_id).await;
            if let Ok(Some(lp)) = self.db.get_graph(&graph_id) {
                self.activate_graph_trigger(&lp).await;
            }
        }

        if let Some(hooks_map) = new_hooks {
            // Merge legacy `on_completed` into the hooks map if both are provided
            let mut final_hooks = hooks_map;
            if let Some(Some(hook)) = new_completion_hook {
                final_hooks
                    .entry(GraphHookEvent::OnCompleted)
                    .or_default()
                    .push(hook);
            } else if let Some(None) = new_completion_hook {
                // Explicitly clear on_completed when legacy param is null
                final_hooks.remove(&GraphHookEvent::OnCompleted);
            }
            self.db
                .update_graph_hooks(&graph_id, &final_hooks)
                .map_err(internal_error)?;
        } else if let Some(hook) = new_completion_hook {
            // Legacy path: only update on_completed, preserve other events
            self.db
                .update_graph_completion_hook(&graph_id, hook.as_ref())
                .map_err(internal_error)?;
        }

        if let Some(infra_node_id) = new_infra_node_id {
            self.db
                .update_graph_infra_node_id(&graph_id, infra_node_id.as_deref())
                .map_err(internal_error)?;
        }

        if let Some(value) = params.allow_dirty_start {
            self.db
                .update_graph_allow_dirty_start(&graph_id, value)
                .map_err(internal_error)?;
        }

        Ok(build_graph_update_response(&graph_id))
    }
}

impl TaskTriggerHandler {
    /// Reconcile a graph's live trigger wiring after create/update: wake the
    /// cron scheduler for cron graphs, and start the file watcher for watch
    /// graphs. Manual graphs need no wiring.
    pub(crate) async fn activate_graph_trigger(&self, lp: &Graph) {
        if lp.is_cron() {
            self.scheduler_notify.notify_one();
        } else if lp.is_watch() {
            if let Err(e) = self.watcher_engine.start_graph_watcher(lp).await {
                tracing::warn!("Graph '{}' saved but watcher failed to start: {}", lp.id, e);
            }
        }
    }
}

/// Build a graph [`Trigger`] from MCP parameters, reusing the same cron/watch
/// validation as agents. Returns `Ok(None)` for a manual graph (no trigger or
/// `kind = "manual"`), and an `Err(message)` for invalid input.
pub(crate) fn build_graph_trigger(
    params: &Option<GraphTriggerParams>,
) -> Result<Option<Trigger>, String> {
    let Some(params) = params else {
        return Ok(None);
    };

    match params.kind.trim().to_ascii_lowercase().as_str() {
        "" | "manual" => Ok(None),
        "cron" => {
            let schedule = params
                .schedule
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or("A cron trigger requires a 'schedule' expression.")?;
            if !crate::scheduler::validate_cron(schedule) {
                return Err(format!("Invalid cron expression: '{schedule}'."));
            }
            Ok(Some(Trigger::Cron {
                schedule_expr: schedule.to_string(),
            }))
        }
        "watch" => {
            let path = params
                .path
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or("A watch trigger requires a 'path'.")?;
            if !std::path::Path::new(path).is_absolute() {
                return Err("Watch trigger 'path' must be absolute.".into());
            }
            let event_strs = params
                .events
                .clone()
                .filter(|events| !events.is_empty())
                .ok_or("A watch trigger requires at least one event.")?;
            let events = crate::domain::models::WatchEvent::parse_list(&event_strs)?;
            Ok(Some(Trigger::Watch {
                path: path.to_string(),
                events,
                debounce_seconds: params.debounce_seconds.unwrap_or(2),
                recursive: params.recursive.unwrap_or(false),
            }))
        }
        other => Err(format!(
            "Unknown trigger kind '{other}'. Use 'cron', 'watch', or 'manual'."
        )),
    }
}

/// Build a validated [`crate::domain::graphs::GraphCompletionHook`] from MCP
/// params — an agent payload (platform + prompt), a command, an
/// interactive message (prompt + target_session_id), or a graph launch
/// (target_graph_id). Exactly one mode must be configured; any other
/// combination is rejected.
pub(crate) fn validate_graph_hook_templates(
    params: &GraphCompletionHookParams,
) -> Result<(), String> {
    let templates = [
        ("prompt", params.prompt.as_deref()),
        ("command", params.command.as_deref()),
        ("idea", params.idea.as_deref()),
    ];
    for (field, template) in templates {
        let Some(template) = template else {
            continue;
        };
        let unknown =
            crate::domain::prompts::unbindable_placeholders(template, GRAPH_HOOK_BINDINGS);
        if !unknown.is_empty() {
            return Err(format!(
                "Hook {field} contains unknown placeholder(s): {}. Supported graph hook placeholders: {}.",
                unknown.join(", "),
                GRAPH_HOOK_BINDINGS
                    .iter()
                    .map(|name| format!("{{{{{name}}}}}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    Ok(())
}

pub(crate) fn build_graph_completion_hook(
    params: &GraphCompletionHookParams,
) -> Result<crate::domain::graphs::GraphCompletionHook, String> {
    validate_graph_hook_templates(params)?;
    let modes = graph_hook_modes(params);

    if modes.target_graph {
        return build_graph_hook_target(params, &modes);
    }

    // CM27: 'target_session_id' and 'target_session_name' are alternative
    // ways to say the same thing (an interactive hook's destination) — never
    // both, and if the caller clearly means an interactive hook (they set
    // `prompt` but no other mode), never neither either. One message covers
    // both misconfigurations because "set exactly one of X or Y" is true
    // whether zero or two of them are set.
    if modes.target && modes.target_name {
        return Err(
            "Interactive hook requires exactly one of 'target_session_id' \
             or 'target_session_name', not both."
                .to_string(),
        );
    }
    if modes.prompt
        && !modes.target
        && !modes.target_name
        && params.platform.is_none()
        && params.command.is_none()
    {
        return Err(
            "Interactive hook requires exactly one of 'target_session_id' \
             or 'target_session_name'."
                .to_string(),
        );
    }

    if modes.target || modes.target_name {
        return build_interactive_graph_hook(params, &modes);
    }

    if modes.platform && modes.command {
        return Err("Hook cannot have both a 'command' and an agent payload \
             ('platform'/'prompt'). Configure exactly one."
            .to_string());
    }
    if !modes.platform && !modes.command {
        return Err("Hook must have either a 'command', an agent payload \
             ('platform' + 'prompt'), an interactive payload \
             ('target_session_id' + 'prompt'), or a graph payload \
             ('target_graph_id'). Configure exactly one."
            .to_string());
    }

    if modes.platform {
        return build_agent_graph_hook(params);
    }

    build_command_graph_hook(params)
}

/// Which hook mode the params select. Computed once from trimmed non-empty
/// presence, so every validation and builder below reads the same
/// booleans — at most one mode may be configured.
pub(crate) struct GraphHookModes {
    /// `target_graph_id` set — the graph-launch mode.
    pub(crate) target_graph: bool,
    /// `target_session_id` set — interactive mode, by id.
    pub(crate) target: bool,
    /// `target_session_name` set — interactive mode, by name.
    pub(crate) target_name: bool,
    /// `platform` set — agent payload mode.
    pub(crate) platform: bool,
    /// `command` set — shell command mode.
    pub(crate) command: bool,
    /// `prompt` set — the payload text.
    pub(crate) prompt: bool,
    /// `model` set — agent payload model override.
    pub(crate) model: bool,
    /// `effort` set — agent payload effort override.
    pub(crate) effort: bool,
}

/// Trimmed non-empty presence — the same test every hook field is judged by.
pub(crate) fn non_blank(value: Option<&str>) -> bool {
    value.is_some_and(|s| !s.trim().is_empty())
}

pub(crate) fn graph_hook_modes(params: &GraphCompletionHookParams) -> GraphHookModes {
    GraphHookModes {
        target_graph: non_blank(params.target_graph_id.as_deref()),
        target: non_blank(params.target_session_id.as_deref()),
        target_name: non_blank(params.target_session_name.as_deref()),
        platform: non_blank(params.platform.as_deref()),
        command: non_blank(params.command.as_deref()),
        prompt: non_blank(params.prompt.as_deref()),
        model: non_blank(params.model.as_deref()),
        effort: non_blank(params.effort.as_deref()),
    }
}

/// The graph-launch hook: `target_graph_id`, optionally paired with exactly
/// one of `queue_id`/`idea`. Exclusivity against every other mode's fields
/// is checked before anything else, exactly as the planner has always done.
pub(crate) fn build_graph_hook_target(
    params: &GraphCompletionHookParams,
    modes: &GraphHookModes,
) -> Result<crate::domain::graphs::GraphCompletionHook, String> {
    if modes.platform
        || modes.command
        || modes.target
        || modes.target_name
        || modes.model
        || modes.effort
    {
        return Err(
            "Graph hook ('target_graph_id') must not set 'platform', 'command', \
             'target_session_id', 'target_session_name', 'model', or 'effort'. Configure exactly one hook mode."
                .to_string(),
        );
    }
    let queue_id = params
        .queue_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let idea = params
        .idea
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if queue_id.is_some() && idea.is_some() {
        return Err("Graph hook: 'queue_id' and 'idea' are mutually exclusive.".to_string());
    }
    let target_graph_id = params
        .target_graph_id
        .as_deref()
        .unwrap()
        .trim()
        .to_string();

    Ok(crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: Some(target_graph_id),
        queue_id,
        workdir_override: params
            .workdir_override
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        idea,
    })
}

/// The interactive hook: a prompt plus exactly one of `target_session_id`/
/// `target_session_name`, and no agent/command fields (the caller has
/// already rejected both-or-neither targets and a targetless prompt).
pub(crate) fn build_interactive_graph_hook(
    params: &GraphCompletionHookParams,
    modes: &GraphHookModes,
) -> Result<crate::domain::graphs::GraphCompletionHook, String> {
    if modes.platform || modes.command || modes.model || modes.effort {
        return Err(
            "Interactive hook ('target_session_id'/'target_session_name' + 'prompt') must not \
             set 'platform', 'model', 'effort', or 'command'. Configure exactly one hook mode."
                .to_string(),
        );
    }
    if !modes.prompt {
        return Err("Interactive hook 'prompt' must not be empty.".to_string());
    }
    let (target_session_id, target_session_name) = if modes.target {
        (
            Some(
                params
                    .target_session_id
                    .as_deref()
                    .unwrap()
                    .trim()
                    .to_string(),
            ),
            None,
        )
    } else {
        (
            None,
            Some(
                params
                    .target_session_name
                    .as_deref()
                    .unwrap()
                    .trim()
                    .to_string(),
            ),
        )
    };
    let prompt = params.prompt.as_deref().unwrap().trim().to_string();

    Ok(crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: Some(prompt),
        command: None,
        target_session_id,
        timeout_minutes: params.timeout_minutes,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
        target_session_name,
    })
}

/// The agent hook: platform + prompt (+ optional model/effort). A prompt
/// that is empty once trimmed is rejected, not silently dropped.
pub(crate) fn build_agent_graph_hook(
    params: &GraphCompletionHookParams,
) -> Result<crate::domain::graphs::GraphCompletionHook, String> {
    let platform = params.platform.as_deref().unwrap().trim().to_string();
    let prompt = params
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "Agent hook 'prompt' must not be empty.".to_string())?
        .to_string();
    let model = params
        .model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let effort = params
        .effort
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    Ok(crate::domain::graphs::GraphCompletionHook {
        platform: Some(platform),
        model,
        effort,
        prompt: Some(prompt),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: params.timeout_minutes,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
    })
}

/// The command hook — the last mode standing once every other one has been
/// rejected: a non-blank command, nothing else set.
pub(crate) fn build_command_graph_hook(
    params: &GraphCompletionHookParams,
) -> Result<crate::domain::graphs::GraphCompletionHook, String> {
    let command = params.command.as_deref().unwrap().trim().to_string();
    Ok(crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(command),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: params.timeout_minutes,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
    })
}
