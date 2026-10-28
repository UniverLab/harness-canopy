//! MCP handler tools split out of `daemon::handler` (run lifecycle tools (reset, schedule, pause, complete, blockers)).
//! Moved verbatim; tool names, schemas, and behaviour unchanged.

use crate::daemon::handler::{OptionalExtension, TaskTriggerHandler};
use crate::daemon::handler_formatting::internal_error;
use crate::daemon::handler_run_directive::{
    limit_reset_note, not_continuable_refusal, reset_execution_limit, restart_at_node,
    retry_restart_target,
};
use crate::daemon::handler_run_exec::{
    directed_run_report, find_in_flight_run, handle_retry_current_node, handle_skip_next_spec,
    in_flight_run_error, node_name_for_error, resolve_directed_spec,
};
use crate::daemon::handler_validate::resolve_prefix_or_error;
use crate::daemon::helpers::{error_result, success_result, with_deprecated_note};
use crate::daemon::params::*;
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::graphs::{ArchiveGraphOutcome, GraphResetOutcome, GraphStatus};
use crate::shared::sync_identity::graph_identity;
use axum::http::request::Parts;
use rmcp::model::CallToolResult;
use rmcp::tool;
use rmcp::tool_router;
use rmcp::ErrorData as McpError;
use std::sync::Arc;

#[tool_router(router = tool_router_run_lifecycle, vis = "pub(crate)")]
impl TaskTriggerHandler {
    /// Reset a `completed`/`failed` (or otherwise stalled) graph back to
    /// `pending` so it can be relaunched via `graph_control`, which otherwise
    /// refuses to resume a completed or failed graph.
    ///
    /// This is also the exact state transition the scheduler uses to
    /// auto-reset a `failed` graph when its `graph_control` fires —
    /// both paths call [`crate::db::Database::reset_graph`], so a human
    /// calling this tool and the scheduler resuming a failed graph on its own
    /// can never diverge in behavior.
    pub(crate) async fn graph_reset_impl(
        &self,
        params: GraphResetParams,
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
        perform_graph_reset(
            &self.db,
            &graph_id,
            params.specs.as_deref(),
            params.clear_notes,
        )
    }

    /// Schedule a one-shot future resume for a graph (e.g. a graph that failed
    /// on a quota can reschedule itself at the exact reset time instead of
    /// relying on a blindly polling cron). The scheduler fires it once the
    /// time is reached and the graph is fireable, then clears the schedule.
    ///
    /// Firing on a `failed` graph performs an explicit, logged
    /// auto-reset-and-resume: it runs the same transition as `graph_control`
    /// (see [`crate::db::Database::reset_graph`]) and then resumes execution —
    /// this is the sanctioned way a quota-failed graph revives itself
    /// unattended. Firing on a `completed` graph does not re-run it: that is
    /// a human decision, made via `graph_control` + `graph_control`.
    pub(crate) async fn graph_schedule_autorun_impl(
        &self,
        params: GraphScheduleAutorunParams,
    ) -> Result<CallToolResult, McpError> {
        self.schedule_autorun_impl(params.graph_id, params.at, params.quota_reset_message)
            .await
    }

