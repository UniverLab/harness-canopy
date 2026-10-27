//! Named-mutex gate: the wait loop a node run parks in until every mutex
//! it declared is granted.
//!
//! A run whose node (or ensemble) declares `mutex` is inserted as `waiting`;
//! [`acquire_mutexes_before_run`] then grants the full set atomically via
//! [`Database::try_acquire_mutex_set`], announces the wait once
//! (`mutex_waiting`, with who holds each resource), and flips the run to
//! `running` at the grant (`mutex_acquired`) — the node's timeout starts
//! there, not at the enqueue. Waiters sleep on
//! [`Database::mutex_notified`] (woken by every lease change) with a 30 s
//! safety poll, and stop — releasing nothing they never held — when the run
//! is finalized out from under them, the graph stops being live, or the
//! daemon shuts down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::db::Database;
use crate::domain::activity;
use crate::domain::graphs::{Graph, GraphNode, GraphRunStatus, GraphStatus};

/// The mutex resources `node` declares in its config (`mutex: [...]`).
/// Absent or misshaped reads as none — writers validate the shape, so the
/// engine never refuses a dispatch on it.
pub(crate) fn node_mutex_resources(node: &GraphNode) -> Vec<String> {
    node.config
        .get("mutex")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The mutex set one ensemble member run acquires: its node's own plus the
/// ensemble's, deduplicated.
pub(crate) fn union_mutex_resources(node: &GraphNode, ensemble_mutex: &[String]) -> Vec<String> {
    let mut resources = node_mutex_resources(node);
    for name in ensemble_mutex {
        if !resources.contains(name) {
            resources.push(name.clone());
        }
    }
    resources
}

/// Output marker for a run that stopped waiting because its graph was no
/// longer live — the same `terminated` shape `terminate_run_row` writes, so
/// `run_was_terminated_out_of_band` recognizes it as engine bookkeeping.
fn mutex_wait_terminated_output(reason: &str) -> Value {
    serde_json::json!({ "terminated": true, "reason": reason })
}

/// Wait until `run_id` holds every mutex in `resources`, then flip it to
/// `running` and return `true`. Returns `false` (the caller must stop
/// without executing) when the run was finalized out from under the wait
/// (interrupt, pause, reset, fail sweep, shutdown sweep), its graph stopped
/// being live, or the daemon is shutting down — settling the still-`waiting`
/// row itself in the latter two cases so no waiter is ever stranded.
///
/// An empty set grants immediately without touching the mutex tables.
pub(crate) async fn acquire_mutexes_before_run(
    db: &Database,
    shutdown: &AtomicBool,
    lp: &Graph,
    spec_id: &str,
    node: &GraphNode,
    run_id: &str,
    resources: &[String],
) -> Result<bool> {
    if resources.is_empty() {
        return Ok(true);
    }
    let mut announced_waiting = false;
    loop {
        if db.try_acquire_mutex_set(run_id, &lp.id, &node.name, resources)? {
            // Granted — unless someone finalized the run between the grant
            // and this mark (an interrupt racing us), in which case the
            // leases go back and this dispatch stops.
            if !db.mark_graph_run_started_from_waiting(run_id)? {
                let _ = db.release_mutexes_for_run(run_id);
                return Ok(false);
            }
            activity::record(
                db,
                activity::GraphEvent {
                    graph_id: &lp.id,
                    graph_name: &lp.name,
                    workdir: &lp.workdir,
                    spec_id: Some(spec_id),
                    node_id: Some(&node.id),
                    run_id: Some(run_id),
                    kind: activity::GraphEventKind::MutexAcquired,
                    message: format!(
                        "Node '{}' acquired mutexes: {}.",
                        node.name,
                        resources.join(", ")
                    ),
                    payload: Some(serde_json::json!({ "resources": resources })),
                },
            );
            return Ok(true);
        }
        let Some(run) = db.get_graph_run(run_id)? else {
            return Ok(false);
        };
        if run.status != GraphRunStatus::Waiting {
            return Ok(false);
        }
        if shutdown.load(Ordering::SeqCst) {
            db.update_graph_run_result(
                run_id,
                GraphRunStatus::Fail,
                Some(&super::routing::shutdown_interrupted_output()),
                Some(chrono::Utc::now()),
            )?;
            return Ok(false);
        }
        let graph_live = db.get_graph(&lp.id)?.is_some_and(|current| {
            matches!(current.status, GraphStatus::Running | GraphStatus::Pausing)
        });
        if !graph_live {
            db.update_graph_run_result(
                run_id,
                GraphRunStatus::Fail,
                Some(&mutex_wait_terminated_output(
                    "graph stopped while this node was waiting for its mutex",
                )),
                Some(chrono::Utc::now()),
            )?;
            return Ok(false);
        }
        if !announced_waiting {
            announced_waiting = true;
            announce_mutex_wait(db, lp, spec_id, node, run_id, resources)?;
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), db.mutex_notified()).await;
    }
}

/// Record the one `mutex_waiting` event for a wait, carrying who holds each
/// requested resource (the same `waiting_for` shape the read surfaces show).
fn announce_mutex_wait(
    db: &Database,
    lp: &Graph,
    spec_id: &str,
    node: &GraphNode,
    run_id: &str,
    resources: &[String],
) -> Result<()> {
    let waiting_for = db.get_graph_run_waiting_for(run_id)?;
    let holders = waiting_for
        .iter()
        .map(|detail| match (&detail.holder_graph, &detail.holder_node) {
            (Some(graph), Some(holder)) => {
                format!("{} (held by {graph}/{holder})", detail.resource)
            }
            _ => detail.resource.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ");
    activity::record(
        db,
        activity::GraphEvent {
            graph_id: &lp.id,
            graph_name: &lp.name,
            workdir: &lp.workdir,
            spec_id: Some(spec_id),
            node_id: Some(&node.id),
            run_id: Some(run_id),
            kind: activity::GraphEventKind::MutexWaiting,
            message: format!("Node '{}' waiting for mutexes: {holders}.", node.name),
            payload: Some(serde_json::json!({
                "resources": resources,
                "waiting_for": waiting_for,
            })),
        },
    );
    Ok(())
}
