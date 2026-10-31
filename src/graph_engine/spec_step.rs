use super::*;
use crate::db::connection::{is_busy_error, with_busy_retry};

impl GraphEngine {
    /// CB31: a wait-for-completion pause requested while this step was
    /// running. The step ran to completion and its verdict is recorded
    /// normally; flag its runs so a `graph_control` re-execution does
    /// not spend one of its iterations (the pause is not an attempt),
    /// then report paused and stop before the next step.
    pub(crate) async fn pause_after_step_if_requested(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &SpecRunState,
        iteration_value: usize,
    ) -> Result<Option<SpecExecutionOutcome>> {
        // A resume (`graph_control` continue/run) may resolve the pending
        // pause concurrently — cancelling it back to `Running`, or
        // completing it to `Paused` and proceeding itself. Only the winner
        // narrates: claim the transition first, and on loss follow whoever
        // won. Losing to a cancel must fall through and keep this dispatch
        // going — writing `graph_paused` here would end a `Running` graph's
        // dispatch with no live run left, stranding it.
        if self.is_pausing(&ctx.lp.id)? && self.claim_pause(&ctx.lp.id)? {
            let _ = persist_bookkeeping("mark_spec_node_runs_paused_through", || {
                self.db.mark_spec_node_runs_paused_through(
                    &ctx.spec.id,
                    &cursor_node_ids(&state.cursor, ctx.ensembles),
                    iteration_value as i64,
                )
            })?;
            activity::record(
                &self.db,
                activity::GraphEvent {
                    graph_id: &ctx.lp.id,
                    graph_name: &ctx.lp.name,
                    workdir: ctx.workdir,
                    spec_id: None,
                    node_id: None,
                    run_id: None,
                    kind: activity::GraphEventKind::GraphPaused,
                    message: "Paused.".to_string(),
                    payload: None,
                },
            );
            return Ok(Some(SpecExecutionOutcome::Paused));
        }
        if self.is_paused(&ctx.lp.id)? {
            // Either paused all along, or a concurrent resume completed the
            // pending pause and proceeds itself while this dispatch ends —
            // both without an event of their own here.
            return Ok(Some(SpecExecutionOutcome::Paused));
        }
        Ok(None)
    }

    /// Claim a pending pause, with busy retry. An exhausted busy warns and
    /// reports unclaimed — the dispatch keeps going exactly as when it loses
    /// the claim race to a concurrent resume, and the pause is retried on a
    /// later step instead of failing the graph.
    fn claim_pause(&self, graph_id: &str) -> Result<bool> {
        match with_busy_retry("complete_pause", || self.db.complete_pause(graph_id)) {
            Ok(claimed) => Ok(claimed),
            Err(error) if is_busy_error(&error) => {
                tracing::warn!("{error:#}; leaving the pause pending");
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn finish_advanced_spec(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &SpecRunState,
        execution: NodeExecution,
    ) -> Result<SpecExecutionOutcome> {
        let _ = persist_bookkeeping("update_graph_spec_status", || {
            self.db.update_graph_spec_status(
                &ctx.spec.id,
                GraphSpecStatus::Completed,
                None,
                Some(chrono::Utc::now()),
            )
        })?;
        self.record_spec_end_dirty(&ctx.spec.id, ctx.workdir).await;
        self.notify_spec_completed(ctx.lp, ctx.spec, ctx.queue_id)?;
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::SpecCompleted,
                message: match &state.spec_committed_head {
                    Some(head) => format!(
                        "Spec {} completed (committed {}).",
                        activity::spec_label(ctx.spec),
                        &head[..head.len().min(7)]
                    ),
                    None => format!("Spec {} completed.", activity::spec_label(ctx.spec)),
                },
                payload: None,
            },
        );
        Ok(SpecExecutionOutcome::Completed {
            summary: execution.summary,
        })
    }

    pub(crate) async fn apply_spec_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        step: NodeStepResult,
    ) -> Result<SpecLoopFlow> {
        // CB82 backstop choke point for every step type (single node,
        // parallel quorum, no-verdict join): while shutting down, stop
        // silently — never the `Error` edge in `apply_no_verdict_step`,
        // never any routed edge.
        if self.is_shutting_down() {
            return Ok(SpecLoopFlow::Done(SpecExecutionOutcome::Superseded));
        }
        if Self::is_no_verdict_execution(&step.execution) {
            return self.apply_no_verdict_step(ctx, state, &step).await;
        }
        let (selection, is_routed_router) = self.select_spec_step(ctx, &step)?;
        match &selection {
            Some(sel) => Self::log_edge_traversal(&step, sel),
            None => {
                tracing::info!(
                    run_id = step.run_id.as_deref().unwrap_or(""),
                    from_node = %step.from_node_id,
                    status = ?step.execution.status,
                    "no outgoing edge matched; spec terminating"
                );
                self.record_terminal_blocker_if_failed(ctx, &step)?;
            }
        }
        match selection.map(|sel| sel.cursor) {
            Some(next) => {
                self.advance_after_step(ctx, state, &step, next, is_routed_router);
                Ok(SpecLoopFlow::Continue)
            }
            None if step.execution.status == GraphRunStatus::Pass => Ok(SpecLoopFlow::Done(
                self.complete_terminal_spec(ctx, state, &step).await?,
            )),
            None => Ok(SpecLoopFlow::Done(
                self.fail_terminal_spec(ctx, &step).await?,
            )),
        }
    }

