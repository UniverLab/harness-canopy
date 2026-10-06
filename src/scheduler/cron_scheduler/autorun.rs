//! One-shot autorun launch path for the cron scheduler.
//!
//! Takes over a due autorun after its schedule was cleared: handle
//! completed/failed graphs, publish the activity, run the dirty-start
//! check, then resume the graph in the background.

use std::sync::Arc;

use crate::db::Database;
use crate::domain::activity;
use crate::domain::graphs::{GraphResetOutcome, GraphStatus};
use crate::graph_engine::GraphEngine;

/// Surface a due-but-blocked autorun every tick it's still blocked.
pub(super) fn warn_blocked_operator_paused_autorun(
    lp: &crate::domain::graphs::Graph,
    now_utc: chrono::DateTime<chrono::Utc>,
) {
    // C1: a due schedule on a graph the *operator* paused
    // (`paused_by_reconciliation` is false) can never fire on its
    // own — `is_fireable()` deliberately keeps excluding `Paused`
    // for every caller but reconciliation's. That's correct, but
    // it must not be a second silent expiry: surface it every
    // tick it's still blocked, so an operator watching logs sees
    // why the resume never happened instead of concluding canopy
    // forgot.
    if lp.status == GraphStatus::Paused
        && !lp.paused_by_reconciliation
        && lp.autorun_at.is_some_and(|at| now_utc >= at)
    {
        tracing::warn!(
            "Graph '{}' autorun_at is due but the graph is paused (not by \
             reconciliation); it will not fire until resumed manually via \
             graph_continue or graph_run — the schedule remains pending.",
            lp.id
        );
    }
}

/// Take over a due autorun after its schedule was cleared: handle
/// completed/failed graphs, publish the activity, run the dirty-start
/// check, then resume the graph in the background.
pub(super) async fn launch_due_autorun(
    graph_engine: &Arc<GraphEngine>,
    db: &Database,
    lp: &crate::domain::graphs::Graph,
) -> anyhow::Result<()> {
    if lp.status == GraphStatus::Completed {
        warn_autorun_completed(lp);
        return Ok(());
    }

    if lp.status == GraphStatus::Failed && !reset_failed_autorun_graph(db, lp)? {
        return Ok(());
    }

    tracing::info!("Graph '{}' reached its autorun_at time; launching", lp.id);
    activity::publish(
        db,
        &lp.workdir,
        &lp.id,
        &lp.name,
        "Autorun fired; resuming.",
    );

    let next_spec_preview = graph_engine
        .preview_next_spec_id(&lp.id, lp.active_run_queue_id.as_deref())
        .unwrap_or(None);
    if !autorun_dirty_start_verdict(graph_engine, lp, next_spec_preview.as_deref()).await {
        return Ok(());
    }

    // Resume with the graph's persisted run context (its queue, if any)
    // rather than a fresh `start_background`, which would fall back
    // to the graph's own bound specs — empty for a queue run, and
    // exactly how a resumed queue run used to be mistaken for
    // "nothing to do" and marked completed with members still
    // pending.
    Arc::clone(graph_engine).resume_background(lp.id.clone());
    Ok(())
}

/// WARN that a due autorun landed on an already-completed graph — the
/// schedule is cleared by the caller, and re-running a finished graph stays
/// a human decision via graph_reset + graph_run.
fn warn_autorun_completed(lp: &crate::domain::graphs::Graph) {
    tracing::warn!(
        "Graph '{}' autorun fired but the graph is already completed; clearing the \
         schedule without re-running it (re-running a finished graph is a human \
         decision via graph_reset + graph_run)",
        lp.id
    );
}

/// Reset a failed graph through its schedule and log the outcome; `Ok(false)`
/// when the graph could not be auto-reset, in which case the launch is
/// skipped exactly as the schedule path always did.
fn reset_failed_autorun_graph(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
) -> anyhow::Result<bool> {
    match db.reset_graph(&lp.id, None)? {
        GraphResetOutcome::Reset {
            spec_count,
            skipped_count,
        } => {
            tracing::info!(
                "Graph '{}' was failed; auto-reset by its schedule ({} spec(s) reset, {} skipped preserved) \
                 and resuming",
                lp.id,
                spec_count,
                skipped_count
            );
            Ok(true)
        }
        other => {
            tracing::warn!(
                "Graph '{}' autorun could not auto-reset it ({:?}); skipping launch",
                lp.id,
                other
            );
            Ok(false)
        }
    }
}

/// Run the dirty-start check for a due autorun and log its verdict; `false`
/// when the check refuses the launch. A check failure is logged and treated
/// as "proceed", exactly as the schedule path always did.
async fn autorun_dirty_start_verdict(
    graph_engine: &Arc<GraphEngine>,
    lp: &crate::domain::graphs::Graph,
    next_spec_preview: Option<&str>,
) -> bool {
    match graph_engine
        .dirty_start_check(&lp.id, &lp.workdir, next_spec_preview, lp.allow_dirty_start)
        .await
    {
        Ok(Some(notice)) if notice.refuse => {
            warn_autorun_refused(lp, &notice.message);
            false
        }
        Ok(Some(notice)) => {
            tracing::warn!(
                "Graph '{}' autorun launching with a warning: {}",
                lp.id,
                notice.message
            );
            true
        }
        Ok(None) => true,
        Err(e) => {
            tracing::error!("Graph '{}' autorun dirty-start check failed: {}", lp.id, e);
            true
        }
    }
}

/// WARN that the dirty-start check refused a due autorun launch.
fn warn_autorun_refused(lp: &crate::domain::graphs::Graph, message: &str) {
    tracing::warn!(
        "Graph '{}' autorun refused: {}. The schedule was one-shot and is \
         already cleared — relaunch manually via graph_run (or graph_continue) \
         once resolved.",
        lp.id,
        message
    );
}
