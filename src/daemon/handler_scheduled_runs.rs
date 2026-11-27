//! MCP handler implementations for one-shot `at` scheduled graph runs.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_queues::{validate_queue_exists, validate_queue_not_consumed};
use crate::daemon::handler_run_prepare::{refuse_blocked_plain_launch, refuse_in_flight_run};
use crate::daemon::handler_validate::{resolve_prefix_or_error, validate_absolute_dir};
use crate::daemon::helpers::{error_result, success_result};
use crate::daemon::params::{
    GraphListScheduledRunsParams, GraphScheduleRunParams, GraphScheduledRunIdParams,
};
use crate::db::graph_scheduled_runs::GraphScheduledRun;
use crate::db::Database;
use crate::or_early_return;
use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

fn trim_non_empty(opt: Option<&str>) -> Option<String> {
    opt.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl TaskTriggerHandler {
    pub(crate) async fn graph_schedule_run_impl(
        &self,
        params: GraphScheduleRunParams,
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
        if self
            .db
            .get_graph(&graph_id)
            .map_err(internal_error)?
            .is_none()
        {
            return Ok(error_result(&format!("Graph '{graph_id}' not found.")));
        }

        let fire_at = match DateTime::parse_from_rfc3339(params.at.trim()) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => {
                return Ok(error_result(&format!(
                    "Invalid RFC 3339 timestamp '{}': {e}",
                    params.at
                )));
            }
        };
        if fire_at <= Utc::now() {
            return Ok(error_result(
                "Scheduled `at` timestamp must be in the future.",
            ));
        }

        let on_miss = trim_non_empty(params.on_miss.as_deref()).unwrap_or_else(|| "skip".into());
        if on_miss != "skip" && on_miss != "run" {
            return Ok(error_result(
                "Invalid `on_miss` policy: expected 'skip' or 'run'.",
            ));
        }

        let idea = trim_non_empty(params.idea.as_deref());
        let queue_id = trim_non_empty(params.queue_id.as_deref());
        if idea.is_some() && queue_id.is_some() {
            return Ok(error_result(
                "`idea` and `queue_id` are mutually exclusive.",
            ));
        }
        if let Some(qid) = queue_id.as_deref() {
            if let Err(e) = validate_queue_exists(&self.db, qid) {
                return Ok(error_result(&e));
            }
            if let Err(e) = validate_queue_not_consumed(&self.db, qid, &graph_id) {
                return Ok(error_result(&e));
            }
        }

        let workdir = trim_non_empty(params.workdir.as_deref());
        if let Some(wd) = workdir.as_deref() {
            if let Err(e) = validate_absolute_dir(wd) {
                return Ok(error_result(&e));
            }
        }

        let fire_at_str = fire_at.to_rfc3339();
        let run = GraphScheduledRun {
            id: uuid::Uuid::new_v4().to_string(),
            graph_id,
            fire_at: fire_at_str.clone(),
            idea,
            queue_id,
            workdir,
            on_miss: on_miss.clone(),
            status: "pending".to_string(),
            created_at: Utc::now().to_rfc3339(),
            fired_at: None,
            error: None,
        };
        self.db
            .insert_graph_scheduled_run(&run)
            .map_err(internal_error)?;
        self.scheduler_notify.notify_one();

        let body = serde_json::json!({
            "id": run.id,
            "graph_id": run.graph_id,
            "fire_at": fire_at_str,
            "on_miss": on_miss,
            "status": "pending",
        });
        Ok(success_result(
            &serde_json::to_string_pretty(&body).map_err(internal_error)?,
        ))
    }

    pub(crate) async fn graph_list_scheduled_runs_impl(
        &self,
        params: GraphListScheduledRunsParams,
    ) -> Result<CallToolResult, McpError> {
        let graph_id = match trim_non_empty(params.graph_id.as_deref()) {
            Some(raw) => match resolve_prefix_or_error(
                &self.db,
                &raw,
                Database::resolve_graph_id_by_prefix,
                "graph",
            ) {
                Ok(id) => Some(id),
                Err(e) => return Ok(e),
            },
            None => None,
        };
        let status = trim_non_empty(params.status.as_deref());
        let runs = self
            .db
            .list_graph_scheduled_runs(graph_id.as_deref(), status.as_deref())
            .map_err(internal_error)?;
        let body = serde_json::json!({ "scheduled_runs": runs });
        Ok(success_result(
            &serde_json::to_string_pretty(&body).map_err(internal_error)?,
        ))
    }

    pub(crate) async fn graph_cancel_scheduled_run_impl(
        &self,
        params: GraphScheduledRunIdParams,
    ) -> Result<CallToolResult, McpError> {
        let id = params.id.trim();
        if self
            .db
            .get_graph_scheduled_run(id)
            .map_err(internal_error)?
            .is_none()
        {
            return Ok(error_result(&format!(
                "No scheduled run found with ID '{id}'."
            )));
        }
        match self.db.cancel_graph_scheduled_run(id) {
            Ok(true) => {
                self.scheduler_notify.notify_one();
                let body = serde_json::json!({ "id": id, "status": "cancelled" });
                Ok(success_result(
                    &serde_json::to_string_pretty(&body).map_err(internal_error)?,
                ))
            }
            Ok(false) => Ok(error_result(&format!(
                "Scheduled run '{id}' is already cancelled."
            ))),
            Err(e) => Ok(error_result(&e.to_string())),
        }
    }

    pub(crate) async fn graph_run_scheduled_run_impl(
        &self,
        params: GraphScheduledRunIdParams,
    ) -> Result<CallToolResult, McpError> {
        let id = params.id.trim();
        let Some(run) = self
            .db
            .get_graph_scheduled_run(id)
            .map_err(internal_error)?
        else {
            return Ok(error_result(&format!(
                "No scheduled run found with ID '{id}'."
            )));
        };
        if run.status != "missed" && run.status != "pending" {
            return Ok(error_result(&format!(
                "Scheduled run '{id}' cannot be launched in status '{}'.",
                run.status
            )));
        }
        let Some(graph) = self.db.get_graph(&run.graph_id).map_err(internal_error)? else {
            return Ok(error_result(&format!(
                "Graph '{}' not found.",
                run.graph_id
            )));
        };
        let () = or_early_return!(refuse_blocked_plain_launch(&self.db, &run.graph_id, &graph));
        let () = or_early_return!(refuse_in_flight_run(&self.db, &run.graph_id));

        Arc::clone(&self.graph_engine).start_background_run(
            run.graph_id.clone(),
            run.queue_id.clone(),
            run.workdir.clone(),
            run.idea.clone(),
            None,
            None,
        );
        let fired_at = Utc::now().to_rfc3339();
        self.db
            .mark_graph_scheduled_run_status(&run.id, "fired", Some(&fired_at), None)
            .map_err(internal_error)?;

        let body = serde_json::json!({
            "id": run.id,
            "graph_id": run.graph_id,
            "status": "fired",
            "fired_at": fired_at,
        });
        Ok(success_result(
            &serde_json::to_string_pretty(&body).map_err(internal_error)?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::daemon::handler::HandlerDeps;
    use crate::domain::graphs::{Graph, GraphStatus};
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::watchers::WatcherEngine;
    use chrono::Duration as ChronoDuration;

    fn test_handler() -> (tempfile::TempDir, Arc<Database>, TaskTriggerHandler) {
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
        let scheduler_notify = Arc::new(tokio::sync::Notify::new());
        let deps = HandlerDeps {
            db: Arc::clone(&db),
            executor,
            watcher_engine,
            scheduler_notify: Arc::clone(&scheduler_notify),
            scheduled_prompt_notify: scheduler_notify,
            graph_engine,
            notification_service: notif,
            ingestion,
            dynamic_skills,
            shelf: crate::sessions::shelf::ShelfStore::for_test(&db),
            hub: None,
            start_time: std::time::Instant::now(),
            port: 0,
        };
        let handler = TaskTriggerHandler::with_deps(deps);
        (dir, db, handler)
    }

    fn insert_graph(db: &Database, id: &str, workdir: &str) {
        db.insert_graph(&Graph {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            workdir: workdir.to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            trigger_prompt: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            require_spec_type: false,
            infra_node_id: None,
            enabled: true,
            enable_at: None,
            expires_at: None,
        })
        .unwrap();
    }

    fn result_text(res: &CallToolResult) -> String {
        res.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn schedule_list_cancel_and_validation() {
        let (dir, db, handler) = test_handler();
        insert_graph(&db, "g-sched-1", &dir.path().to_string_lossy());

        // Reject past `at`.
        let past = handler
            .graph_schedule_run_impl(GraphScheduleRunParams {
                graph_id: "g-sched-1".into(),
                at: (Utc::now() - ChronoDuration::seconds(30)).to_rfc3339(),
                idea: None,
                queue_id: None,
                workdir: None,
                on_miss: None,
            })
            .await
            .unwrap();
        assert_eq!(past.is_error, Some(true));

        // Reject invalid `on_miss`.
        let bad_miss = handler
            .graph_schedule_run_impl(GraphScheduleRunParams {
                graph_id: "g-sched-1".into(),
                at: (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339(),
                idea: None,
                queue_id: None,
                workdir: None,
                on_miss: Some("explode".into()),
            })
            .await
            .unwrap();
        assert_eq!(bad_miss.is_error, Some(true));

        // Reject idea + queue_id together.
        let both = handler
            .graph_schedule_run_impl(GraphScheduleRunParams {
                graph_id: "g-sched-1".into(),
                at: (Utc::now() + ChronoDuration::seconds(60)).to_rfc3339(),
                idea: Some("hi".into()),
                queue_id: Some("q1".into()),
                workdir: None,
                on_miss: None,
            })
            .await
            .unwrap();
        assert_eq!(both.is_error, Some(true));

        // Valid schedule_run.
        let ok = handler
            .graph_schedule_run_impl(GraphScheduleRunParams {
                graph_id: "g-sched-1".into(),
                at: (Utc::now() + ChronoDuration::seconds(120)).to_rfc3339(),
                idea: Some("build feature".into()),
                queue_id: None,
                workdir: None,
                on_miss: Some("skip".into()),
            })
            .await
            .unwrap();
        assert_ne!(ok.is_error, Some(true));
        let created: serde_json::Value = serde_json::from_str(&result_text(&ok)).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["status"], "pending");
        assert_eq!(created["on_miss"], "skip");

        // List scheduled runs.
        let listed = handler
            .graph_list_scheduled_runs_impl(GraphListScheduledRunsParams {
                graph_id: Some("g-sched-1".into()),
                status: Some("pending".into()),
            })
            .await
            .unwrap();
        let list_json: serde_json::Value = serde_json::from_str(&result_text(&listed)).unwrap();
        assert_eq!(list_json["scheduled_runs"].as_array().unwrap().len(), 1);

        // Cancel scheduled run.
        let cancelled = handler
            .graph_cancel_scheduled_run_impl(GraphScheduledRunIdParams { id: id.clone() })
            .await
            .unwrap();
        assert_ne!(cancelled.is_error, Some(true));
        let row = db.get_graph_scheduled_run(&id).unwrap().unwrap();
        assert_eq!(row.status, "cancelled");
    }

    #[tokio::test]
    async fn run_scheduled_run_launches_missed_and_cancel_refuses_fired() {
        let (dir, db, handler) = test_handler();
        insert_graph(&db, "g-sched-2", &dir.path().to_string_lossy());

        db.insert_graph_scheduled_run(&GraphScheduledRun {
            id: "sr-missed-1".into(),
            graph_id: "g-sched-2".into(),
            fire_at: (Utc::now() - ChronoDuration::seconds(300)).to_rfc3339(),
            idea: Some("manual retry".into()),
            queue_id: None,
            workdir: None,
            on_miss: "skip".into(),
            status: "missed".into(),
            created_at: Utc::now().to_rfc3339(),
            fired_at: None,
            error: None,
        })
        .unwrap();

        let launched = handler
            .graph_run_scheduled_run_impl(GraphScheduledRunIdParams {
                id: "sr-missed-1".into(),
            })
            .await
            .unwrap();
        assert_ne!(launched.is_error, Some(true));
        let row = db.get_graph_scheduled_run("sr-missed-1").unwrap().unwrap();
        assert_eq!(row.status, "fired");
        assert!(row.fired_at.is_some());

        // Cancel refuses already-fired row.
        let cancel_fired = handler
            .graph_cancel_scheduled_run_impl(GraphScheduledRunIdParams {
                id: "sr-missed-1".into(),
            })
            .await
            .unwrap();
        assert_eq!(cancel_fired.is_error, Some(true));
    }
}
