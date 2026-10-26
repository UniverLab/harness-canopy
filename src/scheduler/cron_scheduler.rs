//! Internal cron scheduler — runs inside the daemon process.
//!
//! Instead of polling on a fixed interval, the scheduler computes the
//! nearest `next_fire_time` across all active cron agents and sleeps exactly
//! until that instant.  A `Notify` handle lets the daemon wake the
//! scheduler early when agents are added, updated, or re-enabled.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, Utc};
use cron::Schedule;
use tokio::sync::{Mutex, Notify};
use tokio_util::sync::CancellationToken;

use crate::application::ports::{AgentRepository, RunRepository};
use crate::db::Database;
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;

use autorun::{launch_due_autorun, warn_blocked_operator_paused_autorun};
use retry::run_with_retry;
use schedule_math::{
    agent_due_tick, due_fire_local, earliest_duration_before, fold_earliest, fold_earliest_instant,
    to_7field_cron,
};

#[cfg(test)]
use crate::domain::graphs::GraphStatus;
#[cfg(test)]
use retry::{run_outcome_is_failure, should_retry, wait_retry_delay};
#[cfg(test)]
use schedule_math::next_fire_utc;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub use retry::RetryPolicy;

mod autorun;
mod retry;
mod schedule_math;
/// The internal cron scheduler that runs as a tokio background_agent.
pub struct CronScheduler {
    db: Arc<Database>,
    executor: Arc<Executor>,
    cancel: CancellationToken,
    /// Wakes the scheduler to recalculate the next fire time.
    notify: Arc<Notify>,
    /// Optional graph engine — when set, the scheduler also evaluates graphs
    /// whose trigger is `Cron` and launches them alongside agents.
    graph_engine: Option<Arc<GraphEngine>>,
    /// Tick idempotency: the most recent *scheduled* tick already fired per
    /// schedulable (agents keyed by their id; graphs by [`graph_key`] to avoid
    /// colliding with an agent that happens to share the same id).
    ///
    /// The stored value is the matched fire time itself (what
    /// [`due_fire_local`] returned), not the wall-clock instant the
    /// evaluation ran at. Two evaluations of the *same* tick — whether from
    /// the regular tick graph firing twice in a row or a second evaluation
    /// path racing it — compute the identical scheduled-tick value, so a
    /// plain equality/`>=` check dedupes them exactly. A wall-clock window
    /// (e.g. "fired within the last 60s") would instead depend on how long
    /// evaluation happened to take, which is what let two evaluations of one
    /// tick slip past a time-window check and both spawn.
    last_fired: Arc<Mutex<std::collections::HashMap<String, chrono::DateTime<Utc>>>>,
    /// Per-agent in-flight guard: agents currently spawned and not yet
    /// finalized. A fire for an agent already in this set is SKIPPED and
    /// logged at info — never queued behind, never spawned in parallel.
    /// This is the single choke point every firing path (scheduled, watch,
    /// manual) routes through at the scheduler level.
    in_flight: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Failure-retry policy applied to scheduled runs.
    retry: RetryPolicy,
}

/// Namespace a graph id in the shared `last_fired` map.
fn graph_key(graph_id: &str) -> String {
    format!("graph:{graph_id}")
}

impl CronScheduler {
    pub fn new(db: Arc<Database>, executor: Arc<Executor>) -> Self {
        Self {
            db,
            executor,
            cancel: CancellationToken::new(),
            notify: Arc::new(Notify::new()),
            graph_engine: None,
            last_fired: Arc::new(Mutex::new(std::collections::HashMap::new())),
            in_flight: Arc::new(Mutex::new(std::collections::HashSet::new())),
            retry: RetryPolicy::default(),
        }
    }

