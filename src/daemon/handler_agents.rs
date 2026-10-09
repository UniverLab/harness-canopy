//! MCP handler tools split out of `daemon::handler` (agent/task/scheduled-send tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::application::ports::{AgentRepository, RunRepository};
use crate::daemon::handler::{TaskTriggerHandler, SESSION_LIST_DEFAULT_LIMIT};
use crate::daemon::handler_formatting::{
    agent_detail_json, append_temporal_agents_section, build_json_result, format_agent_info,
    format_log_output, format_uptime, internal_error, make_log_path, recent_runs_output,
    resolve_log_path,
};
use crate::daemon::handler_helpers::{
    apply_scalar_updates, apply_trigger_updates, handle_timed_out_run, new_agent_base,
    parse_report_status, prepare_cron_task, prepare_watch_task, update_agent_last_run,
    validate_report_summary, validate_run_transition, watcher_restart_needed,
};
use crate::daemon::helpers::{data_dir, error_result, notify_run_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::session::InteractiveSession;
use crate::domain::models::Agent;
use crate::domain::models::Trigger;
use crate::domain::validation::validate_id;
use rmcp::model::{CallToolResult, Content};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_agents, vis = "pub(crate)")]
impl TaskTriggerHandler {
    /// Create or update an agent. Supports cron triggers (schedule), watch triggers
    /// (path + events), or manual-only agents (no trigger). When updating an existing
    /// agent, only the fields you provide are changed.
    #[tool(
        name = "agent_add",
        description = "Create or update a scheduled background_agent. \
         Use agent_models to see available model options. \
         The schedule field must be a standard 5-field cron expression. \
         Common patterns: '*/5 * * * *' (every 5 min), '0 9 * * *' (daily 9am), \
         '0 9 * * 1-5' (weekdays 9am), '0 */2 * * *' (every 2 hours), \
         '30 14 1,15 * *' (1st and 15th at 2:30pm). \
         Fields: minute(0-59) hour(0-23) day(1-31) month(1-12) weekday(0-6, 0=Sun). \
         Use duration_minutes for temporary agents that auto-expire. \
         The cli parameter is optional — if omitted, auto-detects from registry. \
         The model parameter is optional — if omitted, the CLI uses its configured default."
    )]
    pub(crate) async fn task_add(
        &self,
        Parameters(params): Parameters<TaskAddParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::scheduler::validate_cron;

        let prepared = match prepare_cron_task(&params, &validate_cron) {
            Ok(prepared) => prepared,
            Err(e) => return Ok(error_result(&e)),
        };

        if let Err(error) =
            crate::domain::cli_strategy::ensure_cli_installed(prepared.cli.as_str()).await
        {
            return Ok(error_result(&error.message()));
        }

        let log_path = make_log_path(&params.id)?;
        let mut agent = new_agent_base(
            params.id.clone(),
            params.prompt,
            prepared.cli,
            params.model,
            params.working_dir,
            params.timeout_minutes,
            log_path,
        );
        agent.effort = params.effort;
        agent.trigger = Some(Trigger::Cron {
            schedule_expr: prepared.schedule_expr.clone(),
        });
        agent.expires_at = prepared.expires_at;

        self.db.upsert_agent(&agent).map_err(internal_error)?;
        if let Some(workdir) = agent.working_dir.as_deref() {
            if crate::domain::project::should_auto_register(std::path::Path::new(workdir)) {
                if let Err(e) = self.db.register_project_path(std::path::Path::new(workdir)) {
                    tracing::debug!("Could not register project at {workdir}: {e}");
                }
            }
        }
        self.scheduler_notify.notify_one();

        Ok(success_result(&format!(
            "Agent '{}' registered with schedule '{}'{}\nThe daemon's internal scheduler will execute this agent automatically.",
            agent.id,
            prepared.schedule_expr,
            agent
                .expires_at
                .map(|expires_at| format!(" (expires: {})", expires_at.to_rfc3339()))
                .unwrap_or_default()
        )))
    }

    /// Register a file or directory watcher.
    #[tool(
        name = "agent_watch",
        description = "Watch a file or directory for changes and execute a prompt when events occur. \
         The cli parameter is optional — if omitted, auto-detects from registry. \
         The model parameter is optional — if omitted, the CLI uses its configured default model."
    )]
    pub(crate) async fn task_watch(
        &self,
        Parameters(params): Parameters<TaskWatchParams>,
    ) -> Result<CallToolResult, McpError> {
        let prepared = match prepare_watch_task(&params) {
            Ok(prepared) => prepared,
            Err(e) => return Ok(error_result(&e)),
        };

        if let Err(error) =
            crate::domain::cli_strategy::ensure_cli_installed(prepared.cli.as_str()).await
        {
            return Ok(error_result(&error.message()));
        }

        let log_path = make_log_path(&params.id)?;
        let mut agent = new_agent_base(
            params.id.clone(),
            params.prompt,
            prepared.cli,
            params.model,
            None,
            params.timeout_minutes,
            log_path,
        );
        agent.effort = params.effort;
        agent.trigger = Some(Trigger::Watch {
            path: params.path.clone(),
            events: prepared.events,
            debounce_seconds: prepared.debounce_seconds,
            recursive: prepared.recursive,
        });

        self.db.upsert_agent(&agent).map_err(internal_error)?;

        if let Err(e) = self.watcher_engine.start_watcher(&agent).await {
            tracing::warn!("Watcher '{}' saved but failed to start: {}", agent.id, e);
            return Ok(CallToolResult::success(vec![Content::text(format!(
                "Watcher '{}' registered but could not start watching '{}': {}. It will be retried on daemon restart.",
                agent.id, params.path, e
            ))]));
        }

        Ok(success_result(&format!(
            "Watcher '{}' active on '{}' for events: {:?}",
            agent.id, params.path, params.events
        )))
    }

    /// List all registered agents with status.
    #[tool(
        name = "agent_list",
        description = "List all registered scheduled agents with their current status"
    )]
    pub(crate) async fn task_list(&self) -> Result<CallToolResult, McpError> {
        let agents = self
            .db
            .list_agents()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let corrupt = self
            .db
            .list_corrupt_agents()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if agents.is_empty() && corrupt.is_empty() {
            return Ok(success_result("No agents registered."));
        }

        let mut lines = vec![format!(
            "Found {} agent(s){}:\n",
            agents.len(),
            if corrupt.is_empty() {
                String::new()
            } else {
                format!(" ({} corrupt)", corrupt.len())
            }
        )];

        for a in &agents {
            let mut info = format_agent_info(a);

            if a.is_watch() {
                let runtime_active = self.watcher_engine.is_active(&a.id).await;
                let watch_status = if !a.enabled {
                    "paused"
                } else if runtime_active {
                    "active"
                } else {
                    "registered (not running)"
                };
                info.push_str(&format!("  Watch status: {}\n", watch_status));
            }

            lines.push(info);
        }

        for c in &corrupt {
            lines.push(format!(
                "- **{}** [corrupt config] — trigger_config failed to parse: {}\n",
                c.id, c.error
            ));
        }

        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Get one agent's full stored definition.
    #[tool(
        name = "agent_get",
        description = "Get one agent's complete stored definition as JSON — every field including the full, untruncated prompt. agent_list truncates the prompt to ~80 characters; call this with an agent's id when you need the exact prompt text (e.g. to fix a stale command reference before agent_update) or any other stored field: trigger_type, trigger_config (parsed), cli, model, effort, working_dir, enabled, timeout_minutes, expires_at, enable_at, notify_on_success, log_path, created_at, last_run_at, last_run_ok, last_triggered_at, trigger_count. Read-only."
    )]
    pub(crate) async fn agent_get(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> Result<CallToolResult, McpError> {
        let Some(agent) = self.db.get_agent(&id).map_err(internal_error)? else {
            return Ok(error_result(&format!("agent '{}' not found", id)));
        };
        let notify = self
            .db
            .agent_notify_on_success(&id)
            .map_err(internal_error)?;
        Ok(build_json_result(&agent_detail_json(&agent, notify)))
    }

    /// List interactive sessions that an interactive hook can address.
    #[tool(
        name = "session_list",
        description = "List live interactive sessions with the exact ids an interactive graph hook's target_session_id accepts. Pass session_id to look one up; read-only, no session is woken."
    )]
    pub(crate) async fn session_list(
        &self,
        Parameters(params): Parameters<SessionListParams>,
    ) -> Result<CallToolResult, McpError> {
        let limit = params
            .limit
            .unwrap_or(SESSION_LIST_DEFAULT_LIMIT)
            .clamp(1, 200) as i64;

        let session_id = params.session_id.as_deref().map(str::trim);
        if let Some(session_id) = session_id {
            if session_id.is_empty() {
                return Ok(error_result("No session found with ID ''"));
            }

            let session = self
                .db
                .get_hookable_session(session_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            let Some(session) = session else {
                return Ok(error_result(&format!(
                    "No session found with ID '{session_id}'"
                )));
            };

            return Ok(success_result(&Self::format_session_info(&session)));
        }

        let sessions = self
            .db
            .list_hookable_sessions(limit)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        if sessions.is_empty() {
            return Ok(success_result("No live interactive sessions found."));
        }

        let mut lines = vec![format!(
            "Found {} live interactive session(s):\n",
            sessions.len()
        )];
        lines.extend(sessions.iter().map(Self::format_session_info));
        Ok(CallToolResult::success(vec![Content::text(
            lines.join("\n"),
        )]))
    }

    /// Remove an agent completely.
    #[tool(
        name = "agent_remove",
        description = "Remove an agent completely — deletes from database and stops any active watcher"
    )]
    pub(crate) async fn task_remove(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> Result<CallToolResult, McpError> {
        let _ = self.watcher_engine.stop_watcher(&id).await;

        // Deletes by id without parsing the stored row, so a corrupt row
        // (e.g. malformed trigger_config) can always be removed — it must
        // never block its own repair.
        let deleted = self
            .db
            .delete_agent(&id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if !deleted {
            return Ok(error_result(&format!("No agent found with ID '{}'", id)));
        }

        self.scheduler_notify.notify_one();
        Ok(success_result(&format!("Agent '{}' removed", id)))
    }

    /// Enable a disabled agent.
    #[tool(
        name = "agent_enable",
        description = "Enable a disabled agent — resumes scheduling or file watching"
    )]
    pub(crate) async fn task_enable(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> Result<CallToolResult, McpError> {
        let Some(existing) = self
            .db
            .get_agent(&id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Ok(error_result(&format!("No agent found with ID '{}'", id)));
        };

        if existing.is_expired() {
            let mut updated = existing.clone();
            updated.expires_at = None;
            self.db
                .upsert_agent(&updated)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        }

        self.db
            .update_agent_enabled(&id, true)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.scheduler_notify.notify_one();

        if existing.is_watch() {
            let _ = self.watcher_engine.start_watcher(&existing).await;
        }

        Ok(success_result(&format!("Agent '{}' enabled", id)))
    }

    /// Schedule a one-shot future enable for a disabled agent.
    #[tool(
        name = "agent_schedule_enable",
        description = "Schedule a one-shot enable for an agent at a future ISO 8601 time — the agent stays disabled until then, when the scheduler enables it and clears the schedule"
    )]
    pub(crate) async fn task_schedule_enable(
        &self,
        Parameters(AgentScheduleEnableParams { id, at }): Parameters<AgentScheduleEnableParams>,
    ) -> Result<CallToolResult, McpError> {
        let Some(_existing) = self
            .db
            .get_agent(&id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Ok(error_result(&format!("No agent found with ID '{}'", id)));
        };

        let at = match chrono::DateTime::parse_from_rfc3339(&at) {
            Ok(dt) => dt.with_timezone(&chrono::Utc),
            Err(e) => {
                return Ok(error_result(&format!(
                    "Invalid ISO 8601 timestamp '{}': {}",
                    at, e
                )));
            }
        };

        self.db
            .schedule_agent_enable(&id, at)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.scheduler_notify.notify_one();

        Ok(success_result(&format!(
            "Agent '{}' scheduled to enable at {}",
            id,
            at.to_rfc3339()
        )))
    }

    /// Disable an agent without removing it.
    #[tool(
        name = "agent_disable",
        description = "Disable an agent without removing it — pauses scheduling or file watching"
    )]
    pub(crate) async fn task_disable(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> Result<CallToolResult, McpError> {
        let Some(existing) = self
            .db
            .get_agent(&id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Ok(error_result(&format!("No agent found with ID '{}'", id)));
        };

        self.db
            .update_agent_enabled(&id, false)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if existing.is_watch() {
            let _ = self.watcher_engine.stop_watcher(&id).await;
        }

        self.scheduler_notify.notify_one();
        Ok(success_result(&format!("Agent '{}' disabled", id)))
    }

    /// Execute an agent immediately, outside its schedule.
    #[tool(
        name = "agent_run",
        description = "Execute an agent immediately outside its schedule — useful for testing"
    )]
    pub(crate) async fn agent_run(
        &self,
        Parameters(IdParam { id }): Parameters<IdParam>,
    ) -> Result<CallToolResult, McpError> {
        let existing = self
            .db
            .get_agent(&id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let Some(agent) = existing else {
            return Ok(error_result(&format!("No agent found with ID '{}'", id)));
        };

        let executor = Arc::clone(&self.executor);
        let notification_service = self.notification_service.clone();
        let agent_id = id.clone();

        tokio::spawn(async move {
            let result = executor.execute_agent(&agent, true).await;
            notify_run_result(&notification_service, &agent_id, result);
        });

        Ok(success_result(&format!(
            "Agent '{}' launched in background. Use agent_logs to check progress.",
            id
        )))
    }

    /// Get daemon status and statistics.
    #[tool(
        name = "agent_status",
        description = "Get overall daemon health, scheduler state, and statistics"
    )]
    pub(crate) async fn task_status(&self) -> Result<CallToolResult, McpError> {
        let agents = self
            .db
            .list_agents()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        let active_agents = agents
            .iter()
            .filter(|a| a.enabled && !a.is_expired())
            .count();
        let active_watchers = self.watcher_engine.active_count().await;
        let uptime_str = format_uptime(self.start_time.elapsed().as_secs());
        let log_dir = data_dir()
            .map(|d| d.join("logs").to_string_lossy().to_string())
            .unwrap_or_else(|_| "unknown".to_string());

        let cron_count = agents.iter().filter(|a| a.is_cron()).count();
        let watch_count = agents.iter().filter(|a| a.is_watch()).count();
        let manual_count = agents.len() - cron_count - watch_count;

        let (transport, port_str) = transport_details(self.port);

        let mut status = format!(
            "canopy v{}\n\
             Uptime: {}\n\
             Transport: {}\n\
             Port: {}\n\
             Scheduler: internal (tokio)\n\
             Active agents: {} / {} (cron: {}, watch: {}, manual: {})\n\
             Active watchers: {}\n\
             Log directory: {}",
            env!("CARGO_PKG_VERSION"),
            uptime_str,
            transport,
            port_str,
            active_agents,
            agents.len(),
            cron_count,
            watch_count,
            manual_count,
            active_watchers,
            log_dir,
        );

        append_temporal_agents_section(&mut status, &agents);

        Ok(CallToolResult::success(vec![Content::text(status)]))
    }

    /// Get log output for an agent.
    #[tool(
        name = "agent_logs",
        description = "Get the log output for an agent with optional line and time filters"
    )]
    pub(crate) async fn task_logs(
        &self,
        Parameters(params): Parameters<TaskLogsParams>,
    ) -> Result<CallToolResult, McpError> {
        let log_path = resolve_log_path(&self.db, &params.id)?;
        let path = std::path::Path::new(&log_path);
        if !path.exists() {
            return Ok(success_result(&format!(
                "No logs found for '{}'. The agent has not been executed yet.",
                params.id
            )));
        }

        let output = format_log_output(
            path,
            &params.id,
            params.since.as_deref(),
            params.lines.unwrap_or(50),
        )?;
        let Some(run_info) = recent_runs_output(&self.db, &params.id) else {
            return Ok(CallToolResult::success(vec![Content::text(output)]));
        };

        Ok(CallToolResult::success(vec![Content::text(format!(
            "{output}{run_info}"
        ))]))
    }

    /// Update fields of an existing agent without recreating it.
    #[tool(
        name = "agent_update",
        description = "Modify an existing agent. Only the provided fields are updated — omitted fields remain unchanged. Auto-detects whether the agent is a cron or watch agent and applies the appropriate fields."
    )]
    pub(crate) async fn task_update(
        &self,
        Parameters(params): Parameters<TaskUpdateParams>,
    ) -> Result<CallToolResult, McpError> {
        use crate::scheduler::validate_cron;

        if let Err(e) = validate_id(&params.id) {
            return Ok(error_result(&e));
        }

        let Some(mut agent) = self.db.get_agent(&params.id).map_err(internal_error)? else {
            return Ok(error_result(&format!(
                "No agent found with ID '{}'",
                params.id
            )));
        };

        if let Some(new_id) = params.new_id.as_deref() {
            if new_id != params.id {
                if let Err(e) = validate_id(new_id) {
                    return Ok(error_result(&e));
                }
                let new_log_path = make_log_path(new_id)?;

                if agent.is_watch() {
                    let _ = self.watcher_engine.stop_watcher(&params.id).await;
                }

                if let Err(e) = self.db.rename_agent(&params.id, new_id, &new_log_path) {
                    return Ok(error_result(&e.to_string()));
                }

                agent.id = new_id.to_string();
                agent.log_path = new_log_path;
            }
        }

        if let Err(e) = apply_scalar_updates(&mut agent, &params) {
            return Ok(error_result(&e));
        }
        if let Err(e) = apply_trigger_updates(&mut agent, &params, &validate_cron) {
            return Ok(error_result(&e));
        }

        self.db.upsert_agent(&agent).map_err(internal_error)?;
        // Success-notification opt-in (B27) lives outside the agent row's
        // upserted columns, so set it separately when the caller provided it.
        if let Some(notify_on_success) = params.notify_on_success {
            self.db
                .set_agent_notify_on_success(&agent.id, notify_on_success)
                .map_err(internal_error)?;
        }
        if agent.is_cron() {
            self.scheduler_notify.notify_one();
        }

        if let Some(result) = self.restart_updated_watcher(&params, &agent).await? {
            return Ok(result);
        }

        Ok(success_result(&format!(
            "Agent '{}' updated successfully.",
            agent.id
        )))
    }

    /// Report execution status from within a running agent.
    #[tool(
        name = "agent_report",
        description = "Report execution status for a running agent. The run_id is provided in the agent execution prompt. Call with status='in_progress' immediately when starting, then status='success' or status='error' with a summary when finished."
    )]
    pub(crate) async fn task_report(
        &self,
        Parameters(params): Parameters<TaskReportParams>,
    ) -> Result<CallToolResult, McpError> {
        let status = match parse_report_status(&params.status) {
            Ok(status) => status,
            Err(e) => return Ok(error_result(e)),
        };
        if let Err(e) = validate_report_summary(status, params.summary.as_deref()) {
            return Ok(error_result(e));
        }

        let Some(run) = self.db.get_run(&params.run_id).map_err(internal_error)? else {
            return Err(internal_error(format!(
                "Run '{}' not found.",
                params.run_id
            )));
        };
        if let Some(result) = handle_timed_out_run(&self.db, &params.run_id, &run) {
            return Ok(result);
        }
        if let Err(e) = validate_run_transition(run.status, status) {
            return Ok(error_result(&e));
        }

        let updated = self
            .db
            .update_run_status(&params.run_id, status, params.summary.as_deref())
            .map_err(internal_error)?;
        if !updated {
            return Ok(error_result(&format!(
                "Failed to update run '{}'.",
                params.run_id
            )));
        }

        update_agent_last_run(&self.db, &run, status);
        Ok(success_result(&format!(
            "Run '{}' status updated to '{}'.",
            params.run_id, status
        )))
    }
}

