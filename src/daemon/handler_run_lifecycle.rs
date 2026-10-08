//! MCP handler tools split out of `daemon::handler` (run lifecycle tools (reset, schedule, pause, complete, blockers)).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::TaskTriggerHandler;
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_run_exec::{
    handle_retry_current_node, handle_skip_next_spec, in_flight_run_error, node_name_for_error,
    resolve_reported_run,
};
use crate::daemon::handler_validate::resolve_prefix_or_error;
use crate::daemon::helpers::{error_result, success_result, with_deprecated_note};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{ArchiveGraphOutcome, GraphResetOutcome, GraphRunStatus, GraphStatus};
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_run_lifecycle, vis = "pub(crate)")]
impl TaskTriggerHandler {
    /// Reset a `completed`/`failed` (or otherwise stalled) graph back to
    /// `pending` so it can be relaunched via `graph_run`, which otherwise
    /// refuses to resume a completed or failed graph.
    ///
    /// This is also the exact state transition the scheduler uses to
    /// auto-reset a `failed` graph when its `graph_schedule_autorun` fires —
    /// both paths call [`crate::db::Database::reset_graph`], so a human
    /// calling this tool and the scheduler resuming a failed graph on its own
    /// can never diverge in behavior.
    #[tool(
        name = "graph_reset",
        description = "Reset a completed/failed graph back to pending so graph_run can relaunch it. Without `specs`, resets every non-completed spec, leaving already-completed ones untouched so graph_run resumes at the first pending spec. With `specs`, resets exactly those spec IDs, even if they were completed. If the graph's last run was against a queue, its queue members are what get reset (same semantics), since a queue run's own bound specs are typically empty. Rejects a `running` graph — call graph_pause first. Note: a `failed` graph with a pending graph_schedule_autorun resets and resumes itself automatically when the schedule fires — call this manually only to reset sooner, reset a `completed` graph, or reset specific spec IDs."
    )]
    pub(crate) async fn graph_reset(
        &self,
        Parameters(params): Parameters<GraphResetParams>,
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
        perform_graph_reset(&self.db, &graph_id, params.specs.as_deref())
    }

    /// Schedule a one-shot future resume for a graph (e.g. a graph that failed
    /// on a quota can reschedule itself at the exact reset time instead of
    /// relying on a blindly polling cron). The scheduler fires it once the
    /// time is reached and the graph is fireable, then clears the schedule.
    ///
    /// Firing on a `failed` graph performs an explicit, logged
    /// auto-reset-and-resume: it runs the same transition as `graph_reset`
    /// (see [`crate::db::Database::reset_graph`]) and then resumes execution —
    /// this is the sanctioned way a quota-failed graph revives itself
    /// unattended. Firing on a `completed` graph does not re-run it: that is
    /// a human decision, made via `graph_reset` + `graph_run`.
    #[tool(
        name = "graph_schedule_autorun",
        description = "Schedule a one-shot resume for a graph at a future ISO 8601 time, or cancel a pending one. When the scheduler reaches that time: a `failed` graph is auto-reset (same transition as graph_reset) and resumed — useful for a graph that failed on a quota to reschedule its own resumption at the exact reset time; a `completed` graph is left alone (the schedule is cleared but the graph is not re-run — use graph_reset + graph_run to re-run a finished graph); any other fireable status launches normally. If the graph's last run was against a queue, the resume targets that same queue (its pending members, in queue order) instead of the graph's own bound specs. The schedule always clears after firing (one-shot). Omit both `at` and `quota_reset_message` to cancel any pending autorun instead of scheduling one — valid regardless of the graph's current status, and a no-op (not an error) if nothing was scheduled. After a quota failure, prefer `quota_reset_message` (the raw CLI text, e.g. \"resets 1pm (America/Bogota)\") over computing `at` yourself — the engine parses the stated local time/timezone and converts it deterministically, avoiding scheduling errors from doing that arithmetic by hand."
    )]
    pub(crate) async fn graph_schedule_autorun(
        &self,
        Parameters(params): Parameters<GraphScheduleAutorunParams>,
    ) -> Result<CallToolResult, McpError> {
        self.schedule_autorun_impl(params.graph_id, params.at, params.quota_reset_message)
            .await
    }

    #[tool(
        name = "loop_schedule_autorun",
        description = "Deprecated: renamed graph_schedule_autorun in 3.0.0. Same parameters and \
        behaviour as graph_schedule_autorun; `loop_id` is still accepted as the graph id. Hidden \
        from tool listings; call graph_schedule_autorun instead."
    )]
    pub(crate) async fn loop_schedule_autorun(
        &self,
        Parameters(params): Parameters<LoopScheduleAutorunParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .schedule_autorun_impl(params.graph_id, params.at, params.quota_reset_message)
            .await?;
        Ok(with_deprecated_note(
            result,
            "loop_schedule_autorun was renamed graph_schedule_autorun in 3.0.0",
        ))
    }

    /// Schedule a one-shot deferred resume for a graph that is (or will be)
    /// `Paused` — the counterpart to `graph_schedule_autorun` for a graph a
    /// user paused on purpose (e.g. to stop burning quota right now) rather
    /// than one that failed. When the scheduler reaches `at` and the graph is
    /// still `Paused`, it fires the exact `graph_continue` action requested
    /// here — preserving the paused cursor/context — instead of
    /// `autorun_at`'s reset-and-relaunch. If the graph is no longer `Paused`
    /// by then (resumed manually, failed, completed, running), the schedule
    /// is cleared without firing; it never double-runs. Always one-shot.
    #[tool(
        name = "graph_schedule_continue",
        description = "Schedule a one-shot deferred graph_continue for a paused graph at a future ISO 8601 time, or cancel a pending one. When the scheduler reaches that time and the graph is still `paused`, it fires graph_continue with `action` (retry_current_node by default, or skip_next_spec) — resuming with the paused cursor/context intact, never resetting or relaunching. If the graph is no longer paused by then (already continued manually, failed, completed, running), the schedule is cleared without firing. The schedule always clears after firing (one-shot). Omit `at` (or pass null) to cancel any pending auto-continue instead of scheduling one — a no-op (not an error) if nothing was scheduled. Use this instead of graph_schedule_autorun to defer resuming a graph you paused on purpose, e.g. to stop burning quota now and pick back up automatically at a later time."
    )]
    pub(crate) async fn graph_schedule_continue(
        &self,
        Parameters(GraphScheduleContinueParams {
            graph_id: raw_graph_id,
            at,
            action,
        }): Parameters<GraphScheduleContinueParams>,
    ) -> Result<CallToolResult, McpError> {
        let graph_id = match resolve_prefix_or_error(
            &self.db,
            raw_graph_id.trim(),
            Database::resolve_graph_id_by_prefix,
            "graph",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let Some(existing) = self
            .db
            .get_graph(&graph_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Ok(error_result(&format!(
                "No graph found with ID '{}'",
                graph_id
            )));
        };

        let Some(at) = at else {
            let previously_scheduled = existing.auto_continue_at;
            self.db
                .clear_graph_auto_continue(&graph_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            return Ok(success_result(&match previously_scheduled {
                Some(previous_at) => format!(
                    "Graph '{}' auto-continue scheduled for {} was cancelled.",
                    graph_id,
                    previous_at.to_rfc3339()
                ),
                None => format!(
                    "Graph '{}' had no pending auto-continue to cancel.",
                    graph_id
                ),
            }));
        };

        let action = action.unwrap_or_else(|| "retry_current_node".to_string());
        if action != "retry_current_node" && action != "skip_next_spec" {
            return Ok(error_result(
                "graph_schedule_continue action must be retry_current_node or skip_next_spec.",
            ));
        }

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
            .schedule_graph_auto_continue(&graph_id, at, Some(&action))
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.scheduler_notify.notify_one();

        Ok(success_result(&format!(
            "Graph '{}' scheduled to auto-continue ({}) at {}",
            graph_id,
            action,
            at.to_rfc3339()
        )))
    }

    #[tool(
        name = "graph_pause",
        description = "Pause a running graph. By default, waits for the current node to finish before pausing. Set `interrupt: true` to immediately stop the running node (marked as interrupted, not failed)."
    )]
    pub(crate) async fn graph_pause(
        &self,
        Parameters(params): Parameters<GraphPauseParams>,
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
        let interrupt = params.interrupt.unwrap_or(false);
        let paused = self
            .graph_engine
            .request_pause(&graph_id, interrupt)
            .map_err(internal_error)?;
        if paused {
            if interrupt {
                Ok(success_result(&format!(
                    "Graph '{}' paused (running node interrupted).",
                    graph_id
                )))
            } else {
                Ok(success_result(&format!(
                    "Graph '{}' will pause after the current node finishes.",
                    graph_id
                )))
            }
        } else {
            Ok(error_result(&format!(
                "Graph '{}' is not running or does not exist.",
                graph_id
            )))
        }
    }

    #[tool(
        name = "graph_archive",
        description = "Archive a graph: it leaves the main graph_list/sidebar view but its row, specs, and full run history are untouched (never deleted, never moved to another table) and it can be restored with graph_restore at any time. Refuses a `running` graph — pause it first. Permanent deletion is a separate, deliberate act on an already-archived graph, not something F4/this tool does."
    )]
    pub(crate) async fn graph_archive(
        &self,
        Parameters(params): Parameters<GraphArchiveParams>,
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
        match self.db.archive_graph(&graph_id).map_err(internal_error)? {
            ArchiveGraphOutcome::Archived => Ok(success_result(&format!(
                "Graph '{}' archived. Its specs and run history are intact; restore it with graph_restore.",
                graph_id
            ))),
            ArchiveGraphOutcome::AlreadyArchived => Ok(error_result(&format!(
                "Graph '{}' is already archived.",
                graph_id
            ))),
            ArchiveGraphOutcome::Running => Ok(error_result(&format!(
                "Graph '{}' is running — pause it before archiving.",
                graph_id
            ))),
            ArchiveGraphOutcome::NotFound => Ok(error_result(&format!(
                "Graph '{}' not found.",
                graph_id
            ))),
        }
    }

    #[tool(
        name = "graph_restore",
        description = "Restore an archived graph back to the main graph_list/sidebar view. The graph's row, specs, and run history were never touched by archiving, so this is a plain flag flip."
    )]
    pub(crate) async fn graph_restore(
        &self,
        Parameters(params): Parameters<GraphRestoreParams>,
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
        let restored = self.db.restore_graph(&graph_id).map_err(internal_error)?;
        if restored {
            Ok(success_result(&format!(
                "Graph '{}' restored to the main list.",
                graph_id
            )))
        } else {
            Ok(error_result(&format!(
                "Graph '{}' not found or not archived.",
                graph_id
            )))
        }
    }

    #[tool(
        name = "graph_continue",
        description = "Continue a paused graph by retrying the current node or skipping to the next spec."
    )]
    pub(crate) async fn graph_continue(
        &self,
        Parameters(params): Parameters<GraphContinueParams>,
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
        let lp = match self.db.get_graph(&graph_id) {
            Ok(Some(w)) => w,
            Ok(None) => return Ok(error_result(&format!("Graph '{}' not found.", graph_id))),
            Err(e) => return Err(internal_error(e.to_string())),
        };
        if lp.status != GraphStatus::Paused {
            return Ok(error_result(&format!(
                "Graph '{}' is not paused.",
                graph_id
            )));
        }

        match params.action.trim() {
            "retry_current_node" => {
                handle_retry_current_node(&self.db, &graph_id)?;
            }
            "skip_next_spec" => handle_skip_next_spec(&self.db, &graph_id)?,
            _ => {
                return Ok(error_result(
                    "graph_continue action must be retry_current_node or skip_next_spec.",
                ));
            }
        }

        Arc::clone(&self.graph_engine).resume_background(graph_id.clone());

        Ok(success_result(&format!(
            "Graph '{}' resumed with action '{}'.",
            graph_id, params.action
        )))
    }

    #[tool(
        name = "graph_complete_node",
        description = "Mark the active run for a graph node as pass or fail and attach its output."
    )]
    pub(crate) async fn graph_complete_node(
        &self,
        Parameters(params): Parameters<GraphCompleteNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.complete_node_impl(params).await
    }

    #[tool(
        name = "loop_complete_node",
        description = "Deprecated: renamed graph_complete_node in 3.0.0. Same parameters and \
        behaviour as graph_complete_node — kept working for the whole 3.x line so a prompt \
        still written against the 2.x name doesn't end unreported. Hidden from tool listings; \
        call graph_complete_node instead."
    )]
    pub(crate) async fn loop_complete_node(
        &self,
        Parameters(params): Parameters<GraphCompleteNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.complete_node_impl(params).await?;
        Ok(with_deprecated_note(
            result,
            "loop_complete_node was renamed graph_complete_node in 3.0.0",
        ))
    }

    #[tool(
        name = "graph_report_blocker",
        description = "Pause a graph because the active node is blocked and needs human intervention."
    )]
    pub(crate) async fn graph_report_blocker(
        &self,
        Parameters(params): Parameters<GraphReportBlockerParams>,
    ) -> Result<CallToolResult, McpError> {
        self.report_blocker_impl(params).await
    }

    #[tool(
        name = "loop_report_blocker",
        description = "Deprecated: renamed graph_report_blocker in 3.0.0. Same parameters and \
        behaviour as graph_report_blocker. Hidden from tool listings; call graph_report_blocker \
        instead."
    )]
    pub(crate) async fn loop_report_blocker(
        &self,
        Parameters(params): Parameters<GraphReportBlockerParams>,
    ) -> Result<CallToolResult, McpError> {
        let result = self.report_blocker_impl(params).await?;
        Ok(with_deprecated_note(
            result,
            "loop_report_blocker was renamed graph_report_blocker in 3.0.0",
        ))
    }
}