    /// Schedule a one-shot deferred resume for a graph that is (or will be)
    /// `Paused` — the counterpart to `graph_control` for a graph a
    /// user paused on purpose (e.g. to stop burning quota right now) rather
    /// than one that failed. When the scheduler reaches `at` and the graph is
    /// still `Paused`, it fires the exact `graph_control` action requested
    /// here — preserving the paused cursor/context — instead of
    /// `autorun_at`'s reset-and-relaunch. If the graph is no longer `Paused`
    /// by then (resumed manually, failed, completed, running), the schedule
    /// is cleared without firing; it never double-runs. Always one-shot.
    pub(crate) async fn graph_schedule_continue_impl(
        &self,
        GraphScheduleContinueParams {
            graph_id: raw_graph_id,
            at,
            mode,
        }: GraphScheduleContinueParams,
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

        let mode = mode.unwrap_or_else(|| "retry_current_node".to_string());
        if mode != "retry_current_node" && mode != "skip_next_spec" {
            return Ok(error_result(
                "The schedule_continue mode must be retry_current_node or skip_next_spec.",
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
            .schedule_graph_auto_continue(&graph_id, at, Some(&mode))
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;

        self.scheduler_notify.notify_one();

        Ok(success_result(&format!(
            "Graph '{}' scheduled to auto-continue ({}) at {}",
            graph_id,
            mode,
            at.to_rfc3339()
        )))
    }

    pub(crate) async fn graph_pause_impl(
        &self,
        params: GraphPauseParams,
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

    pub(crate) async fn graph_archive_impl(
        &self,
        params: GraphArchiveParams,
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
                "Graph '{}' archived. Its specs and run history are intact; restore it with graph_edit.",
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

    pub(crate) async fn graph_restore_impl(
        &self,
        params: GraphRestoreParams,
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

    pub(crate) async fn graph_continue_impl(
        &self,
        params: GraphContinueParams,
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
        if params.mode.trim() == "run_from_node" {
            return self
                .graph_continue_run_from_node(graph_id, lp, params.node_id, params.spec_id)
                .await;
        }
        // FR6: a failed graph takes the same two actions as a paused one.
        if !matches!(lp.status, GraphStatus::Paused | GraphStatus::Failed) {
            return Ok(error_result(&not_continuable_refusal(&graph_id, lp.status)));
        }

        let limit = match params.mode.trim() {
            "retry_current_node" => {
                // FR6/FR7: a failed graph, or a pending spec with a recorded
                // current node, restarts as a directed run at that node.
                if let Some((spec, node_id)) = retry_restart_target(&self.db, &lp)? {
                    return restart_at_node(
                        &self.graph_engine,
                        &self.db,
                        &graph_id,
                        &spec,
                        node_id,
                    );
                }
                // FR3: same never-run idea-graph refusal as run_from_node.
                if let Some(refusal) = self
                    .graph_engine
                    .idea_resume_ready(&lp)
                    .map_err(internal_error)?
                {
                    return Ok(error_result(&refusal));
                }
                let spec = resolve_directed_spec(&self.db, &graph_id, None).ok();
                handle_retry_current_node(&self.db, &graph_id)?;
                // FR1: the directive clears the spec's execution limit before the run.
                match spec {
                    Some(spec) => reset_execution_limit(&self.db, &spec.id, &spec.name)?,
                    None => String::new(),
                }
            }
            "skip_next_spec" => {
                handle_skip_next_spec(&self.db, &graph_id)?;
                String::new()
            }
            _ => {
                return Ok(error_result(
                    "The continue mode must be retry_current_node, skip_next_spec or run_from_node.",
                ));
            }
        };

        Arc::clone(&self.graph_engine).resume_background(graph_id.clone(), None);

        Ok(success_result(&format!(
            "Graph '{}' resumed with mode '{}'.{limit}",
            graph_id, params.mode
        )))
    }

    /// `graph_control { run_from_node }`: run `spec` starting at a chosen node,
    /// reusing earlier nodes' recorded outputs. Accepts any graph with no active
    /// run (paused, or completed/failed) — a directed run is the operator's tool
    /// for exactly those states.
    pub(crate) async fn graph_continue_run_from_node(
        &self,
        graph_id: String,
        lp: crate::domain::graphs::Graph,
        node_id: Option<String>,
        spec_id: Option<String>,
    ) -> Result<CallToolResult, McpError> {
        // FR2: "the graph has no active run" is checked against the run rows,
        // not the graph's own status — a blocker can flip a graph to `paused`
        // while a sibling node keeps executing, and a live run must refuse
        // (and be named) whatever the status column says.
        if let Some(run) = find_in_flight_run(&self.db, &graph_id).map_err(internal_error)? {
            let node_name = node_name_for_error(&self.db, &run.node_id).map_err(internal_error)?;
            return Ok(error_result(&in_flight_run_error(
                &graph_id,
                &run.id,
                &node_name,
                run.started_at,
            )));
        }
        if matches!(lp.status, GraphStatus::Running | GraphStatus::Pausing) {
            return Ok(error_result(&format!(
                "Graph '{}' is running; pause it before a directed run.",
                graph_id
            )));
        }
        let Some(node_id) = node_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        else {
            return Ok(error_result(
                "graph_control run_from_node requires a non-empty node_id.",
            ));
        };
        // FR3: an idea graph that never ran has no stored idea to resume —
        // refuse it here, naming the way to start one, instead of letting
        // `resolve_directed_spec` fail with a bare "no spec to target".
        if let Some(refusal) = self
            .graph_engine
            .idea_resume_ready(&lp)
            .map_err(internal_error)?
        {
            return Ok(error_result(&refusal));
        }
        let spec = match resolve_directed_spec(&self.db, &graph_id, spec_id.as_deref()) {
            Ok(spec) => spec,
            Err(message) => return Ok(error_result(&message)),
        };
        // FR2 accepts a spec the run is `paused` on — which this schema
        // stores as spec status `running` with a paused graph and no active
        // run (the exact state `graph_control` mid-spec leaves behind). Only
        // `skipped` is outside the accepted states; an active run was already
        // refused above.
        if spec.status == crate::domain::graphs::GraphSpecStatus::Skipped {
            return Ok(error_result(&format!(
                "Graph '{graph_id}': spec '{}' is 'skipped', which a directed run does not accept.",
                spec.name
            )));
        }
        let plan = match self
            .graph_engine
            .plan_directed_run(&graph_id, &spec.id, &node_id)
        {
            Ok(plan) => plan,
            Err(message) => return Ok(error_result(&message)),
        };
        // FR1: the engine zeroes the count when the directed run starts.
        let limit = limit_reset_note(&spec.name);
        Arc::clone(&self.graph_engine).resume_background(
            graph_id.clone(),
            Some(crate::graph_engine::DirectedStart {
                spec_id: spec.id,
                node_arg: node_id,
            }),
        );
        Ok(success_result(&format!(
            "Graph '{}' resumed.{limit}{}",
            graph_id,
            directed_run_report(Some(&plan))
        )))
    }

    #[tool(
        name = "graph_complete_node",
        description = "Mark the active run for a graph node as pass or fail and attach its output. run_id/node_id may be omitted inside a node run: they resolve from the CANOPY_NODE_RUN_ID/CANOPY_NODE_ID environment the engine sets on every agent process."
    )]
    pub(crate) async fn graph_complete_node(
        &self,
        Parameters(params): Parameters<GraphCompleteNodeParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.complete_node_impl(params, graph_identity(parts.as_ref()))
            .await
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
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .complete_node_impl(params, graph_identity(parts.as_ref()))
            .await?;
        Ok(with_deprecated_note(
            result,
            "loop_complete_node was renamed graph_complete_node in 3.0.0",
        ))
    }

    #[tool(
        name = "graph_report_blocker",
        description = "Pause a graph because the active node is blocked and needs human intervention. run_id/node_id may be omitted inside a node run: they resolve from the CANOPY_NODE_RUN_ID/CANOPY_NODE_ID environment the engine sets on every agent process."
    )]
    pub(crate) async fn graph_report_blocker(
        &self,
        Parameters(params): Parameters<GraphReportBlockerParams>,
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        self.report_blocker_impl(params, graph_identity(parts.as_ref()))
            .await
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
        OptionalExtension(parts): OptionalExtension<Parts>,
    ) -> Result<CallToolResult, McpError> {
        let result = self
            .report_blocker_impl(params, graph_identity(parts.as_ref()))
            .await?;
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

    /// Shared tail of `graph_control`'s two entry points (`at` and
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
}

/// Core logic for `graph_control`, factored out of the tool method so it only
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
    clear_notes: bool,
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
            // CM47: notes are keyed by spec, not run, so they survive a reset
            // by default. Only an explicit `clear_notes: true` removes them —
            // named specs when `specs` is given, the graph's bound specs plus
            // its active queue's members on a blanket reset.
            let notes_note = if clear_notes {
                let removed = match specs {
                    Some(ids) => {
                        let mut removed = 0;
                        for spec_id in ids {
                            removed += db.clear_spec_notes(spec_id).map_err(internal_error)?;
                        }
                        removed
                    }
                    None => db
                        .clear_spec_notes_for_graph(graph_id)
                        .map_err(internal_error)?,
                };
                format!(" {removed} note(s) cleared.")
            } else {
                String::new()
            };
            let skipped_note = if skipped_count > 0 {
                format!(" {skipped_count} skipped spec(s) preserved.")
            } else {
                String::new()
            };
            Ok(success_result(&format!(
                "Graph '{graph_id}' reset to pending; {spec_count} spec(s) reset.{skipped_note}{notes_note}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::DirectedPlan;

    #[test]
    fn directed_run_report_lists_start_skipped_and_reused() {
        let plan = DirectedPlan {
            spec_name: "S".into(),
            start_label: "node 'Review'".into(),
            skipped_nodes: vec!["Design".into(), "Build".into()],
            reused_outputs: vec![("Build".into(), "run-1".into())],
            start_head: Some("abc123".into()),
        };
        let text = directed_run_report(Some(&plan));
        assert!(text.contains("starts at node 'Review'"));
        assert!(text.contains("skipped Design, Build node(s)"));
        assert!(text.contains("Build (run-1)"));
        assert!(text.contains("iteration counters restarted at 1"));
        assert!(text.contains("abc123"));
    }
}