    pub(crate) fn is_no_verdict_execution(execution: &NodeExecution) -> bool {
        execution
            .output
            .get("no_verdict")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub(crate) fn log_edge_traversal(step: &NodeStepResult, sel: &StepSelection) {
        match &sel.cursor {
            SpecCursor::Node(target) => {
                tracing::info!(
                    run_id = step.run_id.as_deref().unwrap_or(""),
                    from_node = %step.from_node_id,
                    to_node = %target,
                    status = ?step.execution.status,
                    edge_condition = sel.edge_condition.as_str(),
                    route = sel.edge_condition.route_label().unwrap_or(""),
                    "edge traversed"
                );
            }
            SpecCursor::Ensemble(eid) => {
                tracing::info!(
                    run_id = step.run_id.as_deref().unwrap_or(""),
                    from_node = %step.from_node_id,
                    to_ensemble = %eid,
                    status = ?step.execution.status,
                    edge_condition = sel.edge_condition.as_str(),
                    "edge traversed to ensemble"
                );
            }
        }
    }

    pub(crate) async fn apply_no_verdict_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        step: &NodeStepResult,
    ) -> Result<SpecLoopFlow> {
        // FR4: this exit was not a review cycle — undo the loop-top
        // increment. The ceiling guard above still fires on every
        // re-entry, so a permanently dead crew cannot loop forever.
        let budget_key = budget_key(&state.cursor);
        if let Some(c) = state.iterations.get_mut(&budget_key) {
            *c = c.saturating_sub(1);
        }
        if let Some(sel) = select_next_step_with_condition(
            ctx.edges,
            ctx.ensembles,
            &step.from_node_id,
            &GraphEdgeCondition::Error,
        )? {
            Self::log_edge_traversal(step, &sel);
            let from_node_name = ctx
                .nodes_by_id
                .get(step.from_node_id.as_str())
                .map(|n| n.name.clone())
                .unwrap_or_default();
            state
                .node_outputs
                .insert(from_node_name.clone(), step.execution.output.clone());
            state.previous_node_name = Some(from_node_name);
            state.cursor = sel.cursor;
            return Ok(SpecLoopFlow::Continue);
        }
        // FR2: no error edge — pause with a blocker naming the
        // ensemble + member reasons. `Blocked` converts via
        // `block_graph`, which pauses the graph and fires on_blocked
        // hooks — the same mechanism as `graph_report_blocker`. Never
        // touches the C19 cross-run budget: infra never consumes it.
        let member_texts = Self::ensemble_member_failure_lines(&step.execution.output);
        let ensemble_name = Self::ensemble_name_for_join(ctx.ensembles, &step.from_node_id);
        let blocker = format!(
            "Ensemble '{}' produced no verdict ({} members, none filed a verdict): {}. The graph is paused; \
             this spec is marked interrupted (not failed). Three ways out: `graph_control continue` with mode \
             `retry_current_node` resumes at this ensemble — its current member list, with the nodes that \
             already passed in this spec left alone (rewire the members first with `graph_ensemble update` \
             if the crew itself is the problem); `graph_control continue` with mode `run_from_node` restarts \
             the spec at a node you choose, reusing what earlier nodes already produced; while `graph_control \
             reset` restarts the spec from Start.",
            ensemble_name,
            member_texts.len(),
            member_texts.join("; "),
        );
        // The blocker belongs to the join run: member rows share its
        // second-granularity `started_at` and `iteration`, so "last row"
        // does not reliably name the join.
        self.set_join_run_blocker(&ctx.spec.id, &step.from_node_id, &blocker)?;
        // FR1: a no-verdict block is transient — an operator can fix it by
        // retrying or rewiring the ensemble — so it lands `Interrupted` (its
        // cursor is the ensemble join and `is_resumable`), not `Failed`. The
        // C19 cross-run-budget and B12 iteration-budget blocks stay `Failed`.
        Ok(SpecLoopFlow::Done(
            self.finish_failed_spec(
                ctx.spec,
                ctx.workdir,
                blocker,
                GraphSpecStatus::Interrupted,
                true,
            )
            .await?,
        ))
    }