    /// CM51: take the retry policy from the loaded config's `[scheduler]`
    /// table (which already carries any `CANOPY_RETRY_*` override) instead of
    /// the registry defaults.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Build a scheduler that also fires cron-triggered graphs via `graph_engine`.
    pub fn with_graphs(
        db: Arc<Database>,
        executor: Arc<Executor>,
        graph_engine: Arc<GraphEngine>,
    ) -> Self {
        Self {
            graph_engine: Some(graph_engine),
            ..Self::new(db, executor)
        }
    }

    /// Get a handle to wake the scheduler when agents change.
    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// Seed the tick-idempotency map from the database so a restart doesn't
    /// immediately re-fire a tick whose run is still recorded from before
    /// the restart. `last_run_at` is a completion timestamp, not a scheduled
    /// tick, so this is a conservative approximation: it only suppresses a
    /// re-fire when the next candidate tick is not newer than the last
    /// recorded run. The [`try_start_run`](crate::application::ports::RunRepository::try_start_run)
    /// in-flight guard is what actually prevents a concurrent duplicate
    /// execution; this seed is just about not re-spawning a tick that was
    /// already handled moments before the restart.
    async fn initialize_last_fired(&self) {
        let mut last_fired = self.last_fired.lock().await;
        if let Ok(agents) = self.db.list_cron_agents() {
            for agent in agents {
                if let Some(last_run_at) = agent.last_run_at {
                    last_fired.insert(agent.id, last_run_at);
                }
            }
        }
    }

    /// Start the scheduler graph as a background tokio task.
    ///
    /// Returns a `CancellationToken` that can be used to stop the scheduler.
    pub fn start(self: Arc<Self>) -> CancellationToken {
        let cancel = self.cancel.clone();
        let scheduler = Arc::clone(&self);

        tokio::spawn(async move {
            tracing::info!("Internal cron scheduler started");
            // Initialize from database to prevent duplicate executions after restart
            scheduler.initialize_last_fired().await;
            scheduler.run_graph().await;
            tracing::info!("Internal cron scheduler stopped");
        });

        cancel
    }

    /// The main scheduler graph. Sleeps until the next agent is due,
    /// or wakes early on cancel/notify.
    async fn run_graph(&self) {
        loop {
            let sleep_dur = self.next_sleep_duration();

            tokio::select! {
                _ = self.cancel.cancelled() => break,
                _ = self.notify.notified() => {
                    continue;
                }
                _ = tokio::time::sleep(RECONCILE_INTERVAL) => {
                    continue;
                }
                _ = tokio::time::sleep(sleep_dur) => {
                    if let Err(e) = self.fire_due_tasks().await {
                        tracing::error!("Scheduler fire failed: {}", e);
                    }
                }
            }
        }
    }

    /// Compute how long to sleep until the nearest agent fires.
    /// Falls back to 60 s if there are no active agents or on parse errors.
    fn next_sleep_duration(&self) -> std::time::Duration {
        const FALLBACK: std::time::Duration = std::time::Duration::from_secs(60);

        let Ok(agents) = self.db.list_cron_agents() else {
            return FALLBACK;
        };

        // Cron expressions are authored in the user's local timezone (a user
        // who types `0 9 * * *` expects 9 AM on their wall clock, not 9 AM
        // UTC). We feed the schedule iterator a `Local` "now" so it walks
        // fire times in the same frame the user wrote the expression in,
        // then convert the result to UTC for sleep-delta math (which uses
        // a wall-clock-independent `Duration`).
        let now_local = Local::now();
        let now_utc = Utc::now();
        let mut earliest: Option<chrono::DateTime<Utc>> = None;

        for agent in &agents {
            if !agent.enabled || agent.is_expired() {
                continue;
            }
            fold_earliest(&mut earliest, agent.schedule_expr(), now_local);
        }

        self.fold_fireable_graph_schedules(&mut earliest, now_local);
        self.fold_pending_one_shot_wakeups(&mut earliest);

        match earliest {
            Some(t) => earliest_duration_before(now_utc, t),
            None => FALLBACK,
        }
    }

