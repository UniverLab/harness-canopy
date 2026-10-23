//! MCP handler tools split out of `daemon::handler` (scheduled-send tools).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::scheduled_sends::ScheduledSendProvenance;
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;

#[tool_router(router = tool_router_scheduled_sends, vis = "pub(crate)")]
impl TaskTriggerHandler {
    /// Schedule a prompt for future delivery to an interactive session
    /// (CM22). Exposes the same `scheduled_sends` mechanism the
    /// promptbuilder and graph hooks already use: delivery happens on the
    /// TUI's next refresh tick, a dead target is preserved in
    /// `failed_scheduled_sends`, and a message enqueued while no TUI is
    /// running simply waits.
    #[tool(
        name = "scheduled_send_create",
        description = "Schedule a prompt for delivery to an interactive session at a future time — the way to have an agent wake itself (or another session) later instead of polling on a cron. Omit `target_session_id` to target your own session. Give exactly one of `at` (ISO 8601, e.g. \"2026-07-10T09:00:00Z\") or `in_seconds` (relative delay, e.g. 2400 for \"in 40 minutes\"). Delivery happens on the target's next TUI refresh tick — if no TUI is attached when the time arrives, the send waits rather than being lost. Refused immediately if the target session does not exist. Returns the new scheduled send's id."
    )]
    pub(crate) async fn scheduled_send_create(
        &self,
        Parameters(params): Parameters<ScheduledSendCreateParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.reject_if_nursery(parts.as_ref())?;
        let agent_id = self.resolve_sync_agent_id(parts.as_ref())?;

        if let Err(e) = crate::domain::validation::validate_prompt(&params.prompt) {
            return Ok(error_result(&e));
        }

        if params.at.is_some() && params.in_seconds.is_some() {
            return Ok(error_result("Pass either `at` or `in_seconds`, not both."));
        }
        let fire_at = if let Some(at) = params.at.as_deref() {
            match chrono::DateTime::parse_from_rfc3339(at) {
                Ok(dt) => dt.with_timezone(&chrono::Utc),
                Err(e) => {
                    return Ok(error_result(&format!(
                        "Invalid ISO 8601 timestamp '{}': {}",
                        at, e
                    )));
                }
            }
        } else if let Some(secs) = params.in_seconds {
            if secs < 0 {
                return Ok(error_result("`in_seconds` must be zero or positive."));
            }
            chrono::Utc::now() + chrono::Duration::seconds(secs)
        } else {
            return Ok(error_result(
                "Provide either `at` (an ISO 8601 timestamp) or `in_seconds` (a relative delay in seconds).",
            ));
        };

        let target_session_id = match params
            .target_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(id) => id.to_string(),
            None => agent_id.clone(),
        };

        let Some(target_session) = self
            .db
            .get_hookable_session(&target_session_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Ok(error_result(&format!(
                "No live interactive session found with ID '{}'.",
                target_session_id
            )));
        };

        let provenance = ScheduledSendProvenance::agent(&agent_id);
        let workdir_opt =
            (!target_session.working_dir.is_empty()).then_some(target_session.working_dir.as_str());
        let id = format!("ss-{}", uuid::Uuid::new_v4());

        self.db
            .insert_scheduled_send(
                &id,
                &params.prompt,
                &target_session_id,
                workdir_opt,
                fire_at,
                None,
                Some(&provenance),
            )
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        Ok(success_result(&format!(
            "Scheduled send '{}' created for session '{}' — fires at {}.",
            id,
            target_session_id,
            fire_at.to_rfc3339()
        )))
    }

    /// List pending scheduled sends for a session (CM22).
    #[tool(
        name = "scheduled_send_list",
        description = "List pending (not yet fired) scheduled sends for a session, soonest first. Omit `session_id` to list your own session's pending sends. A send disappears from this list once delivered or cancelled."
    )]
    pub(crate) async fn scheduled_send_list(
        &self,
        Parameters(params): Parameters<ScheduledSendListParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let session_id = match params
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(id) => id.to_string(),
            None => {
                self.reject_if_nursery(parts.as_ref())?;
                self.resolve_sync_agent_id(parts.as_ref())?
            }
        };

        let pending = self
            .db
            .list_pending_scheduled_sends_for_session(&session_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if pending.is_empty() {
            return Ok(success_result(&format!(
                "No pending scheduled sends for session '{}'.",
                session_id
            )));
        }

        let mut lines = vec![format!(
            "Found {} pending scheduled send(s) for session '{}':\n",
            pending.len(),
            session_id
        )];
        lines.extend(pending.iter().map(|send| {
            let preview: String = send.prompt.chars().take(80).collect();
            let ellipsis = if send.prompt.chars().count() > 80 {
                "…"
            } else {
                ""
            };
            let origin = match send.provenance.as_ref().map(|p| p.kind.as_str()) {
                Some("agent") => " | origin: agent",
                Some("hook") => " | origin: hook",
                _ => "",
            };
            format!(
                "- id: {} | fire_at: {}{} | prompt: {}{}",
                send.id,
                send.fire_at.to_rfc3339(),
                origin,
                preview,
                ellipsis
            )
        }));
        Ok(CallToolResult::success(vec![ContentBlock::text(
            lines.join("\n"),
        )]))
    }

    /// Cancel a pending scheduled send by id (CM22).
    #[tool(
        name = "scheduled_send_cancel",
        description = "Cancel a pending scheduled send by id before it fires. Errors if the id is unknown or already fired — cancellation is not retroactive."
    )]
    pub(crate) async fn scheduled_send_cancel(
        &self,
        Parameters(ScheduledSendCancelParams { id }): Parameters<ScheduledSendCancelParams>,
    ) -> Result<CallToolResult, McpError> {
        let deleted = self
            .db
            .delete_scheduled_send(&id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        if !deleted {
            return Ok(error_result(&format!(
                "No pending scheduled send found with ID '{}'.",
                id
            )));
        }
        Ok(success_result(&format!(
            "Scheduled send '{}' cancelled.",
            id
        )))
    }
}
