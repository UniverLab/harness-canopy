//! `scheduled_prompts` action implementations split out of `daemon::handler`.
//! Called from the resource tool in `handler_resource_tools_final`; behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::*;
use crate::db::scheduled_prompts::ScheduledPromptProvenance;
use axum::http::request::Parts;
use rmcp::model::{CallToolResult, ContentBlock};
use rmcp::ErrorData as McpError;

impl TaskTriggerHandler {
    /// Schedule a prompt for future delivery to an interactive session
    /// (CM22). Exposes the same `scheduled_prompts` mechanism the
    /// promptbuilder and graph hooks already use: delivery happens on the
    /// daemon's delivery task within seconds of `fire_at`, a dead target is preserved in
    /// `failed_scheduled_prompts`, and a message enqueued while no TUI is
    /// running simply waits.
    pub(crate) async fn scheduled_prompt_create_impl(
        &self,
        params: ScheduledPromptCreateParams,
        parts: Option<&Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.record_harness_id(parts);
        let agent_id = self.resolve_sync_agent_id(parts)?;

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

        let target_id = params
            .target_session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let target_name = params
            .target_session_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let resolved_target = if target_id.is_none() && target_name.is_none() {
            match self
                .db
                .resolve_target_session(Some(agent_id.as_str()), None)
            {
                Ok(resolved) => resolved,
                Err(err) => return Ok(error_result(&err.to_string())),
            }
        } else {
            match self.db.resolve_target_session(target_id, target_name) {
                Ok(resolved) => resolved,
                Err(err) => return Ok(error_result(&err.to_string())),
            }
        };
        let target_session_id = resolved_target.id;
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

        let provenance = ScheduledPromptProvenance::agent(&agent_id);
        let workdir_opt =
            (!target_session.working_dir.is_empty()).then_some(target_session.working_dir.as_str());
        let id = format!("ss-{}", uuid::Uuid::new_v4());

        self.db
            .insert_scheduled_prompt(
                &id,
                &params.prompt,
                &target_session_id,
                workdir_opt,
                fire_at,
                None,
                Some(&provenance),
            )
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.scheduled_prompt_notify.notify_one();

        Ok(success_result(&format!(
            "Scheduled send '{}' created for session '{}' — fires at {}.",
            id,
            target_session_id,
            fire_at.to_rfc3339()
        )))
    }

    /// List pending scheduled sends for a session (CM22).
    pub(crate) async fn scheduled_prompt_list_impl(
        &self,
        params: ScheduledPromptListParams,
        parts: Option<&Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.record_harness_id(parts);
        let session_id = match params
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            Some(id) => id.to_string(),
            None => self.resolve_sync_agent_id(parts)?,
        };