    /// Cron-triggered graphs share the same sleep math as agents.
    fn fold_fireable_graph_schedules(
        &self,
        earliest: &mut Option<chrono::DateTime<Utc>>,
        now_local: chrono::DateTime<Local>,
    ) {
        if self.graph_engine.is_none() {
            return;
        }
        let Ok(graphs) = self.db.list_cron_graphs() else {
            return;
        };
        for lp in &graphs {
            if !lp.is_fireable() {
                continue;
            }
            fold_earliest(earliest, lp.schedule_expr(), now_local);
        }
    }

    /// Fold every pending one-shot schedule into `earliest`: agent
    /// `enable_at`, then graph `autorun_at`, then graph `auto_continue_at` —
    /// each gated on the graph engine exactly where its query sits.
    fn fold_pending_one_shot_wakeups(&self, earliest: &mut Option<chrono::DateTime<Utc>>) {
        // One-shot `enable_at` schedules also need a wakeup, independent of
        // any cron expression on the agent.
        if let Ok(pending) = self.db.list_pending_enable_agents() {
            for agent in pending {
                fold_earliest_instant(earliest, agent.enable_at);
            }
        }

        // One-shot `autorun_at` graph schedules need a wakeup too, independent
        // of any cron trigger on the graph.
        if self.graph_engine.is_none() {
            return;
        }
        if let Ok(pending) = self.db.list_pending_autorun_graphs() {
            for lp in pending {
                fold_earliest_instant(earliest, lp.autorun_at);
            }
        }

        // One-shot `auto_continue_at` graph schedules (deferred resume of a
        // paused graph) need a wakeup too, same as `autorun_at` above.
        if self.graph_engine.is_none() {
            return;
        }
        if let Ok(pending) = self.db.list_pending_auto_continue_graphs() {
            for lp in pending {
                fold_earliest_instant(earliest, lp.auto_continue_at);
            }
        }
    }

    /// Quarantine any agent whose row failed to decode (e.g. a `trigger_config`
    /// written directly to SQLite by an external tool, not the JSON shape
    /// canopy expects): disable it and warn once, then leave it alone.
    ///
    /// Once disabled, the row is excluded from `list_cron_agents` (which
    /// filters `enabled = 1`), so subsequent ticks never see it as still
    /// enabled and never re-warn — this is what keeps a single corrupt row
    /// from producing a repeating per-tick error. The row itself is never
    /// touched or reinterpreted; only `enabled` changes.
    fn quarantine_corrupt_agents(&self) -> anyhow::Result<()> {
        for corrupt in self.db.list_corrupt_agents()? {
            if !corrupt.enabled {
                continue;
            }
            tracing::warn!(
                "Agent '{}' has a corrupt trigger_config and cannot be scheduled ({}); \
                 quarantining (disabling) it",
                corrupt.id,
                corrupt.error
            );
            self.db.update_agent_enabled(&corrupt.id, false)?;
        }
        Ok(())
    }

    /// Fire all agents whose next cron time is now (within a 1-second tolerance).
    async fn fire_due_tasks(&self) -> anyhow::Result<()> {
        self.quarantine_corrupt_agents()?;
        let agents = self.db.list_cron_agents()?;
        // Evaluate schedules in the user's local timezone. `now_utc` is only
        // used for the persisted `last_fired` comparison (which is stored in
        // UTC), and `now_local` for the cron-field match.
        let now_local = Local::now();
        let now_utc = Utc::now();

        for agent in &agents {
            self.try_fire_agent(agent, now_local).await?;
        }

        // Cron-triggered graphs are evaluated in the same local frame.
        if self.graph_engine.is_some() {
            let graphs = self.db.list_cron_graphs()?;
            for lp in &graphs {
                self.try_fire_graph(lp, now_local).await?;
            }
        }

        self.fire_due_enable_at(now_utc)?;
        self.fire_due_autorun_graphs(now_utc).await?;
        self.fire_due_auto_continue_graphs(now_utc)?;

        Ok(())
    }

