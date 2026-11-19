//! Single-node cursor steps, split from `run_spec.rs`.
use super::*;

impl GraphEngine {
    pub(crate) async fn run_spec_node_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        node_id: &str,
        previous_output: Option<&Value>,
        iteration_value: usize,
    ) -> Result<SpecStepFlow> {
        // CM48: a single agent node whose pair is declared unavailable blocks
        // here, before any run row is inserted or attempt counted — the
        // existing `block_graph` path (via `SpecExecutionOutcome::Blocked`)
        // pauses the graph and fires the `on_blocked` hooks.
        if let Some(node) = self.db.get_graph_node(node_id)? {
            if let Some(blocker) = self.node_availability_blocker(&node) {
                return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Blocked(blocker)));
            }
        }
        let attempt_setup = self
            .prepare_node_attempt(
                ctx,
                &state.resumable_sessions,
                node_id,
                previous_output,
                iteration_value,
            )
            .await?;
        let node = &attempt_setup.node;
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: Some(&node.id),
                run_id: Some(&attempt_setup.run_id),
                kind: activity::GraphEventKind::NodeStarted,
                message: format!("Node '{}' started.", node.name),
                payload: None,
            },
        );
        let mut retry = NodeRetryCtx {
            node,
            previous_output,
            run_id: attempt_setup.run_id.clone(),
            resume_candidate: attempt_setup.resume_candidate.clone(),
            resume_crosses_spec: attempt_setup.resume_crosses_spec,
            iteration_value,
            crew: attempt_setup.crew.clone(),
            crew_index: attempt_setup.crew_index,
        };
        let (execution, run, had_infra_crash) = self
            .run_node_with_infra_retries(ctx, state, &mut retry)
            .await?;
        let run_id = retry.run_id;

        tracing::info!(
            run_id = %run_id,
            status = ?run.status,
            node = %node.name,
            "node run completed"
        );

        // CB82: the daemon began graceful shutdown while this node ran (its
        // process was or will be SIGTERMed). Finalize as interrupted and
        // route no edge — never the fail edge a nonzero exit would take.
        if self.is_shutting_down() {
            return Ok(SpecStepFlow::Done(
                self.finalize_shutdown_interrupted(&run_id),
            ));
        }

        // B42/2026-08-05: something terminated this run out from
        // under us — a newer attempt at this node, a concurrent
        // `graph_control`, `graph_control`, or budget/`fail_graph`
        // sweep (see `run_was_terminated_out_of_band`). That is
        // engine bookkeeping, not a node failure — so this
        // dispatch stops here: it evaluates NO edge (never the
        // fail edge to a resilience node), fails nothing, and
        // leaves the graph to whichever dispatch now owns it.
        // Checked before any routing so the termination can
        // never be routed as a fail (the runaway that
        // manufactured a resilience run per killed implementer,
        // and the incident where a stale dispatch's late
        // completion failed a graph out from under a healthy
        // sibling dispatch).
        if run_was_terminated_out_of_band(&run) {
            return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Superseded));
        }

        // RS2: remember this node's captured session so a later
        // fail-edge bounce back to it resumes instead of cold
        // starting. A resumed run recorded the same id it continued;
        // a cold run recorded whatever it captured (or nothing).
        if let Some(sid) = run.session_id.clone() {
            state.resumable_sessions.insert(node.id.clone(), sid);
        }

        let execution = self.resolve_running_node_execution(&run_id, execution, &run)?;

        // FR5: an agent node settled its verdict — the run "worked", so a
        // later check-failure dead end in this run is a failure, never idle.
        if node.kind == GraphNodeKind::Agent {
            state.ran_agent = true;
            ctx.run_activity.agent_ran.store(true, Ordering::SeqCst);
        }

        // B37 / read-only: applied AFTER the node's own verdict is settled,
        // so they override every way a node can report success — a clean exit
        // code, or a `graph_complete_node` self-report of `pass`. A node that
        // moved history without the right to, or changed the tree while
        // read-only, fails; the fail is persisted on the run row so
        // `canopy graph info` shows it.
        let execution = self
            .apply_post_verdict_guards(ctx, &attempt_setup, &run_id, execution)
            .await?;

        // CB39: verify spec_start_head is still an ancestor of
        // HEAD after a committing node's turn. Runs whether the
        // node reported pass or fail. Skipped when there is no
        // spec_start_head (non-git workdir) — the engine stays
        // usable outside git.
        let (execution, had_infra_crash) = self
            .enforce_commit_ancestry(
                ctx,
                state,
                &attempt_setup,
                &run_id,
                execution,
                had_infra_crash,
            )
            .await?;

        self.publish_node_outcome(ctx, node, &execution, &run_id);

        // CB31: a wait-for-completion pause was requested while this
        // node was running. It ran to its own completion and its
        // verdict is now recorded normally; flag that run so
        // `graph_control` re-executing this node does not spend one
        // of its iterations (the pause is not an attempt), then
        // transition to paused and stop before the next node.
        if let Some(outcome) = self
            .pause_after_step_if_requested(ctx, state, iteration_value)
            .await?
        {
            return Ok(SpecStepFlow::Done(outcome));
        }

        if should_advance_to_next_spec(node, execution.status) {
            return Ok(SpecStepFlow::Done(
                self.finish_advanced_spec(ctx, state, execution).await?,
            ));
        }

        Ok(SpecStepFlow::Routed(NodeStepResult {
            execution,
            from_node_id: node.id.clone(),
            run_id: Some(run_id),
            had_infra_crash,
        }))
    }
}
