//! Evaluation and dispatch of one-shot `graph_scheduled_runs` inside `CronScheduler`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};

use crate::daemon::handler_run_exec::{find_in_flight_run, graph_run_status_guard};
use crate::db::graph_scheduled_runs::GraphScheduledRun;
use crate::scheduler::cron_scheduler::schedule_math::fold_earliest_instant;
use crate::scheduler::cron_scheduler::CronScheduler;

const MISS_WINDOW_SECS: i64 = 60;

impl CronScheduler {
    /// Fold all `pending` one-shot `graph_scheduled_runs` `fire_at` timestamps
    /// into `earliest` so `CronScheduler` wakes in time for them.
    pub(super) fn fold_scheduled_run_wakeups(&self, earliest: &mut Option<DateTime<Utc>>) {
        let Ok(pending) = self.db.list_graph_scheduled_runs(None, Some("pending")) else {
            return;
        };
        for run in pending {
            if let Ok(dt) = DateTime::parse_from_rfc3339(&run.fire_at) {
                fold_earliest_instant(earliest, Some(dt.with_timezone(&Utc)));
            }
        }
    }

    /// Inspect `pending` one-shot scheduled runs whose `fire_at <= now_utc`
    /// and either mark them `missed` (when past the 60 s window with
    /// `on_miss == "skip"`) or launch them via `GraphEngine`.
    pub(super) fn fire_due_scheduled_runs(&self, now_utc: DateTime<Utc>) -> anyhow::Result<()> {
        let Some(graph_engine) = self.graph_engine.as_ref() else {
            return Ok(());
        };
        let pending = self.db.list_graph_scheduled_runs(None, Some("pending"))?;
        for run in &pending {
            self.evaluate_scheduled_run(graph_engine, run, now_utc)?;
        }
        Ok(())
    }

    fn evaluate_scheduled_run(
        &self,
        graph_engine: &Arc<crate::graph_engine::GraphEngine>,
        run: &GraphScheduledRun,
        now_utc: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let fire_at = match DateTime::parse_from_rfc3339(&run.fire_at) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(e) => {
                let msg = format!("invalid fire_at '{}': {e}", run.fire_at);
                self.db
                    .mark_graph_scheduled_run_status(&run.id, "error", None, Some(&msg))?;
                return Ok(());
            }
        };
        if fire_at > now_utc {
            return Ok(());
        }

        if now_utc.signed_duration_since(fire_at) > Duration::seconds(MISS_WINDOW_SECS)
            && run.on_miss == "skip"
        {
            tracing::info!(
                "Scheduled run '{}' for graph '{}' missed its window (fire_at={}); marking missed",
                run.id,
                run.graph_id,
                run.fire_at
            );
            self.db
                .mark_graph_scheduled_run_status(&run.id, "missed", None, None)?;
            return Ok(());
        }

        let Some(graph) = self.db.get_graph(&run.graph_id)? else {
            let msg = format!("graph '{}' not found", run.graph_id);
            self.db
                .mark_graph_scheduled_run_status(&run.id, "error", None, Some(&msg))?;
            return Ok(());
        };
        if let Err(msg) = graph_run_status_guard(&run.graph_id, graph.status) {
            self.db
                .mark_graph_scheduled_run_status(&run.id, "error", None, Some(&msg))?;
            return Ok(());
        }

        match find_in_flight_run(&self.db, &run.graph_id) {
            Ok(Some(in_flight)) => {
                let msg = format!(
                    "graph '{}' already has in-flight run '{}'",
                    run.graph_id, in_flight.id
                );
                self.db
                    .mark_graph_scheduled_run_status(&run.id, "error", None, Some(&msg))?;
                return Ok(());
            }
            Ok(None) => {}
            Err(e) => {
                let msg = format!("failed checking in-flight run: {e}");
                self.db
                    .mark_graph_scheduled_run_status(&run.id, "error", None, Some(&msg))?;
                return Ok(());
            }
        }

        tracing::info!(
            "Launching one-shot scheduled run '{}' for graph '{}'",
            run.id,
            run.graph_id
        );
        Arc::clone(graph_engine).start_background_run(
            run.graph_id.clone(),
            run.queue_id.clone(),
            run.workdir.clone(),
            run.idea.clone(),
            None,
            None,
        );
        let fired_at = now_utc.to_rfc3339();
        self.db
            .mark_graph_scheduled_run_status(&run.id, "fired", Some(&fired_at), None)?;
        Ok(())
    }
}