    pub(crate) fn ensemble_member_failure_lines(output: &Value) -> Vec<String> {
        output
            .get("members")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .map(|m| {
                        let node = m.get("node").and_then(Value::as_str).unwrap_or("?");
                        let platform = m.get("platform").and_then(Value::as_str).unwrap_or("?");
                        let reason = m
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("no reason recorded");
                        format!("{node} ({platform}): {reason}")
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub(crate) fn ensemble_name_for_join(
        ensembles: &[EnsembleDetails],
        from_node_id: &str,
    ) -> String {
        ensembles
            .iter()
            .find(|d| d.ensemble.join_node_id == from_node_id)
            .map(|d| d.ensemble.name.clone())
            .unwrap_or_else(|| from_node_id.to_string())
    }

    /// Stamp the no-verdict blocker onto the ensemble's join run, found by
    /// its node id rather than by recency.
    pub(crate) fn set_join_run_blocker(
        &self,
        spec_id: &str,
        join_node_id: &str,
        blocker: &str,
    ) -> Result<()> {
        let join = self
            .db
            .list_graph_runs_for_spec(spec_id)?
            .into_iter()
            .rfind(|run| run.node_id == join_node_id);
        if let Some(run) = join {
            self.set_run_blocker(
                &run.id,
                GraphRunStatus::Error,
                run.output.as_ref().unwrap_or(&serde_json::json!({})),
                blocker,
            )?;
        }
        Ok(())
    }

    pub(crate) fn select_spec_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        step: &NodeStepResult,
    ) -> Result<(Option<StepSelection>, bool)> {
        // A router that finished `Pass` routes by its chosen label
        // (`select_router_step`), never by Pass/Fail/Always
        // (`select_next_step`) — a router's verdict has no notion of
        // success/failure. A failed router (spawn failure/timeout, per
        // `execute_router_node`) falls through to `select_next_step`
        // exactly like any other node's fail edge.
        let is_routed_router = ctx
            .nodes_by_id
            .get(step.from_node_id.as_str())
            .is_some_and(|node| node.kind == GraphNodeKind::Router)
            && step.execution.status == GraphRunStatus::Pass;
        if is_routed_router {
            let route_label = step
                .execution
                .output
                .get("route")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Ok((
                select_router_step(ctx.edges, &step.from_node_id, route_label)?,
                true,
            ));
        }
        if step.had_infra_crash {
            // CM2: infra failures try `Error` edges first, falling back
            // to `Fail`/`Always` when no `Error` edge exists — additive,
            // no existing graph changes behavior.
            let error_selection = select_next_step_with_condition(
                ctx.edges,
                ctx.ensembles,
                &step.from_node_id,
                &GraphEdgeCondition::Error,
            )?;
            if error_selection.is_some() {
                return Ok((error_selection, false));
            }
        }
        Ok((
            select_next_step(
                ctx.edges,
                ctx.ensembles,
                &step.from_node_id,
                step.execution.status,
            )?,
            is_routed_router,
        ))
    }

    pub(crate) fn record_terminal_blocker_if_failed(
        &self,
        ctx: &SpecStepCtx<'_>,
        step: &NodeStepResult,
    ) -> Result<()> {
        if step.execution.status != GraphRunStatus::Fail {
            return Ok(());
        }
        let Some(run_id) = step.run_id.as_deref() else {
            return Ok(());
        };
        let node_name = ctx
            .nodes_by_id
            .get(step.from_node_id.as_str())
            .map_or(step.from_node_id.as_str(), |node| node.name.as_str());
        self.record_terminal_blocker(run_id, node_name, &step.execution)
    }

    pub(crate) fn advance_after_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        step: &NodeStepResult,
        next: SpecCursor,
        is_routed_router: bool,
    ) {
        // CB6: a routed router's output is control-flow metadata
        // (the route label), not data. That label already drove
        // edge selection above (`is_routed_router` /
        // `select_router_step`); propagating it as
        // `previous_output` would overwrite what the next node is
        // meant to see — the data the router read. A failed router
        // is not routed, so its failure payload still propagates.
        if !is_routed_router {
            let from_node_name = ctx
                .nodes_by_id
                .get(step.from_node_id.as_str())
                .map(|n| n.name.clone())
                .unwrap_or_default();
            state
                .node_outputs
                .insert(from_node_name.clone(), step.execution.output.clone());
            state.previous_node_name = Some(from_node_name);
        }
        state.cursor = next;
    }

    pub(crate) async fn complete_terminal_spec(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &SpecRunState,
        step: &NodeStepResult,
    ) -> Result<SpecExecutionOutcome> {
        self.db.update_graph_spec_status(
            &ctx.spec.id,
            GraphSpecStatus::Completed,
            None,
            Some(chrono::Utc::now()),
        )?;
        self.record_spec_end_dirty(&ctx.spec.id, ctx.workdir).await;
        self.notify_spec_completed(ctx.lp, ctx.spec, ctx.queue_id)?;
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::SpecCompleted,
                message: match &state.spec_committed_head {
                    Some(head) => format!(
                        "Spec {} completed (committed {}).",
                        activity::spec_label(ctx.spec),
                        &head[..head.len().min(7)]
                    ),
                    None => format!("Spec {} completed.", activity::spec_label(ctx.spec)),
                },
                payload: None,
            },
        );
        Ok(SpecExecutionOutcome::Completed {
            summary: step.execution.summary.clone(),
        })
    }

    pub(crate) async fn fail_terminal_spec(
        &self,
        ctx: &SpecStepCtx<'_>,
        step: &NodeStepResult,
    ) -> Result<SpecExecutionOutcome> {
        // C19: this dead-end Fail is what just ended the
        // attempt — check it directly for the infra markers
        // `is_infra_crash`/`agent_finished_execution` already
        // write, rather than re-deriving "did an agent actually
        // produce a verdict".
        let is_infra = execution_is_infra_failure(&step.execution.output);
        if let Some(blocker) =
            self.record_spec_attempt(ctx.spec, &step.execution.summary, is_infra)?
        {
            if let Some(terminal_run_id) = step.run_id.as_deref() {
                self.set_run_blocker(
                    terminal_run_id,
                    step.execution.status,
                    &step.execution.output,
                    &blocker,
                )?;
            }
            return self
                .finish_failed_spec(
                    ctx.spec,
                    ctx.workdir,
                    blocker,
                    GraphSpecStatus::Failed,
                    true,
                )
                .await;
        }
        self.finish_failed_spec(
            ctx.spec,
            ctx.workdir,
            step.execution.summary.clone(),
            GraphSpecStatus::Failed,
            false,
        )
        .await
    }

    pub(crate) async fn finish_failed_spec(
        &self,
        spec: &GraphSpec,
        workdir: &str,
        summary: String,
        final_status: GraphSpecStatus,
        blocked: bool,
    ) -> Result<SpecExecutionOutcome> {
        self.db
            .update_graph_spec_status(&spec.id, final_status, None, Some(chrono::Utc::now()))?;
        let (dirty, paths) = self.record_spec_end_dirty(&spec.id, workdir).await;
        let summary = match dirty_tree_note(dirty, &paths) {
            Some(note) => format!("{summary} ({note})"),
            None => summary,
        };
        if blocked {
            return Ok(SpecExecutionOutcome::Blocked(summary));
        }
        Ok(SpecExecutionOutcome::Failed(summary))
    }

    /// CM29: capture + persist the workdir's git-dirty state at the exact
    /// moment `spec_id`'s attempt ends — mirrors `spec_start_head`/
    /// `spec_start_dirty`'s capture at the other boundary. Never errors the
    /// caller: a failed capture or DB write only logs, the spec's own
    /// outcome is unaffected.
    pub(crate) fn reap_stale_cursor_runs(
        &self,
        cursor: &SpecCursor,
        ensembles: &[EnsembleDetails],
    ) -> Result<()> {
        for node_id in cursor_node_ids(cursor, ensembles) {
            if let Some(stale) = self.db.get_active_graph_run_for_node(&node_id)? {
                self.terminate_run(&stale, SUPERSEDE_REASON);
            }
        }
        Ok(())
    }

    pub(crate) async fn check_iteration_budget(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        budget_key: &str,
    ) -> Result<(usize, Option<SpecExecutionOutcome>)> {
        let iteration = state.iterations.entry(budget_key.to_owned()).or_insert(0);

        // CB31: an operator pause or interrupt is not a node attempt. If
        // the previous run at this cursor was ended by the operator —
        // recorded `Interrupted` (explicit `graph_control(interrupt: true)`),
        // or finalized with its own verdict while a wait-for-completion
        // pause was pending (`paused_through`) — the re-execution on
        // `graph_control` reuses the same iteration number instead of
        // consuming a fresh one. Only genuine node outcomes count against
        // `graphs.max_iterations_per_node`.
        let previous_was_operator_paused = self.db.last_spec_node_run_was_operator_paused(
            &ctx.spec.id,
            &cursor_node_ids(&state.cursor, ctx.ensembles),
        )?;
        if !previous_was_operator_paused {
            *iteration += 1;
        }
        let max_iterations = self.graph_settings().max_iterations_per_node;
        if *iteration > max_iterations {
            // B12: the process from the last execution at this node
            // (or any other node still running) must not survive the
            // spec failure — otherwise it burns quota, holds locks,
            // and could call graph_complete_node late with a stale
            // report. The process from the *previous* iteration is
            // the most likely survivor: the budget check fires before
            // any new execution starts, so the in-flight child is
            // always from a prior run at this node.
            for run in self
                .db
                .list_running_graph_runs(&ctx.lp.id)
                .unwrap_or_default()
            {
                self.terminate_run(&run, "iteration budget exhausted");
            }
            // CB7: blocker explicativo que nombra spec, nodo, intentos consumidos y techo
            let iteration_budget_blocker = format!(
                    "Spec '{}' agotó el techo de intentos para {}: {} intentos consumidos (techo: {}). \
                     El nodo completó sus iteraciones pero el spec no progresó; esto no es un fallo del \
                     nodo individual sino del spec en su conjunto.",
                    ctx.spec.name,
                    cursor_label(&state.cursor, ctx.ensembles),
                    max_iterations,
                    max_iterations,
                );
            // CB7: SIEMPRE escribir el blocker en el último run para que sea visible
            // en graph/graph sin necesidad de encadenar graph
            if let Some(run) = self.db.list_graph_runs_for_spec(&ctx.spec.id)?.last() {
                self.set_run_blocker(
                    &run.id,
                    run.status,
                    run.output.as_ref().unwrap_or(&serde_json::json!({})),
                    &iteration_budget_blocker,
                )?;
            }
            // C19: every bounce that grew this counter was a genuine
            // fail-edge routing decision, not an in-flight infra retry
            // (those never touch `iterations` — see its increment
            // above) — so reaching the per-node budget always reflects
            // repeated real verdicts, never pure infrastructure noise.
            if let Some(blocker) =
                self.record_spec_attempt(ctx.spec, &iteration_budget_blocker, false)?
            {
                // C19: this attempt also tripped the persisted cross-run
                // budget — overwrite the run blocker with the richer
                // cross-run text (it embeds the iteration message as its
                // "last failure"), so the single-call diagnosis still
                // names the cross-execution count, not just this run's.
                if let Some(run) = self.db.list_graph_runs_for_spec(&ctx.spec.id)?.last() {
                    self.set_run_blocker(
                        &run.id,
                        run.status,
                        run.output.as_ref().unwrap_or(&serde_json::json!({})),
                        &blocker,
                    )?;
                }
                self.db.update_graph_spec_status(
                    &ctx.spec.id,
                    GraphSpecStatus::Failed,
                    None,
                    Some(chrono::Utc::now()),
                )?;
                let (dirty, paths) = self.record_spec_end_dirty(&ctx.spec.id, ctx.workdir).await;
                let blocker = match dirty_tree_note(dirty, &paths) {
                    Some(note) => format!("{blocker} ({note})"),
                    None => blocker,
                };
                return Ok((0, Some(SpecExecutionOutcome::Blocked(blocker))));
            }
            self.db.update_graph_spec_status(
                &ctx.spec.id,
                GraphSpecStatus::Failed,
                None,
                Some(chrono::Utc::now()),
            )?;
            let (dirty, paths) = self.record_spec_end_dirty(&ctx.spec.id, ctx.workdir).await;
            let iteration_budget_blocker = match dirty_tree_note(dirty, &paths) {
                Some(note) => format!("{iteration_budget_blocker} ({note})"),
                None => iteration_budget_blocker,
            };
            return Ok((
                0,
                Some(SpecExecutionOutcome::Failed(iteration_budget_blocker)),
            ));
        }
        Ok((*iteration, None))
    }
}
