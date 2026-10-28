//! The eight second-wave resource tools: one `#[tool]` method per resource,
//! each dispatching on `action` through the shared [`ToolSpec`] contract in
//! [`crate::daemon::actions`]. Every action validates first (one refusal
//! listing the whole action table), then deserializes the same JSON — minus
//! the dispatch key — into the existing per-operation params struct and
//! calls the `<tool>_<action>` `_impl` method. No behaviour lives here; the
//! logic is the old tools' own.
//!
//! `agent_report` stays a standalone tool (the only way to report a run) and
//! `session_list` was never in a moved family, so it keeps its own `#[tool]`
//! in its family module.

use crate::daemon::actions::{action_params, ActionObject, ActionSpec, ToolSpec};
use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::params_extract::Parameters;
use axum::http::request::Parts;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

// ── Action tables ────────────────────────────────────────────────────

static SPEC: ToolSpec = ToolSpec {
    name: "spec",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &[
                "workdir",
                "status",
                "unassigned_only",
                "spec_type",
                "include_descriptions",
                "limit",
                "offset",
            ],
        },
        ActionSpec {
            name: "get",
            required: &["spec_id"],
            optional: &[],
        },
        ActionSpec {
            name: "section",
            required: &["spec_id", "section"],
            optional: &[],
        },
        ActionSpec {
            name: "notes",
            required: &["spec_id"],
            optional: &[],
        },
    ],
};

static SPEC_EDIT: ToolSpec = ToolSpec {
    name: "spec_edit",
    actions: &[
        ActionSpec {
            name: "create",
            required: &["name", "description"],
            optional: &["workdir", "spec_type", "spec_scope"],
        },
        ActionSpec {
            name: "update",
            required: &["spec_id"],
            optional: &["name", "description", "workdir", "spec_type", "spec_scope"],
        },
        ActionSpec {
            name: "delete",
            required: &["spec_id"],
            optional: &[],
        },
        ActionSpec {
            name: "set_status",
            required: &["spec_id", "status", "reason"],
            optional: &[],
        },
        ActionSpec {
            name: "note_write",
            required: &["body"],
            optional: &["run_id", "spec_id"],
        },
    ],
};

static QUEUE: ToolSpec = ToolSpec {
    name: "queue",
    actions: &[
        ActionSpec {
            name: "create",
            required: &["name"],
            optional: &[],
        },
        ActionSpec {
            name: "list",
            required: &[],
            optional: &["queue_id", "include_descriptions"],
        },
        ActionSpec {
            name: "add_spec",
            required: &["queue_id", "spec_id"],
            optional: &["group"],
        },
        ActionSpec {
            name: "remove_spec",
            required: &["queue_id", "spec_id"],
            optional: &[],
        },
        ActionSpec {
            name: "reorder",
            required: &["queue_id", "spec_ids"],
            optional: &[],
        },
        ActionSpec {
            name: "set_group",
            required: &["queue_id", "spec_id"],
            optional: &["group"],
        },
    ],
};

static AGENT: ToolSpec = ToolSpec {
    name: "agent",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "get",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "logs",
            required: &["id"],
            optional: &["lines", "since"],
        },
        ActionSpec {
            name: "status",
            required: &[],
            optional: &[],
        },
    ],
};

static AGENT_EDIT: ToolSpec = ToolSpec {
    name: "agent_edit",
    actions: &[
        ActionSpec {
            name: "add",
            required: &["id", "prompt", "schedule"],
            optional: &[
                "cli",
                "model",
                "effort",
                "duration_minutes",
                "working_dir",
                "timeout_minutes",
                "delivery_action",
                "target_session_name",
                "target_session_id",
            ],
        },
        ActionSpec {
            name: "watch",
            required: &["id", "path", "events", "prompt"],
            optional: &[
                "cli",
                "model",
                "effort",
                "debounce_seconds",
                "recursive",
                "timeout_minutes",
                "delivery_action",
                "target_session_name",
                "target_session_id",
            ],
        },
        ActionSpec {
            name: "update",
            required: &["id"],
            optional: &[
                "new_id",
                "prompt",
                "cli",
                "model",
                "effort",
                "schedule",
                "working_dir",
                "duration_minutes",
                "path",
                "events",
                "debounce_seconds",
                "recursive",
                "enabled",
                "notify_on_success",
                "delivery_action",
                "target_session_name",
                "target_session_id",
            ],
        },
        ActionSpec {
            name: "remove",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "enable",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "disable",
            required: &["id"],
            optional: &[],
        },
        ActionSpec {
            name: "schedule",
            required: &["id", "at"],
            optional: &[],
        },
        ActionSpec {
            name: "run",
            required: &["id"],
            optional: &[],
        },
    ],
};