    /// One-shot `enable_at`: for each disabled agent with a pending
    /// `enable_at` in the past, enable it and clear the schedule. Unlike
    /// cron agents (`list_cron_agents` filters `enabled = 1`), these are
    /// disabled by definition, hence the dedicated query.
    fn fire_due_enable_at(&self, now_utc: chrono::DateTime<Utc>) -> anyhow::Result<()> {
        let pending = self.db.list_pending_enable_agents()?;
        for agent in &pending {
            if agent.enable_at.is_some_and(|at| now_utc >= at) {
                tracing::info!("Agent '{}' reached its enable_at time; enabling", agent.id);
                self.db.activate_scheduled_enable(&agent.id)?;
            }
        }
        Ok(())
    }

    /// One-shot `autorun_at`: for each graph with a pending `autorun_at` in
    /// the past that is still fireable (not `Running`/`Paused`) — or `Paused`
    /// because `reconcile_orphaned_graphs` put it there, see
    /// [`crate::domain::graphs::Graph::is_autorun_due`] — clear the schedule
    /// and launch it once via the graph engine. Unlike cron graphs, this never
    /// repeats.
    ///
    /// A `failed` graph is not launched as-is — `graph_run` refuses `failed`
    /// graphs, so firing here performs an explicit auto-reset-and-resume
    /// first: the same transition [`Database::reset_graph`] that backs the
    /// `graph_reset` MCP tool, logged at INFO. That is the sanctioned,
    /// intentional way a quota-failed graph revives itself unattended (see
    /// [`crate::daemon::handler`] `graph_reset`/`graph_schedule_autorun` tool
    /// docs). A `completed` graph is deliberately left alone — re-running a
    /// finished graph is a human decision via `graph_reset` + `graph_run` — so
    /// firing on one only logs a WARN and clears the schedule.
    async fn fire_due_autorun_graphs(&self, now_utc: chrono::DateTime<Utc>) -> anyhow::Result<()> {
        let Some(graph_engine) = self.graph_engine.as_ref() else {
            return Ok(());
        };
        let pending = self.db.list_pending_autorun_graphs()?;
        for lp in &pending {
            if !lp.is_autorun_due(now_utc) {
                warn_blocked_operator_paused_autorun(lp, now_utc);
                continue;
            }
            self.db.clear_graph_autorun(&lp.id)?;
            launch_due_autorun(graph_engine, &self.db, lp).await?;
        }
        Ok(())
    }

    /// One-shot `auto_continue_at`: for each graph with a pending
    /// auto-continue schedule whose time has been reached, clear the
    /// schedule and — only if the graph is still `Paused` — fire the
    /// requested `graph_continue` action (`retry_current_node` by default) and
    /// resume it in place.
    ///
    /// Unlike [`Self::fire_due_autorun_graphs`], a graph that is no longer
    /// `Paused` by the scheduled time (already continued manually, failed,
    /// completed, or running) is *not* waited on further — the schedule is
    /// cleared right away without firing, so a stale deferred-resume can
    /// never double-run a graph that moved on some other way. This never
    /// resets or relaunches the graph (that's `autorun_at`'s job): it goes
    /// straight through the same action-then-resume path the `graph_continue`
    /// MCP tool uses, preserving the paused cursor/context.
    fn fire_due_auto_continue_graphs(&self, now_utc: chrono::DateTime<Utc>) -> anyhow::Result<()> {
        let Some(graph_engine) = self.graph_engine.as_ref() else {
            return Ok(());
        };
        let pending = self.db.list_pending_auto_continue_graphs()?;
        for lp in &pending {
            if !lp.is_auto_continue_due(now_utc) {
                self.clear_stale_auto_continue(lp, now_utc)?;
                continue;
            }
            self.db.clear_graph_auto_continue(&lp.id)?;
            self.resume_graph_via_auto_continue(graph_engine, lp);
        }
        Ok(())
    }

