//! Bound-spec advance loop, split from `dispatch.rs` (FR1/FR2 live read).
use super::*;

/// Action to take after handling a spec execution outcome.
/// Used by `run_graph_dispatch` to reduce nesting and duplication.
pub(crate) enum SpecOutcomeAction {
    Continue,
    Pause,
}
/// Immutable inputs shared by every dispatch helper.
pub(crate) struct DispatchCtx<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) graph_id: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) is_resume: bool,
    /// When set, the first spec this run executes starts at this node
    /// instead of at Start.
    pub(crate) directed: Option<&'a DirectedStart>,
    pub(crate) claimed_at: chrono::DateTime<chrono::Utc>,
    /// FR5: this run was started by a cron tick of a spec-less graph, so a
    /// check-failure dead end with no agent run is an *idle* completion
    /// rather than a failure. `false` for every other dispatch, including a
    /// manual run of the same graph.
    pub(crate) idle_run: bool,
    /// FR5: agent/Fail-edge activity accumulated across every spec this
    /// dispatch runs — the idle rule is run-scoped, not per-spec.
    pub(crate) run_activity: RunActivity,
    /// Daemon trigger source for run logging; `None` for manual run/continue.
    pub(crate) daemon_trigger: Option<crate::graph_engine::graph_run_log::GraphRunTrigger>,
    /// Watch-fire context for this dispatch; `Some` only on the watchers'
    /// watch-fire path. Cron ticks and manual runs leave this `None`.
    pub(crate) watch_fire: Option<WatchFireCtx>,
}
/// What a bound-spec loop learned beyond its flow: whether an idle tick
/// terminal produced the completion (FR5) and whether any agent node ran.
pub(crate) struct BoundLoopResult {
    pub(crate) flow: DispatchFlow,
    pub(crate) idle: bool,
    pub(crate) agent_ran: bool,
}
/// Whether a dispatch helper wants the main loop to keep going or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchFlow {
    Continue,
    Stop,
}

impl GraphEngine {
    /// FR1/FR2: run the graph's bound specs to completion, deciding the next
    /// one from a **live** read of the graph at every advance — never from a
    /// list captured when the run started. A spec bound with `graph_edit`
    /// while the run is in flight is therefore seen by the very next advance:
    /// at its own position when it comes after the spec that just ran, and
    /// right after it when it was bound at a lower position (the scan restarts
    /// at position 0 and skips the finished ones). The 2026-10-26 loss — a
    /// spec bound at position 6 while spec 4 ran, left `pending` forever as the
    /// graph reported `Completed (5/6 specs).` — is exactly what the fresh read
    /// rules out.
    pub(crate) async fn run_bound_spec_loop(
        &self,
        ctx: &DispatchCtx<'_>,
        completed_specs: &mut Vec<(String, String)>,
    ) -> Result<BoundLoopResult> {
        // A directed run makes its target spec the FIRST spec it executes —
        // bound specs before it are skipped, and every spec after it runs
        // normally from Start.
        let mut reached_directed = ctx.directed.is_none();
        // Specs this dispatch has already dealt with: the live read shows a
        // finished spec again on every later advance, and re-announcing it (or
        // re-running it) on each one would flood the log and loop forever.
        let mut settled: Vec<String> = Vec::new();
        // FR5: what the loop learned beyond its flow — whether an idle tick
        // terminal completed the run, and whether any agent node ran.
        let mut idle = false;
        let mut agent_ran = false;
        loop {
            if self.is_paused(ctx.graph_id)? {
                return Ok(BoundLoopResult {
                    flow: DispatchFlow::Stop,
                    idle,
                    agent_ran,
                });
            }
            let specs = self.db.list_graph_specs(ctx.graph_id)?;
            let Some(spec) =
                self.pick_next_bound_spec(ctx, &specs, &mut reached_directed, &mut settled)?
            else {
                break;
            };
            settled.push(spec.id.clone());
            let outcome = self.run_spec(ctx, &spec, None).await?;
            match &outcome {
                SpecExecutionOutcome::Idle { .. } => idle = true,
                SpecExecutionOutcome::Completed { ran_agent, .. } => agent_ran |= ran_agent,
                _ => {}
            }
            let action = self
                .handle_spec_outcome(outcome, &spec, ctx, completed_specs)
                .await?;
            match action {
                SpecOutcomeAction::Continue => continue,
                SpecOutcomeAction::Pause => {
                    return Ok(BoundLoopResult {
                        flow: DispatchFlow::Stop,
                        idle,
                        agent_ran,
                    });
                }
            }
        }
        Ok(BoundLoopResult {
            flow: DispatchFlow::Continue,
            idle,
            agent_ran,
        })
    }
}