static HARNESS: ToolSpec = ToolSpec {
    name: "harness",
    actions: &[
        ActionSpec {
            name: "models",
            required: &[],
            optional: &["platform", "refresh", "full"],
        },
        ActionSpec {
            name: "probe",
            required: &[],
            optional: &["platform", "model", "timeout_seconds"],
        },
        ActionSpec {
            name: "probe_recent",
            required: &[],
            optional: &["timeout_seconds", "max_pairs"],
        },
        ActionSpec {
            name: "refresh_registry",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "roster_list",
            required: &[],
            optional: &["platform"],
        },
        ActionSpec {
            name: "roster_set",
            required: &["platform", "state"],
            optional: &["model_pattern", "reason", "until"],
        },
        ActionSpec {
            name: "roster_clear",
            required: &["platform"],
            optional: &["model_pattern"],
        },
        ActionSpec {
            name: "roster_check",
            required: &["platform"],
            optional: &["model"],
        },
    ],
};

static CONFIG: ToolSpec = ToolSpec {
    name: "config",
    actions: &[
        ActionSpec {
            name: "get",
            required: &["key"],
            optional: &[],
        },
        ActionSpec {
            name: "list",
            required: &[],
            optional: &["group"],
        },
        ActionSpec {
            name: "set",
            required: &["key", "value"],
            optional: &[],
        },
        ActionSpec {
            name: "reset",
            required: &["key"],
            optional: &[],
        },
    ],
};

static CREW: ToolSpec = ToolSpec {
    name: "crew",
    actions: &[
        ActionSpec {
            name: "list",
            required: &[],
            optional: &[],
        },
        ActionSpec {
            name: "get",
            required: &["name"],
            optional: &[],
        },
        ActionSpec {
            name: "create",
            required: &["name"],
            optional: &["description", "members", "from_ensemble"],
        },
        ActionSpec {
            name: "update",
            required: &["name"],
            optional: &["members", "description", "rename"],
        },
        ActionSpec {
            name: "delete",
            required: &["name"],
            optional: &[],
        },
        ActionSpec {
            name: "assignments",
            required: &[],
            optional: &[
                "graph_id",
                "platform",
                "model",
                "include_archived",
                "group_by",
            ],
        },
    ],
};

// ── The eight resource tools ─────────────────────────────────────────

