//! Finalization of a graph dispatch: the queue-run tail guards and the
//! successful-completion path.

use super::*;

impl GraphEngine {
    pub(crate) fn warn_incomplete_queue_members(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<bool> {
        let Some(queue_id) = queue_id else {
            return Ok(false);
        };
        if !self.db.queue_has_incomplete_members(queue_id)? {
            return Ok(false);
        }
        tracing::warn!(
            "Graph '{}' queue run against '{}' found no pending member to pick, but the \
             queue still has incomplete (non completed/skipped) member(s); leaving the \
             graph as-is rather than marking it completed. Reset the stuck member(s) via \
             graph_control reset to resume.",
            graph_id,
            queue_id
        );
        Ok(true)
    }

    /// Fail the graph when a queue run leaves failed members behind.
    /// Returns `true` when the graph was failed and the dispatch must stop.
    pub(crate) async fn fail_dispatch_on_queue_failures(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: Option<&str>,
        completed_specs: &[(String, String)],
    ) -> Result<bool> {
        let Some(queue_id) = queue_id else {
            return Ok(false);
        };
        let failed = self.db.queue_failed_members(queue_id)?;
        if failed.is_empty() {
            return Ok(false);
        }
        let blocker = failed
            .iter()
            .map(|(_, name, summary)| {
                format!(
                    "{}: {}",
                    name,
                    summary.clone().unwrap_or_else(|| "failed".to_string())
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let blocker = format!("Queue run failed: {blocker}");
        self.fail_graph(
            ctx.graph_id,
            GraphFailure {
                dispatch_started_at: Some(ctx.claimed_at),
                spec_name: None,
                spec_id: None,
                summary: &blocker,
                completed_specs,
                daemon_trigger: ctx.daemon_trigger,
            },
        )
        .await?;
        Ok(true)
    }

    /// FR3: a bound-spec run reports completion only when no bound spec is
    /// still pending. Whatever the live advance left behind — a spec bound in
    /// the window between its last read and this finish, a resume that settled
    /// early, a skipped/limited spec left pending — is named here and the graph
    /// failed instead, so the operator gets a signal. `on_completed` hooks do
    /// not fire, because completion never happened.
    ///
    /// Returns `true` when the graph was failed and `finish_dispatch` must
    /// stop. The queue path is untouched: it has its own
    /// incomplete/failed-member guards.
    async fn fail_on_leftover_bound_specs(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: Option<&str>,
        completed_specs: &[(String, String)],
    ) -> Result<bool> {
        if queue_id.is_some() {
            return Ok(false);
        }
        let pending: Vec<String> = self
            .db
            .list_graph_specs(ctx.graph_id)?
            .into_iter()
            .filter(|spec| {
                matches!(
                    spec.status,
                    GraphSpecStatus::Pending
                        | GraphSpecStatus::Running
                        | GraphSpecStatus::Interrupted
                )
            })
            .map(|spec| spec.id)
            .collect();
        if pending.is_empty() {
            return Ok(false);
        }
        let blocker = format!(
            "Graph '{}' cannot complete: {} bound spec(s) still pending ({}). They were not \
             run — a pending spec is never reported as a completed graph. Resume with \
             graph_control continue to run them.",
            ctx.lp.name,
            pending.len(),
            pending.join(", ")
        );
        self.fail_graph(
            ctx.graph_id,
            GraphFailure {
                dispatch_started_at: Some(ctx.claimed_at),
                spec_name: None,
                spec_id: None,
                summary: &blocker,
                completed_specs,
                daemon_trigger: ctx.daemon_trigger,
            },
        )
        .await?;
        Ok(true)
    }

    pub(crate) async fn finish_dispatch(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: Option<&str>,
        sandbox: Option<Sandbox>,
        completed_specs: &[(String, String)],
        idle: bool,
        agent_ran: bool,
    ) -> Result<()> {
        if self
            .fail_on_leftover_bound_specs(ctx, queue_id, completed_specs)
            .await?
        {
            // FR3: a pending bound spec is named in a `graph_failed` event;
            // nothing in this dispatch reports completion. `fail_graph` already
            // kept the sandbox, as it does for every failure.
            return Ok(());
        }
        self.db.update_graph_status(
            ctx.graph_id,
            GraphStatus::Completed,
            None,
            Some(chrono::Utc::now()),
        )?;
        let (done, total) = self.spec_progress(ctx.graph_id, queue_id)?;
        // FR5: a tick run of a spec-less graph names how the run ended on
        // the `graph_completed` event — `idle` when its check found nothing
        // to do, `worked` when it reached and ran an agent node. Every
        // other dispatch (including a manual run of the same graph) keeps
        // the payload-less event exactly as before.
        let outcome = if ctx.idle_run {
            if idle {
                Some("idle")
            } else if agent_ran {
                Some("worked")
            } else {
                None
            }
        } else {
            None
        };
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: ctx.graph_id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: None,
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::GraphCompleted,
                message: format!("Completed ({done}/{total} specs)."),
                payload: outcome.map(|outcome| serde_json::json!({"outcome": outcome})),
            },
        );
        let executed_any_spec = !completed_specs.is_empty();
        let hook_launched = executed_any_spec
            && ctx
                .lp
                .hooks
                .get(&GraphHookEvent::OnCompleted)
                .is_some_and(|hooks| !hooks.is_empty());
        self.notification_service.notify_graph_finished(
            &ctx.lp.name,
            GraphFinishOutcome::Completed {
                done,
                total,
                hook_launched,
            },
        );
        if executed_any_spec {
            self.fire_completion_hook(ctx.lp, ctx.workdir, completed_specs)
                .await;
        }
        self.teardown_sandbox_after_final_status(sandbox, "completed".into())
            .await;
        if let Some(trigger) = ctx.daemon_trigger {
            let _ = crate::graph_engine::graph_run_log::append_graph_run_block(
                &self.db,
                ctx.graph_id,
                trigger,
                ctx.claimed_at,
            );
        }
        Ok(())
    }
}
