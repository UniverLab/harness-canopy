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

use crate::daemon::handler_run_exec::find_in_flight_run;
use crate::db::Database;
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;
use crate::sessions::SessionHub;
use crate::watchers::WatcherEngine;

use autorun::{launch_due_autorun, warn_blocked_operator_paused_autorun};
use schedule_math::{
    due_fire_local, earliest_duration_before, fold_earliest, fold_earliest_instant, to_7field_cron,
};

#[cfg(test)]
use crate::domain::graphs::GraphStatus;

const RECONCILE_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub use retry::RetryPolicy;

mod autorun;
mod delivery_merge;
pub(crate) mod held_sends;
mod retry;
mod schedule_math;
pub(crate) mod scheduled_prompts;
/// The internal cron scheduler that runs as a tokio background_agent.
pub struct CronScheduler {
    db: Arc<Database>,
    #[allow(dead_code)]
    executor: Arc<Executor>,
    cancel: CancellationToken,
    /// Wakes the scheduler to recalculate the next fire time.
    notify: Arc<Notify>,
    /// Optional graph engine — when set, the scheduler also evaluates graphs
    /// whose trigger is `Cron` and launches them alongside agents.
    graph_engine: Option<Arc<GraphEngine>>,
    /// Optional watcher engine — when set, enable_at/expiry transitions for
    /// watch-triggered graphs can start/stop their file watchers.
    watcher_engine: Option<Arc<WatcherEngine>>,
    /// Optional session hub — when set, `start` also spawns the dedicated
    /// delivery task that delivers due scheduled sends to hub sessions, so
    /// a schedule fires with no TUI attached.
    hub: Option<SessionHub>,
    /// Wakes the dedicated scheduled-send delivery task when a send is
    /// created, so delivery lands within seconds instead of waiting for the
    /// next cron tick.
    scheduled_prompt_notify: Arc<Notify>,
    /// Tick idempotency: the most recent *scheduled* tick already fired per
    /// schedulable (graphs keyed by [`graph_key`]).
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
    /// Failure-retry policy for scheduled runs (kept for API stability;
    /// agent retries now run through the graph engine).
    #[allow(dead_code)]
    retry: RetryPolicy,
    /// Seconds a due scheduled send waits for later sends to the same
    /// session so they arrive as one message (`0` disables merging).
    delivery_merge_window_s: u64,
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
            watcher_engine: None,
            hub: None,
            scheduled_prompt_notify: Arc::new(Notify::new()),
            last_fired: Arc::new(Mutex::new(std::collections::HashMap::new())),
            retry: RetryPolicy::default(),
            delivery_merge_window_s: delivery_merge::default_merge_window_s(),
        }
    }

    /// CM51: take the retry policy from the loaded config's `[scheduler]`
    /// table (which already carries any `CANOPY_RETRY_*` override) instead of
    /// the registry defaults.
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Take the scheduled-send merge window from the loaded config's
    /// `[scheduler]` table, the same snapshot pattern as [`Self::with_retry`].
    pub fn with_delivery_merge_window_s(mut self, window_s: u64) -> Self {
        self.delivery_merge_window_s = window_s;
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

    /// Attach a watcher engine so enable_at/expiry transitions for watch
    /// graphs can reconcile active watchers.
    pub fn with_watcher_engine(mut self, watcher_engine: Arc<WatcherEngine>) -> Self {
        self.watcher_engine = Some(watcher_engine);
        self
    }

    /// Attach the hub the dedicated delivery task delivers due scheduled
    /// sends through. Without a hub no delivery task starts and the
    /// scheduler neither delivers nor considers scheduled sends at all.
    pub fn with_hub(mut self, hub: SessionHub) -> Self {
        self.hub = Some(hub);
        self
    }

    /// Get a handle to wake the scheduler when agents change.
    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }

    /// Share one wake handle for scheduled sends: every successful
    /// `insert_scheduled_prompt` in the daemon notifies it, and the
    /// dedicated delivery task waits on it.
    pub fn with_scheduled_prompt_notify(mut self, notify: Arc<Notify>) -> Self {
        self.scheduled_prompt_notify = notify;
        self
    }

    /// Get a handle to wake the scheduled-send delivery task when a send is
    /// created.
    pub fn scheduled_prompt_notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.scheduled_prompt_notify)
    }

    /// Start the scheduler graph as a background tokio task.
    ///
    /// Returns a `CancellationToken` that can be used to stop the scheduler.
    pub fn start(self: Arc<Self>) -> CancellationToken {
        let cancel = self.cancel.clone();
        let scheduler = Arc::clone(&self);

        tokio::spawn(async move {
            tracing::info!("Internal cron scheduler started");
            scheduler.run_graph().await;
            tracing::info!("Internal cron scheduler stopped");
        });

        // Due sends get their own delivery task, off the serial cron tick:
        // it delivers everything due, then waits for the earliest of a
        // creation wake, the next `fire_at`, and a 2-second ceiling (which
        // covers sends created by another process that cannot notify).
        if let Some(hub) = self.hub.clone() {
            let db = Arc::clone(&self.db);
            let notify = Arc::clone(&self.scheduled_prompt_notify);
            let delivery_cancel = self.cancel.clone();
            let merge_window_s = self.delivery_merge_window_s;
            tokio::spawn(async move {
                scheduled_prompts::run_delivery_loop(
                    db,
                    hub,
                    notify,
                    delivery_cancel,
                    merge_window_s,
                )
                .await;
            });
        }

        cancel
    }

    /// The main scheduler graph. Sleeps until the next graph tick is due,
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

    /// Compute how long to sleep until the nearest cron-triggered graph fires.
    /// Falls back to 60 s if there are no active graphs or on parse errors.
    /// Agents fire through their backing graphs since the agent-to-graph
    /// migration; there is no separate agent schedule to fold in.
    fn next_sleep_duration(&self) -> std::time::Duration {
        const FALLBACK: std::time::Duration = std::time::Duration::from_secs(60);

        // Cron expressions are authored in the user's local timezone (a user
        // who types `0 9 * * *` expects 9 AM on their wall clock, not 9 AM
        // UTC). We feed the schedule iterator a `Local` "now" so it walks
        // fire times in the same frame the user wrote the expression in,
        // then convert the result to UTC for sleep-delta math (which uses
        // a wall-clock-independent `Duration`).
        let now_local = Local::now();
        let now_utc = Utc::now();
        let mut earliest: Option<chrono::DateTime<Utc>> = None;

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
            if !lp.enabled || lp.is_expired() || !lp.is_fireable() {
                continue;
            }
            fold_earliest(earliest, lp.schedule_expr(), now_local);
        }
    }

    /// Fold every pending one-shot schedule into `earliest`: graph
    /// `enable_at`, then graph `autorun_at`, then graph `auto_continue_at`.
    /// Agent one-shots fire through their backing graphs since the
    /// agent-to-graph migration.
    fn fold_pending_one_shot_wakeups(&self, earliest: &mut Option<chrono::DateTime<Utc>>) {
        if self.graph_engine.is_some() {
            if let Ok(pending) = self.db.list_pending_enable_graphs() {
                for lp in pending {
                    fold_earliest_instant(earliest, lp.enable_at);
                }
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

    /// Fire all cron-triggered graphs whose next cron time is now. Agents
    /// fire through their backing graphs since the agent-to-graph migration:
    /// the legacy agent tick (`try_fire_agent`), the agent one-shot
    /// `enable_at` fire and the per-tick corrupt-row quarantine are deleted.
    /// Corrupt agent rows are quarantined once by the daemon-start migration
    /// (`migrate_all_agents_to_graphs`); the row itself is never deleted.
    async fn fire_due_tasks(&self) -> anyhow::Result<()> {
        // Evaluate schedules in the user's local timezone. `now_utc` is only
        // used for the persisted `last_fired` comparison (which is stored in
        // UTC), and `now_local` for the cron-field match.
        let now_local = Local::now();
        let now_utc = Utc::now();

        // Cron-triggered graphs are evaluated in the same local frame.
        if self.graph_engine.is_some() {
            let graphs = self.db.list_cron_graphs()?;
            for lp in &graphs {
                self.try_fire_graph(lp, now_local).await?;
            }
            self.disable_expired_graphs().await?;
        }

        self.fire_due_enable_graphs(now_utc).await?;
        self.fire_due_autorun_graphs(now_utc).await?;
        self.fire_due_auto_continue_graphs(now_utc)?;

        Ok(())
    }

    /// One-shot `enable_at` for trigger graphs: enable and clear `enable_at`
    /// once due, mirroring agents. When a watch graph becomes active, reconcile
    /// its watcher.
    async fn fire_due_enable_graphs(&self, now_utc: chrono::DateTime<Utc>) -> anyhow::Result<()> {
        if self.graph_engine.is_none() {
            return Ok(());
        }
        let pending = self.db.list_pending_enable_graphs()?;
        for lp in &pending {
            if lp.enable_at.is_some_and(|at| now_utc >= at) {
                tracing::info!("Graph '{}' reached its enable_at time; enabling", lp.id);
                self.db.set_graph_enabled(&lp.id)?;
                self.reload_watch_for_enabled_graph(&lp.id).await;
            }
        }
        Ok(())
    }

    async fn reload_watch_for_enabled_graph(&self, graph_id: &str) {
        let Some(watcher_engine) = &self.watcher_engine else {
            return;
        };
        let Ok(Some(graph)) = self.db.get_graph(graph_id) else {
            return;
        };
        if graph.is_watch() && graph.enabled && !graph.is_expired() {
            let _ = watcher_engine.start_graph_watcher(&graph).await;
        }
    }

    /// Disable expired trigger graphs and stop their watchers so they no longer fire.
    async fn disable_expired_graphs(&self) -> anyhow::Result<()> {
        let mut expired_watch_ids = Vec::new();
        for lp in self
            .db
            .list_cron_graphs()?
            .into_iter()
            .chain(self.db.list_watch_graphs()?)
        {
            if lp.enabled && lp.is_expired() {
                self.db
                    .update_graph_lifecycle(&lp.id, Some(false), None, None)?;
                if lp.is_watch() {
                    expired_watch_ids.push(lp.id.clone());
                }
            }
        }
        if let Some(watcher_engine) = &self.watcher_engine {
            for id in expired_watch_ids {
                let _ = watcher_engine.stop_graph_watcher(&id).await;
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
    /// A `failed` graph is not launched as-is — `graph_control` refuses `failed`
    /// graphs, so firing here performs an explicit auto-reset-and-resume
    /// first: the same transition [`Database::reset_graph`] that backs the
    /// `graph_control` MCP tool, logged at INFO. That is the sanctioned,
    /// intentional way a quota-failed graph revives itself unattended (see
    /// [`crate::daemon::handler`] `graph_control`/`graph_control` tool
    /// docs). A `completed` graph is deliberately left alone — re-running a
    /// finished graph is a human decision via `graph_control` + `graph_control` — so
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
    /// requested `graph_control` action (`retry_current_node` by default) and
    /// resume it in place.
    ///
    /// Unlike [`Self::fire_due_autorun_graphs`], a graph that is no longer
    /// `Paused` by the scheduled time (already continued manually, failed,
    /// completed, or running) is *not* waited on further — the schedule is
    /// cleared right away without firing, so a stale deferred-resume can
    /// never double-run a graph that moved on some other way. This never
    /// resets or relaunches the graph (that's `autorun_at`'s job): it goes
    /// straight through the same action-then-resume path the `graph_control`
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
    /// graph left `Paused` some other way (manual `graph_control`, failure)
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
        Arc::clone(graph_engine).resume_background_with_trigger(
            lp.id.clone(),
            None,
            Some(crate::graph_engine::GraphRunTrigger::AutoContinue),
        );
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
        if self.skip_unfireable_graph(lp)? {
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

        if self.skip_unfireable_graph(lp)? {
            // Overlap suppression on the agent-backed graph: a busy backing
            // graph records `Missed` against its agent and skips the tick —
            // running only, no staleness. Non-agent graphs skip silently.
            let _ = self.db.record_agent_backing_missed(
                &lp.id,
                crate::domain::models::TriggerType::Scheduled,
            )?;
            return Ok(());
        }

        self.launch_due_graph(graph_engine, lp)
    }

    /// A graph not eligible for automatic firing (disabled, expired, or
    /// already running/paused/pausing) must not be relaunched by its trigger.
    /// Returns `true` when the tick is skipped. The busy check reuses the
    /// in-flight run lookup on the backing graph ([`find_in_flight_run`]):
    /// pending/in_progress only, no stale threshold.
    fn skip_unfireable_graph(&self, lp: &crate::domain::graphs::Graph) -> anyhow::Result<bool> {
        if !lp.enabled || lp.is_expired() {
            return Ok(true);
        }
        if lp.is_fireable() {
            return Ok(false);
        }
        if find_in_flight_run(&self.db, &lp.id)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .is_some()
        {
            tracing::info!("Graph '{}' tick skipped: previous run still running", lp.id);
        }
        Ok(true)
    }

    /// Launch a due cron graph. FR3: a spec-less graph with a
    /// `trigger_prompt` runs its nodes on every tick with the prompt as its
    /// idea, through the same idea-run machinery as every other launch. A
    /// graph with bound specs keeps today's behaviour (the prompt is
    /// ignored). Graphs launch fire-and-forget: the graph engine drives the
    /// graph and owns its own failure handling, so the agent RetryPolicy
    /// does not apply here.
    fn launch_due_graph(
        &self,
        graph_engine: &Arc<GraphEngine>,
        lp: &crate::domain::graphs::Graph,
    ) -> anyhow::Result<()> {
        tracing::info!("Cron graph '{}' is due; launching", lp.id);
        if let Some(idea) = graph_engine.trigger_prompt_idea(lp)? {
            // FR4: re-check under the tick — the run may have started
            // between the fireability check above and this launch. Busy
            // reuses [`find_in_flight_run`] (pending/in_progress, no stale).
            if find_in_flight_run(&self.db, &lp.id)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .is_some()
            {
                tracing::info!("Graph '{}' tick skipped: previous run still running", lp.id);
                let _ = self.db.record_agent_backing_missed(
                    &lp.id,
                    crate::domain::models::TriggerType::Scheduled,
                )?;
                return Ok(());
            }
            Arc::clone(graph_engine).start_background_tick(lp.id.clone(), idea);
            return Ok(());
        }
        Arc::clone(graph_engine).start_background_with_trigger(
            lp.id.clone(),
            Some(crate::graph_engine::GraphRunTrigger::Cron),
        );
        Ok(())
    }

    /// Stop the scheduler.
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}
#[cfg(test)]
mod tests;