#[tool_router(router = tool_router_resource_tools, vis = "pub(crate)")]
impl TaskTriggerHandler {
    #[tool(
        name = "spec",
        description = "Read specs, one spec section, and spec notes. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: workdir, status, unassigned_only, spec_type, include_descriptions, limit, offset) — list standalone/backlog specs with limit/offset pagination. \
        `get` (required: spec_id; optional: none) — return one spec, the row `list` would return with its full description. \
        `section` (required: spec_id, section; optional: none) — extract one canonical section from a spec body. \
        `notes` (required: spec_id; optional: none) — read every note of a spec, newest first."
    )]
    pub(crate) async fn spec(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = SPEC.validate(&params)?;
        match action {
            "list" => self.spec_list_impl(action_params(&params)?).await,
            "get" => self.spec_get_impl(action_params(&params)?).await,
            "section" => self.spec_section_impl(action_params(&params)?).await,
            "notes" => self.spec_notes_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "spec_edit",
        description = "Create, change, delete, and administratively transition specs, and write spec notes. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `create` (required: name, description; optional: workdir, spec_type, spec_scope) — create a standalone spec. \
        `update` (required: spec_id; optional: name, description, workdir, spec_type, spec_scope) — update a spec's name, description, and/or workdir tag. \
        `delete` (required: spec_id; optional: none) — delete a standalone spec; refused while it is bound to a graph. \
        `set_status` (required: spec_id, status, reason; optional: none) — administratively complete, skip, or reopen a standalone spec. \
        `note_write` (required: body; optional: run_id, spec_id) — write or replace the caller's note; pass exactly one of run_id (your note) or spec_id (the operator note)."
    )]
    pub(crate) async fn spec_edit(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = SPEC_EDIT.validate(&params)?;
        match action {
            "create" => self.spec_edit_create_impl(action_params(&params)?).await,
            "update" => self.spec_edit_update_impl(action_params(&params)?).await,
            "delete" => self.spec_edit_delete_impl(action_params(&params)?).await,
            "set_status" => {
                self.spec_edit_set_status_impl(action_params(&params)?)
                    .await
            }
            "note_write" => {
                self.spec_edit_note_write_impl(action_params(&params)?)
                    .await
            }
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "queue",
        description = "Author ordered spec queues. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `create` (required: name; optional: none) — create a queue. \
        `list` (required: none; optional: queue_id, include_descriptions) — list a queue's ordered members, or every queue (summary only) when queue_id is omitted. \
        `add_spec` (required: queue_id, spec_id; optional: group) — add a spec; a grouped add lands right after the group's last pending member. \
        `remove_spec` (required: queue_id, spec_id; optional: none) — remove a spec from a queue. \
        `reorder` (required: queue_id, spec_ids; optional: none) — replace the whole order; spec_ids must list every member exactly once. \
        `set_group` (required: queue_id, spec_id; optional: group) — set or clear a member's context group; omit or pass null to clear."
    )]
    pub(crate) async fn queue(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = QUEUE.validate(&params)?;
        match action {
            "create" => self.queue_create_impl(action_params(&params)?).await,
            "list" => self.queue_list_impl(action_params(&params)?).await,
            "add_spec" => self.queue_add_spec_impl(action_params(&params)?).await,
            "remove_spec" => self.queue_remove_spec_impl(action_params(&params)?).await,
            "reorder" => self.queue_reorder_impl(action_params(&params)?).await,
            "set_group" => self.queue_set_group_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "agent",
        description = "Read background agents, their logs, and daemon health. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: none) — list all registered agents with their current status. \
        `get` (required: id; optional: none) — one agent's complete stored definition as JSON, including the full, untruncated prompt. Read-only. \
        `logs` (required: id; optional: lines, since) — log output for an agent with optional line and time filters. \
        `status` (required: none; optional: none) — overall daemon health, scheduler state, and statistics."
    )]
    pub(crate) async fn agent(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = AGENT.validate(&params)?;
        match action {
            "list" => self.agent_list_impl().await,
            "get" => self.agent_get_impl(action_params(&params)?).await,
            "logs" => self.agent_logs_impl(action_params(&params)?).await,
            "status" => self.agent_status_impl().await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "agent_edit",
        description = "Create, change, remove, enable, disable, schedule, and run background agents. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `add` (required: id, prompt, schedule; optional: cli, model, effort, duration_minutes, working_dir, timeout_minutes, delivery_action, target_session_name, target_session_id) — create or update a cron-scheduled agent (schedule is a 5-field cron expression). \
        `watch` (required: id, path, events, prompt; optional: cli, model, effort, debounce_seconds, recursive, timeout_minutes, delivery_action, target_session_name, target_session_id) — register a file or directory watcher. \
        `update` (required: id; optional: new_id, prompt, cli, model, effort, schedule, working_dir, duration_minutes, path, events, debounce_seconds, recursive, enabled, notify_on_success, delivery_action, target_session_name, target_session_id) — modify an agent; only the provided fields are updated. \
        `remove` (required: id; optional: none) — remove an agent completely. \
        `enable` (required: id; optional: none) — enable a disabled agent. \
        `disable` (required: id; optional: none) — disable an agent without removing it. \
        `schedule` (required: id, at; optional: none) — schedule a one-shot enable at a future ISO 8601 time. \
        `run` (required: id; optional: none) — execute an agent immediately outside its schedule. \
        Set delivery_action to send_to_session with target_session_name (recommended) or target_session_id to deliver the prompt into a live interactive session instead of running a CLI."
    )]
    pub(crate) async fn agent_edit(
        &self,
        Parameters(params): Parameters<ActionObject>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = AGENT_EDIT.validate(&params)?;
        match action {
            "add" => {
                self.agent_edit_add_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "watch" => {
                self.agent_edit_watch_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "update" => {
                self.agent_edit_update_impl(action_params(&params)?, parts.as_ref())
                    .await
            }
            "remove" => self.agent_edit_remove_impl(action_params(&params)?).await,
            "enable" => self.agent_edit_enable_impl(action_params(&params)?).await,
            "disable" => self.agent_edit_disable_impl(action_params(&params)?).await,
            "schedule" => self.agent_edit_schedule_impl(action_params(&params)?).await,
            "run" => self.agent_edit_run_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "harness",
        description = "Inspect model catalogs, probe platform liveness, refresh the platform registry, and manage the availability roster. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `models` (required: none; optional: platform, refresh, full) — list AI models available for use with agents. \
        `probe` (required: none; optional: platform, model, timeout_seconds) — invoke a platform headlessly and report whether a real, usable response comes back. Spends real tokens/quota — call explicitly, never automatically or on a schedule. \
        `probe_recent` (required: none; optional: timeout_seconds, max_pairs) — sweep every distinct recently-used platform+model pair exactly once. Spends real tokens/quota — strictly manual. \
        `refresh_registry` (required: none; optional: none) — fetch the platform registry and apply it to config.toml now. \
        `roster_list` (required: none; optional: platform) — list every roster entry, each with its effective state now. \
        `roster_set` (required: platform, state; optional: model_pattern, reason, until) — declare one platform/model pair available or unavailable. \
        `roster_clear` (required: platform; optional: model_pattern) — delete one roster entry (idempotent). \
        `roster_check` (required: platform; optional: model) — the effective availability of one pair, naming the entry that decided it."
    )]
    pub(crate) async fn harness(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = HARNESS.validate(&params)?;
        match action {
            "models" => self.harness_models_impl(action_params(&params)?).await,
            "probe" => self.harness_probe_impl(action_params(&params)?).await,
            "probe_recent" => {
                self.harness_probe_recent_impl(action_params(&params)?)
                    .await
            }
            "refresh_registry" => self.harness_refresh_registry_impl().await,
            "roster_list" => self.harness_roster_list_impl(action_params(&params)?).await,
            "roster_set" => self.harness_roster_set_impl(action_params(&params)?).await,
            "roster_clear" => {
                self.harness_roster_clear_impl(action_params(&params)?)
                    .await
            }
            "roster_check" => {
                self.harness_roster_check_impl(action_params(&params)?)
                    .await
            }
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "config",
        description = "Read and change canopy settings. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `get` (required: key; optional: none) — read one setting by its dotted key. \
        `list` (required: none; optional: group) — list every setting with its group, kind, default, effective value, source, apply mode and status. \
        `set` (required: key, value; optional: none) — change one setting; validated against its type and range first, and nothing is written if it is refused. \
        `reset` (required: key; optional: none) — reset one setting to its registry default."
    )]
    pub(crate) async fn config(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = CONFIG.validate(&params)?;
        match action {
            "get" => self.config_get_impl(action_params(&params)?).await,
            "list" => self.config_list_impl(action_params(&params)?).await,
            "set" => self.config_set_impl(action_params(&params)?).await,
            "reset" => self.config_reset_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }

    #[tool(
        name = "crew",
        description = "Manage crews and read every graph's harness/model assignments. Pass `action` plus the parameters listed for it. \
        Actions (required params; optional params): \
        `list` (required: none; optional: none) — list every crew with its version, ordered members and binding counts. \
        `get` (required: name; optional: none) — show one crew and every binding of it. \
        `create` (required: name; optional: description, members, from_ensemble) — create a crew from an ordered member list or by seeding from an ensemble; pass exactly one of members or from_ensemble. \
        `update` (required: name; optional: members, description, rename) — replace the member list, set the description, and/or rename. \
        `delete` (required: name; optional: none) — delete a crew; refused while anything still binds it. \
        `assignments` (required: none; optional: graph_id, platform, model, include_archived, group_by) — every graph's harness/model assignments, without prompt text; group_by \"pair\" summarises per platform/model pair."
    )]
    pub(crate) async fn crew(
        &self,
        Parameters(params): Parameters<ActionObject>,
    ) -> Result<CallToolResult, McpError> {
        let params = params.into_value();
        let action = CREW.validate(&params)?;
        match action {
            "list" => self.crew_list_impl().await,
            "get" => self.crew_get_impl(action_params(&params)?).await,
            "create" => self.crew_create_impl(action_params(&params)?).await,
            "update" => self.crew_update_impl(action_params(&params)?).await,
            "delete" => self.crew_delete_impl(action_params(&params)?).await,
            "assignments" => self.crew_assignments_impl(action_params(&params)?).await,
            _ => unreachable!("validated action"),
        }
    }
}