    /// Not firing this tick: if the auto-continue time has passed but the
    /// graph left `Paused` some other way (manual `graph_continue`, failure)
    /// before the schedule fired, the schedule is stale — clear it now rather
    /// than leaving it to linger forever waiting for `Paused` to recur
    /// (unlike `autorun_at`, which does wait, since its target statuses
    /// don't otherwise repeat).
    fn clear_stale_auto_continue(
        &self,
        lp: &crate::domain::graphs::Graph,
        now_utc: chrono::DateTime<Utc>,
    ) -> anyhow::Result<()> {
        if lp.is_auto_continue_time_reached(now_utc) {
            self.db.clear_graph_auto_continue(&lp.id)?;
            tracing::warn!(
                "Graph '{}' auto-continue fired but the graph is no longer paused ({}); \
                 clearing the schedule without resuming it",
                lp.id,
                lp.status.as_str()
            );
        }
        Ok(())
    }

    /// Apply the graph's requested auto-continue action and — only if the
    /// action succeeded — resume it in place with its persisted run context.
    fn resume_graph_via_auto_continue(
        &self,
        graph_engine: &Arc<GraphEngine>,
        lp: &crate::domain::graphs::Graph,
    ) {
        let action = lp
            .auto_continue_action
            .as_deref()
            .unwrap_or("retry_current_node");
        let applied = match action {
            "skip_next_spec" => crate::daemon::handler::handle_skip_next_spec(&self.db, &lp.id),
            _ => crate::daemon::handler::handle_retry_current_node(&self.db, &lp.id),
        };
        if let Err(error) = applied {
            tracing::warn!(
                "Graph '{}' auto-continue could not apply action '{}' ({}); skipping resume",
                lp.id,
                action,
                error.message
            );
            return;
        }

        tracing::info!(
            "Graph '{}' reached its auto_continue_at time; resuming with action '{}'",
            lp.id,
            action
        );
        Arc::clone(graph_engine).resume_background(lp.id.clone(), None);
    }