impl TaskTriggerHandler {
    pub(crate) async fn schedule_autorun_impl(
        &self,
        raw_graph_id: String,
        at: Option<String>,
        quota_reset_message: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        let graph_id = match resolve_prefix_or_error(
            &self.db,
            raw_graph_id.trim(),
            Database::resolve_graph_id_by_prefix,
            "graph",
        ) {
            Ok(id) => id,
            Err(e) => return Ok(e),
        };
        let Some(existing) = self
            .db
            .get_graph(&graph_id)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?
        else {
            return Ok(error_result(&format!(
                "No graph found with ID '{}'",
                graph_id
            )));
        };

        if at.is_some() && quota_reset_message.is_some() {
            return Ok(error_result(
                "Pass either `at` or `quota_reset_message`, not both.",
            ));
        }

        // B — deterministic, engine-side conversion: a raw CLI quota message
        // ("resets 1pm (America/Bogota)") is parsed and converted to UTC
        // here, in code, rather than trusting the calling model's own
        // local-time arithmetic (the source of a +2h scheduling error on
        // 2026-07-24 — see `domain::quota_reset`).
        if let Some(message) = quota_reset_message {
            let at = match crate::domain::quota_reset::parse_quota_reset_instant(
                &message,
                chrono::Utc::now(),
            ) {
                Ok(at) => at,
                Err(e) => {
                    return Ok(error_result(&format!(
                        "Could not compute a reset instant from '{}': {}",
                        message, e
                    )));
                }
            };
            return self.commit_graph_autorun(&graph_id, at);
        }

        let Some(at) = at else {
            let previously_scheduled = existing.autorun_at;
            self.db
                .clear_graph_autorun(&graph_id)
                .map_err(|e| McpError::internal_error(e.to_string(), None))?;
            return Ok(success_result(&match previously_scheduled {
                Some(previous_at) => format!(
                    "Graph '{}' autorun scheduled for {} was cancelled.",
                    graph_id,
                    previous_at.to_rfc3339()
                ),
                None => format!("Graph '{}' had no pending autorun to cancel.", graph_id),
            }));
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

        self.commit_graph_autorun(&graph_id, at)
    }

    /// Shared tail of `graph_schedule_autorun`'s two entry points (`at` and
    /// `quota_reset_message`): persist the schedule (idempotent — a retry
    /// with the same `graph_id`/`at` after a lost ack just re-overwrites the
    /// same single-column schedule, never creating a second pending
    /// schedule) and wake the scheduler.
    pub(crate) fn commit_graph_autorun(
        &self,
        graph_id: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<CallToolResult, McpError> {
        self.db
            .schedule_graph_autorun(graph_id, at)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.scheduler_notify.notify_one();

        Ok(success_result(&format!(
            "Graph '{}' scheduled to autorun at {}",
            graph_id,
            at.to_rfc3339()
        )))
    }

    /// Shared body of `graph_complete_node` and the deprecated
    /// `loop_complete_node` alias (CB68) — identical params, identical
    /// behaviour; only the alias's response gets a `deprecated` note.
    pub(crate) async fn complete_node_impl(
        &self,
        params: GraphCompleteNodeParams,
    ) -> Result<CallToolResult, McpError> {
        let status = match params.status.trim() {
            "pass" => Some(GraphRunStatus::Pass),
            "fail" => Some(GraphRunStatus::Fail),
            _ => None,
        };
        let Some(status) = status else {
            return Ok(error_result(
                "graph_complete_node status must be pass or fail.",
            ));
        };
        let run = match resolve_reported_run(&self.db, &params.run_id, &params.node_id)? {
            Ok(run) => run,
            Err(result) => return Ok(result),
        };

        self.db
            .update_graph_run_result(
                &run.id,
                status,
                Some(&serde_json::json!({
                    "reported_output": params.output,
                    "summary": params.summary,
                })),
                Some(chrono::Utc::now()),
            )
            .map_err(internal_error)?;

        Ok(success_result("Graph node result recorded."))
    }

    pub(crate) async fn report_blocker_impl(
        &self,
        params: GraphReportBlockerParams,
    ) -> Result<CallToolResult, McpError> {
        let run = match resolve_reported_run(&self.db, &params.run_id, &params.node_id)? {
            Ok(run) => run,
            Err(result) => return Ok(result),
        };

        self.db
            .update_graph_run_result(
                &run.id,
                GraphRunStatus::Fail,
                Some(&serde_json::json!({
                    "blocker": params.description,
                })),
                Some(chrono::Utc::now()),
            )
            .map_err(internal_error)?;
        self.db
            .update_graph_status(&run.graph_id, GraphStatus::Paused, None, None)
            .map_err(internal_error)?;
        self.graph_engine
            .notify_blocked(&run.graph_id, &params.description)
            .map_err(internal_error)?;
        // Resolve the human-readable node name for {{node}} before firing,
        // falling back to the raw node id when the node row is gone.
        let node_name = self
            .db
            .get_graph_node(&run.node_id)
            .ok()
            .flatten()
            .map(|node| node.name)
            .unwrap_or_else(|| run.node_id.clone());
        self.graph_engine
            .fire_on_blocked_hooks(&run.graph_id, &params.description, Some(&node_name))
            .await;

        Ok(success_result("Graph blocker recorded and graph paused."))
    }
}

/// Core logic for `graph_reset`, factored out of the tool method so it only
/// needs `&Database` (no engine/executor) and can be unit-tested directly.
///
/// Delegates the actual state transition to [`Database::reset_graph`] — the
/// scheduler's auto-reset-and-resume of a `failed` graph on autorun goes
/// through the same function, so this tool and that background path can
/// never drift apart.
pub(crate) fn perform_graph_reset(
    db: &Database,
    graph_id: &str,
    specs: Option<&[String]>,
) -> Result<CallToolResult, McpError> {
    match db.reset_graph(graph_id, specs).map_err(internal_error)? {
        GraphResetOutcome::NotFound => Ok(error_result(&format!("Graph '{graph_id}' not found."))),
        GraphResetOutcome::InFlight {
            run_id,
            node_id,
            started_at,
        } => {
            let node_name = node_name_for_error(db, &node_id).map_err(internal_error)?;
            Ok(error_result(&in_flight_run_error(
                graph_id, &run_id, &node_name, started_at,
            )))
        }
        GraphResetOutcome::InvalidSpec(id) => Ok(error_result(&format!(
            "Spec '{id}' does not belong to graph '{graph_id}'."
        ))),
        GraphResetOutcome::Reset {
            spec_count,
            skipped_count,
        } => {
            let skipped_note = if skipped_count > 0 {
                format!(" {skipped_count} skipped spec(s) preserved.")
            } else {
                String::new()
            };
            Ok(success_result(&format!(
                "Graph '{graph_id}' reset to pending; {spec_count} spec(s) reset.{skipped_note}"
            )))
        }
    }
}