impl TaskTriggerHandler {
    pub(crate) fn format_session_info(session: &InteractiveSession) -> String {
        let attached = if session_is_attached(session) {
            "yes"
        } else {
            "no"
        };
        let hook_target = if session.status == "active" {
            "yes".to_string()
        } else {
            format!(
                "no (status: {} — an interactive hook rejects this id until the session is live again)",
                session.status
            )
        };
        format!(
            "- id: {} | name: {} | platform: {} | workdir: {} | attached: {} | hook_target: {}",
            session.id, session.name, session.cli, session.working_dir, attached, hook_target
        )
    }

    pub(crate) async fn restart_updated_watcher(
        &self,
        params: &TaskUpdateParams,
        agent: &Agent,
    ) -> Result<Option<CallToolResult>, McpError> {
        // A rename always needs the watcher to end up running under the new
        // id, even if no other watch-related field changed.
        let renamed = agent.id != params.id;
        if !agent.is_watch() || (!watcher_restart_needed(params) && !renamed) {
            return Ok(None);
        }

        // The watcher for a renamed agent may already have been stopped
        // under the old id before the rename; stopping it again here is a
        // harmless no-op.
        let _ = self.watcher_engine.stop_watcher(&params.id).await;
        if !agent.enabled {
            return Ok(None);
        }

        let Err(e) = self.watcher_engine.start_watcher(agent).await else {
            return Ok(Some(success_result(&format!(
                "Agent '{}' updated successfully. Watcher restarted with new configuration.",
                agent.id
            ))));
        };

        Ok(Some(CallToolResult::success(vec![Content::text(format!(
            "Agent '{}' updated but watcher failed to restart: {}. It will be retried on daemon restart.",
            agent.id, e
        ))])))
    }
}

/// Whether an interactive session is attached to a live CLI process on this
/// host right now (CM16). `true` only when provable: the row was recorded on
/// the current boot AND its PID is still alive. A different boot id, a NULL
/// boot id, or a missing/dead PID all read as `false` — the session may
/// still be a valid hook target (an enqueued send is delivered when a TUI
/// next attaches), it just is not attached at this moment. Side-effect-free:
/// no PTY touched, no session woken.
pub(crate) fn session_is_attached(session: &InteractiveSession) -> bool {
    match (
        session.boot_id.as_deref(),
        crate::system::boot_id().as_deref(),
    ) {
        (Some(stored), Some(current)) if stored == current => session
            .pid
            .map(crate::system::process_is_alive)
            .unwrap_or(false),
        _ => false,
    }
}

pub(crate) fn transport_details(port: u16) -> (&'static str, String) {
    if port > 0 {
        ("Streamable HTTP", port.to_string())
    } else {
        ("stdio", "N/A".to_owned())
    }
}