    /// Evaluate a single cron graph and launch it via the graph engine if due.
    async fn try_fire_graph(
        &self,
        lp: &crate::domain::graphs::Graph,
        now_local: chrono::DateTime<Local>,
    ) -> anyhow::Result<()> {
        let Some(graph_engine) = self.graph_engine.as_ref() else {
            return Ok(());
        };
        // A graph already running/paused must not be relaunched by its trigger.
        if !lp.is_fireable() {
            return Ok(());
        }

        let Some(schedule_expr) = lp.schedule_expr() else {
            return Ok(());
        };
        let schedule = match Schedule::from_str(&to_7field_cron(schedule_expr)) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "Graph '{}' has invalid cron expression '{}': {}",
                    lp.id,
                    schedule_expr,
                    e
                );
                return Ok(());
            }
        };

        let Some(due_local) = due_fire_local(&schedule, now_local) else {
            return Ok(());
        };
        let scheduled_tick = due_local.with_timezone(&Utc);

        // Tick idempotency in a graph-namespaced key: dedupe by the scheduled
        // tick itself, not by when this evaluation happened to run.
        let key = graph_key(&lp.id);
        {
            let mut last_fired = self.last_fired.lock().await;
            if last_fired
                .get(&key)
                .is_some_and(|last| *last >= scheduled_tick)
            {
                tracing::info!(
                    "Graph '{}' tick {} already fired; skipping duplicate evaluation",
                    lp.id,
                    scheduled_tick
                );
                return Ok(());
            }
            last_fired.insert(key, scheduled_tick);
        }

        tracing::info!("Cron graph '{}' is due; launching", lp.id);
        // Graphs are launched fire-and-forget: the graph engine drives the graph
        // and owns its own failure handling, so the agent RetryPolicy does not
        // apply here.
        Arc::clone(graph_engine).start_background(lp.id.clone());
        Ok(())
    }

    /// Evaluate a single agent and spawn it if it is due. Returns early on
    /// disabled/expired/parse-error conditions.
    async fn try_fire_agent(
        &self,
        agent: &crate::domain::models::Agent,
        now_local: chrono::DateTime<Local>,
    ) -> anyhow::Result<()> {
        if !agent.enabled {
            return Ok(());
        }

        if agent.is_expired() {
            tracing::info!("Agent '{}' has expired, disabling", agent.id);
            self.db.update_agent_enabled(&agent.id, false)?;
            return Ok(());
        }

        let Some(scheduled_tick) = agent_due_tick(agent, now_local) else {
            return Ok(());
        };

        // Tick idempotency: dedupe by the scheduled tick itself (in UTC, so
        // it lines up with the rest of the system — DB schema, `last_run_at`,
        // daemon JSON), not by when this evaluation happened to run. Two
        // evaluations of the same tick — the regular tick graph firing twice,
        // or a second evaluation path racing it — compute the identical
        // `scheduled_tick`, so this is an exact key, not a time-window guess.
        {
            let mut last_fired = self.last_fired.lock().await;
            if last_fired
                .get(&agent.id)
                .is_some_and(|last| *last >= scheduled_tick)
            {
                tracing::info!(
                    "Agent '{}' tick {} already fired; skipping duplicate evaluation",
                    agent.id,
                    scheduled_tick
                );
                return Ok(());
            }
            last_fired.insert(agent.id.clone(), scheduled_tick);
        }

        if !self.claim_in_flight_slot(agent).await {
            return Ok(());
        }
        self.spawn_scheduled_agent_run(agent);
        Ok(())
    }

    /// Per-agent in-flight guard: if this agent is already running
    /// (spawned but not yet finalized), skip this fire. This is the
    /// single choke point at the scheduler level — all firing paths
    /// (scheduled tick, notify wake-up) go through it.
    /// Returns `true` when the slot was newly claimed.
    async fn claim_in_flight_slot(&self, agent: &crate::domain::models::Agent) -> bool {
        let mut in_flight = self.in_flight.lock().await;
        if !in_flight.insert(agent.id.clone()) {
            tracing::info!(
                "Agent '{}' is already running; skipping this fire",
                agent.id
            );
            // Record a Missed run so the skip is visible in execution
            // history, distinguishing "skipped: already running" from
            // "never fired".
            let now = Utc::now();
            let missed = crate::domain::models::RunLog {
                id: uuid::Uuid::new_v4().to_string(),
                background_agent_id: agent.id.clone(),
                status: crate::domain::models::RunStatus::Missed,
                trigger_type: crate::domain::models::TriggerType::Scheduled,
                summary: Some("Skipped: already running (scheduler in-flight guard)".to_string()),
                started_at: now,
                finished_at: Some(now),
                exit_code: None,
                timeout_at: None,
                // CB43: nothing executed on a skipped run — no pair.
                executed_platform: None,
                executed_model: None,
            };
            let _ = self.db.insert_run(&missed);
            return false;
        }
        true
    }

    fn spawn_scheduled_agent_run(&self, agent: &crate::domain::models::Agent) {
        let executor = Arc::clone(&self.executor);
        let agent = agent.clone();
        let retry = self.retry;
        let cancel = self.cancel.clone();
        let in_flight = Arc::clone(&self.in_flight);
        let agent_id = agent.id.clone();
        tokio::spawn(async move {
            run_with_retry(executor, agent, retry, cancel).await;
            // Remove from in-flight set when the run completes (including
            // retries). This must happen unconditionally so a failed run
            // doesn't permanently block future fires.
            in_flight.lock().await.remove(&agent_id);
        });
    }

    /// Stop the scheduler.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}
#[cfg(test)]
mod tests;
