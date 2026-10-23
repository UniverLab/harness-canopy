//! MCP `agent_add` / `agent_watch` delivery action (CM53).
//!
//! An agent's action can be `headless` (the default) or `send_to_session`,
//! which delivers the rendered prompt into a live interactive session instead
//! of spawning a CLI. This module owns the action parsing (including the
//! caller's own-session default, which runs BEFORE the model-level "exactly
//! one target" validation) and the construction of the agent rows.

use axum::http::request::Parts;
use chrono::Utc;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;

use crate::application::ports::AgentRepository;
use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::{internal_error, make_log_path};
use crate::daemon::handler_helpers::parse_schedule_and_expiry;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::{TaskAddParams, TaskWatchParams};
use crate::domain::models::{Agent, AgentAction, Cli, Trigger, WatchEvent};
use crate::domain::validation::{validate_id, validate_prompt, validate_watch_path};

/// Whether a `task_add` / `task_watch` call is a headless run or a session
/// delivery, decided from the action and target fields.
pub(crate) enum AddAction {
    Headless,
    Send,
}

/// Classify the action/target fields of an add/watch call. Targets on a
/// headless call are refused, so an operator cannot silently ignore them.
pub(crate) fn classify_action(
    action: Option<&str>,
    name: Option<&str>,
    id: Option<&str>,
) -> Result<AddAction, String> {
    match action {
        None | Some("headless") => {
            if name.is_some() || id.is_some() {
                Err("target_session_* only apply to action send_to_session.".to_string())
            } else {
                Ok(AddAction::Headless)
            }
        }
        Some("send_to_session") => Ok(AddAction::Send),
        Some(other) => Err(format!(
            "Unknown action '{other}': use headless or send_to_session."
        )),
    }
}

/// Build the `AgentAction` for an add/watch/update call. For
/// `send_to_session` with neither target set, defaults the id to `caller`
/// (the calling session) — a caller with no session is refused with a message
/// naming what to pass. Runs before model validation, so the model's
/// "exactly one target" rule is never in conflict with this default.
pub(crate) fn build_send_action(
    action: Option<&str>,
    name: Option<String>,
    id: Option<String>,
    caller: Option<String>,
) -> Result<AgentAction, String> {
    match action {
        None | Some("headless") => {
            if name.is_some() || id.is_some() {
                Err("target_session_* only apply to action send_to_session.".to_string())
            } else {
                Ok(AgentAction::Headless)
            }
        }
        Some("send_to_session") => {
            let (name, id) = if name.is_none() && id.is_none() {
                match caller {
                    Some(caller) => (None, Some(caller)),
                    None => {
                        return Err("This caller has no session to default to: pass target_session_name or target_session_id.".to_string());
                    }
                }
            } else {
                (name, id)
            };
            AgentAction::send_to_session(name, id)
        }
        Some(other) => Err(format!(
            "Unknown action '{other}': use headless or send_to_session."
        )),
    }
}

/// The calling session's id, resolved from the MCP request parts (the canopy
/// session header). `None` when the caller has no session.
pub(crate) fn caller_session_id(
    parts: Option<&Parts>,
    handler: &TaskTriggerHandler,
) -> Option<String> {
    handler.resolve_sync_agent_id(parts).ok()
}

/// Build a `send_to_session` agent. The CLI is stored empty — it is never
/// read on the delivery path — and `model`/`effort` stay unset.
pub(crate) fn new_send_agent(
    id: String,
    prompt: String,
    action: AgentAction,
    working_dir: Option<String>,
    log_path: String,
) -> Agent {
    Agent {
        id,
        prompt,
        trigger: None,
        cli: Cli(String::new()),
        model: None,
        effort: None,
        working_dir,
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path,
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action,
    }
}

/// A human label for the configured target, used in success text.
pub(crate) fn target_label(action: &AgentAction) -> String {
    action
        .target_session_name()
        .or(action.target_session_id())
        .unwrap_or("")
        .to_string()
}