        let pending = self
            .db
            .list_pending_scheduled_prompts_for_session(&session_id)
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
            let origin = pending_send_origin(send.provenance.as_ref());
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
    pub(crate) async fn scheduled_prompt_cancel_impl(
        &self,
        ScheduledPromptCancelParams { id }: ScheduledPromptCancelParams,
    ) -> Result<CallToolResult, McpError> {
        let deleted = self
            .db
            .delete_scheduled_prompt(&id)
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

/// The `| origin: <kind>` suffix a pending-list line carries for a
/// provenance-bearing send — the same tag the send was enqueued with, so a
/// `deliver` send is distinguishable from `hook`/`agent` ones at a glance.
/// Provenance-free rows (promptbuilder sends and rows written before the
/// column existed) carry no suffix.
pub(crate) fn pending_send_origin(provenance: Option<&ScheduledPromptProvenance>) -> &'static str {
    match provenance.map(|p| p.kind.as_str()) {
        Some("agent") => " | origin: agent",
        Some("hook") => " | origin: hook",
        Some("deliver") => " | origin: deliver",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::notification_service::DefaultNotificationService;
    use crate::application::notification_service::NotificationService;
    use crate::daemon::handler::HandlerDeps;
    use crate::daemon::handler::TaskTriggerHandler;
    use crate::db::Database;
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::watchers::WatcherEngine;
    use std::sync::Arc;
    use tokio::sync::Notify;

    fn make_handler() -> (tempfile::TempDir, Arc<Database>, TaskTriggerHandler) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
        let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
        let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
        let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
        let watcher_engine = Arc::new(WatcherEngine::new(
            Arc::clone(&db),
            Arc::clone(&executor),
            Arc::clone(&graph_engine),
        ));
        let ingestion = Arc::new(IngestionManager::new(
            Arc::clone(&db),
            dir.path().to_path_buf(),
        ));
        let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
            dir.path().join("skills"),
            Vec::new(),
            15,
        ));
        let handler = TaskTriggerHandler::with_deps(HandlerDeps {
            db: Arc::clone(&db),
            executor,
            watcher_engine,
            scheduler_notify: Arc::new(Notify::new()),
            scheduled_prompt_notify: Arc::new(Notify::new()),
            graph_engine,
            notification_service: notif,
            ingestion,
            dynamic_skills,
            shelf: crate::sessions::shelf::ShelfStore::for_test(&db),
            hub: None,
            start_time: std::time::Instant::now(),
            port: 0,
        });
        (dir, db, handler)
    }

    fn add_session(db: &Database, id: &str, name: &str) {
        db.insert_interactive_session(
            id,
            name,
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
    }

    fn text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|b| b.as_text().map(|t| t.text.clone()))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn agent_parts(id: &str) -> axum::http::request::Parts {
        let req = axum::http::Request::builder()
            .header(crate::shared::sync_identity::CANOPY_AGENT_ID_HEADER, id)
            .body(())
            .unwrap();
        req.into_parts().0
    }

    #[test]
    fn pending_origin_names_agent_hook_and_deliver() {
        assert_eq!(
            pending_send_origin(Some(&ScheduledPromptProvenance::agent("s1"))),
            " | origin: agent"
        );
        assert_eq!(
            pending_send_origin(Some(&ScheduledPromptProvenance::hook("g1", "on_failed"))),
            " | origin: hook"
        );
        assert_eq!(
            pending_send_origin(Some(&ScheduledPromptProvenance::deliver("g1", "n1", "r1"))),
            " | origin: deliver"
        );
    }

    #[test]
    fn pending_origin_empty_without_provenance() {
        assert_eq!(pending_send_origin(None), "");
        assert_eq!(
            pending_send_origin(Some(&ScheduledPromptProvenance::agent_trigger(
                "a1", "cron"
            ))),
            ""
        );
        assert_eq!(
            pending_send_origin(Some(&ScheduledPromptProvenance::spec_review())),
            ""
        );
    }

    #[tokio::test]
    async fn scheduled_prompt_create_resolves_target_session_name() {
        let (_dir, db, handler) = make_handler();
        add_session(&db, "s1", "laricifomes");
        let parts = agent_parts("caller-1");
        let result = handler
            .scheduled_prompt_create_impl(
                ScheduledPromptCreateParams {
                    prompt: "hello".into(),
                    target_session_id: None,
                    target_session_name: Some("laricifomes".into()),
                    at: None,
                    in_seconds: Some(60),
                },
                Some(&parts),
            )
            .await
            .unwrap();
        let out = text(&result);
        assert!(!result.is_error.unwrap_or(false), "{out}");
        let pending = db.list_pending_scheduled_prompts_for_session("s1").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].target_session_id, "s1");
    }

    #[tokio::test]
    async fn scheduled_prompt_create_accepts_name_in_target_session_id() {
        let (_dir, db, handler) = make_handler();
        add_session(&db, "s1", "toona");
        let parts = agent_parts("caller-1");
        let result = handler
            .scheduled_prompt_create_impl(
                ScheduledPromptCreateParams {
                    prompt: "hello".into(),
                    target_session_id: Some("toona".into()),
                    target_session_name: None,
                    at: None,
                    in_seconds: Some(60),
                },
                Some(&parts),
            )
            .await
            .unwrap();
        let out = text(&result);
        assert!(!result.is_error.unwrap_or(false), "{out}");
        let pending = db.list_pending_scheduled_prompts_for_session("s1").unwrap();
        assert_eq!(pending.len(), 1);
    }

    #[tokio::test]
    async fn scheduled_prompt_create_ambiguous_name_error_lists_sorted_matches() {
        let (_dir, db, handler) = make_handler();
        add_session(&db, "s2", "orch");
        add_session(&db, "s1", "orch");
        let parts = agent_parts("caller-1");
        let result = handler
            .scheduled_prompt_create_impl(
                ScheduledPromptCreateParams {
                    prompt: "hello".into(),
                    target_session_id: None,
                    target_session_name: Some("orch".into()),
                    at: None,
                    in_seconds: Some(60),
                },
                Some(&parts),
            )
            .await
            .unwrap();
        let out = text(&result);
        assert!(result.is_error.unwrap_or(false), "{out}");
        assert!(out.contains("ambiguous"), "{out}");
        assert!(out.contains("orch (s1), orch (s2)"), "{out}");
    }

    #[tokio::test]
    async fn scheduled_prompt_create_unknown_name_error_lists_sorted_live_names() {
        let (_dir, db, handler) = make_handler();
        add_session(&db, "s1", "zeta");
        add_session(&db, "s2", "alpha");
        let parts = agent_parts("caller-1");
        let result = handler
            .scheduled_prompt_create_impl(
                ScheduledPromptCreateParams {
                    prompt: "hello".into(),
                    target_session_id: None,
                    target_session_name: Some("missing".into()),
                    at: None,
                    in_seconds: Some(60),
                },
                Some(&parts),
            )
            .await
            .unwrap();
        let out = text(&result);
        assert!(result.is_error.unwrap_or(false), "{out}");
        assert!(out.contains("does not exist or is no longer live"), "{out}");
        assert!(
            out.contains("Live interactive session names: alpha, zeta"),
            "{out}"
        );
    }

    #[tokio::test]
    async fn session_rename_tool_renames_live_session() {
        let (_dir, db, handler) = make_handler();
        add_session(&db, "s1", "alpha");
        let result = handler
            .session_rename(crate::daemon::params_extract::Parameters(
                SessionRenameParams {
                    target: "alpha".into(),
                    new_name: "beta".into(),
                },
            ))
            .await
            .unwrap();
        let out = text(&result);
        assert!(!result.is_error.unwrap_or(false), "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["id"], "s1");
        assert_eq!(v["old_name"], "alpha");
        assert_eq!(v["new_name"], "beta");
        let session = db.get_hookable_session("s1").unwrap().unwrap();
        assert_eq!(session.name, "beta");
    }
}