impl TaskTriggerHandler {
    /// `agent_add` with `action: "send_to_session"`.
    pub(crate) async fn send_agent_add(
        &self,
        params: &TaskAddParams,
        parts: Option<&Parts>,
    ) -> Result<CallToolResult, McpError> {
        let caller = caller_session_id(parts, self);
        let action = match build_send_action(
            params.action.as_deref(),
            params.target_session_name.clone(),
            params.target_session_id.clone(),
            caller,
        ) {
            Ok(action) => action,
            Err(e) => return Ok(error_result(&e)),
        };

        if let Err(e) = validate_id(&params.id) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_prompt(&params.prompt) {
            return Ok(error_result(&e));
        }
        let (schedule_expr, expires_at) = match parse_schedule_and_expiry(
            &params.schedule,
            params.duration_minutes,
            &crate::scheduler::validate_cron,
        ) {
            Ok(parsed) => parsed,
            Err(e) => return Ok(error_result(&e)),
        };

        let log_path = make_log_path(&params.id)?;
        let mut agent = new_send_agent(
            params.id.clone(),
            params.prompt.clone(),
            action,
            params.working_dir.clone(),
            log_path,
        );
        // The delivery path never reads it, but an explicitly passed value is
        // still the run lock's timeout — do not silently drop it.
        agent.timeout_minutes = params.timeout_minutes.unwrap_or(15);
        agent.trigger = Some(Trigger::Cron {
            schedule_expr: schedule_expr.clone(),
        });
        agent.expires_at = expires_at;

        self.db.upsert_agent(&agent).map_err(internal_error)?;
        self.scheduler_notify.notify_one();

        Ok(success_result(&format!(
            "Agent '{}' will deliver to session '{}' on schedule '{}'. Delivery waits for a TUI to be open; the message is not lost.",
            agent.id,
            target_label(&agent.action),
            schedule_expr
        )))
    }

    /// `agent_watch` with `action: "send_to_session"`.
    pub(crate) async fn send_agent_watch(
        &self,
        params: &TaskWatchParams,
        parts: Option<&Parts>,
    ) -> Result<CallToolResult, McpError> {
        let caller = caller_session_id(parts, self);
        let action = match build_send_action(
            params.action.as_deref(),
            params.target_session_name.clone(),
            params.target_session_id.clone(),
            caller,
        ) {
            Ok(action) => action,
            Err(e) => return Ok(error_result(&e)),
        };

        if let Err(e) = validate_id(&params.id) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_prompt(&params.prompt) {
            return Ok(error_result(&e));
        }
        if let Err(e) = validate_watch_path(&params.path) {
            return Ok(error_result(&e));
        }
        let events = match WatchEvent::parse_list(&params.events) {
            Ok(events) => events,
            Err(e) => return Ok(error_result(&e)),
        };

        let log_path = make_log_path(&params.id)?;
        let mut agent = new_send_agent(
            params.id.clone(),
            params.prompt.clone(),
            action,
            None,
            log_path,
        );
        agent.timeout_minutes = params.timeout_minutes.unwrap_or(15);
        agent.trigger = Some(Trigger::Watch {
            path: params.path.clone(),
            events,
            debounce_seconds: params.debounce_seconds.unwrap_or(2),
            recursive: params.recursive.unwrap_or(false),
        });

        // Save before starting the watcher, same as the headless path.
        self.db.upsert_agent(&agent).map_err(internal_error)?;

        if let Err(e) = self.watcher_engine.start_watcher(&agent).await {
            tracing::warn!("Watcher '{}' saved but failed to start: {}", agent.id, e);
            return Ok(CallToolResult::success(vec![ContentBlock::text(format!(
                "Watcher '{}' for session '{}' registered but could not start watching '{}': {}. It will be retried on daemon restart.",
                agent.id,
                target_label(&agent.action),
                params.path,
                e
            ))]));
        }

        Ok(success_result(&format!(
            "Watcher '{}' will deliver to session '{}' on '{}' for events: {:?}",
            agent.id,
            target_label(&agent.action),
            params.path,
            params.events
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_send_action_defaults_to_caller_session() {
        let action =
            build_send_action(Some("send_to_session"), None, None, Some("sess-1".into())).unwrap();
        assert_eq!(action.target_session_id(), Some("sess-1"));
        assert!(action.target_session_name().is_none());
    }

    #[test]
    fn build_send_action_without_caller_refuses_and_asks_for_target() {
        let err = build_send_action(Some("send_to_session"), None, None, None).unwrap_err();
        assert!(
            err.contains("pass target_session_name or target_session_id"),
            "error: {err}"
        );
    }

    #[test]
    fn build_send_action_refuses_targets_on_headless() {
        assert!(build_send_action(Some("headless"), Some("orch".into()), None, None).is_err());
        assert!(build_send_action(None, None, Some("sess".into()), None).is_err());
    }

    #[test]
    fn build_send_action_unknown_action_refused() {
        let err = build_send_action(Some("run_graph"), None, None, None).unwrap_err();
        assert!(err.contains("Unknown action"), "error: {err}");
    }

    #[test]
    fn target_label_prefers_name_then_id() {
        let by_name = AgentAction::send_to_session(Some("orch".into()), None).unwrap();
        assert_eq!(target_label(&by_name), "orch");
        let by_id = AgentAction::send_to_session(None, Some("sess-1".into())).unwrap();
        assert_eq!(target_label(&by_id), "sess-1");
    }
}
