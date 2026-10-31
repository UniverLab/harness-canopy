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

use super::{GraphEngine, NodeExecution, NodeStepResult, SpecStepCtx, SpecStepFlow};
use crate::db::connection::{is_busy_error, with_busy_retry};
use crate::db::Database;
use crate::domain::activity;
use crate::domain::graphs::{Graph, GraphNode, GraphNodeRun, GraphRunStatus, GraphStatus};

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

/// Fail output for an exhausted-busy settle: an `error` string carrying the
/// busy message. Deliberately without the `terminated`/`interrupted`
/// markers, so the row routes through the normal fail edges instead of
/// stopping as out-of-band bookkeeping.
pub(crate) fn busy_fail_output(message: &str) -> Value {
    serde_json::json!({ "error": message })
}

/// The busy message when `run` is an exhausted-busy Fail settle, so the
/// refused-wait verdict can narrate it as its summary instead of the
/// generic stopped-while-waiting text.
pub(crate) fn busy_fail_message(run: &GraphNodeRun) -> Option<String> {
    if run.status != GraphRunStatus::Fail {
        return None;
    }
    let message = run.output.as_ref()?.get("error")?.as_str()?;
    message
        .starts_with("transient database busy")
        .then(|| message.to_string())
}

/// Best-effort Fail settle for an exhausted-busy operation (`let _ =`,
/// mirroring `gate_router.rs`): the busy message becomes the node verdict
/// instead of an engine-fatal error. Retried, so a holder that releases
/// inside the extended window still lets the Fail row land.
pub(crate) fn settle_busy_fail(db: &Database, run_id: &str, error: &anyhow::Error) {
    let _ = with_busy_retry("update_graph_run_result", || {
        db.update_graph_run_result(
            run_id,
            GraphRunStatus::Fail,
            Some(&busy_fail_output(&error.to_string())),
            Some(chrono::Utc::now()),
        )
    });
}

/// Stop triple for a mutex wait whose final run-row read exhausted the
/// busy budget: settle Fail best-effort and stop with a synthetic Fail
/// run carrying the busy message, so the verdict still routes through
/// the normal fail edges instead of failing the graph. The stored row
/// catches up on the next successful write.
pub(crate) fn busy_stopped_execution(
    db: &Database,
    ctx: &SpecStepCtx<'_>,
    node: &GraphNode,
    run_id: &str,
    error: &anyhow::Error,
) -> (NodeExecution, GraphNodeRun, bool) {
    settle_busy_fail(db, run_id, error);
    let message = error.to_string();
    let output = busy_fail_output(&message);
    let now = chrono::Utc::now();
    let run = GraphNodeRun {
        id: run_id.to_string(),
        graph_id: ctx.lp.id.clone(),
        spec_id: ctx.spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(output.clone()),
        started_at: now,
        completed_at: Some(now),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    let execution = NodeExecution {
        status: GraphRunStatus::Fail,
        output,
        summary: message,
    };
    (execution, run, false)
}

/// Run one gate DB operation with busy retry. `Ok(Some(_))` on success; on
/// exhausted busy the run is settled as Fail (best-effort) and `Ok(None)`
/// tells the caller to stop the wait without executing — never `Err`.
/// Non-busy errors propagate unchanged.
fn gate_db_op<T>(
    db: &Database,
    run_id: &str,
    operation: &str,
    f: impl FnMut() -> Result<T>,
) -> Result<Option<T>> {
    match with_busy_retry(operation, f) {
        Ok(value) => Ok(Some(value)),
        Err(error) if is_busy_error(&error) => {
            settle_busy_fail(db, run_id, &error);
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// A busy error as a routable node verdict: `Fail` with the busy message
/// as both output `error` and summary, so it travels the normal fail
/// edges. Errors already carrying the retry helper's op-naming context
/// keep it; a raw busy from an unwrapped run-path read is normalized to
/// the same transient shape FR3 requires.
pub(crate) fn busy_fail_execution(error: &anyhow::Error) -> NodeExecution {
    let message = error.to_string();
    let message = if message.starts_with("transient database busy") {
        message
    } else {
        format!("transient database busy on the run path; safe to retry the node: {message}")
    };
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: busy_fail_output(&message),
        summary: message,
    }
}

/// Persist one node-verdict write with busy retry. On success the pending
/// `execution` returns unchanged; an exhausted busy settles Fail
/// best-effort and returns the busy Fail verdict instead — never `Err`.
/// Non-busy errors propagate unchanged.
pub(crate) fn settle_node_verdict(
    db: &Database,
    run_id: &str,
    execution: NodeExecution,
    operation: &'static str,
) -> Result<NodeExecution> {
    let NodeExecution {
        status,
        output,
        summary,
    } = execution;
    match with_busy_retry(operation, || {
        db.update_graph_run_result(run_id, status, Some(&output), Some(chrono::Utc::now()))
            .map(|_| ())
    }) {
        Ok(()) => Ok(NodeExecution {
            status,
            output,
            summary,
        }),
        Err(error) if is_busy_error(&error) => {
            settle_busy_fail(db, run_id, &error);
            Ok(busy_fail_execution(&error))
        }
        Err(error) => Err(error),
    }
}

/// Persist one piece of run bookkeeping (spec heads, blocker annotations,
/// retry markers) with busy retry. `Ok(Some(_))` on success; an exhausted
/// busy warns and returns `Ok(None)` so the caller continues with
/// in-memory state — bookkeeping must never rewrite the verdict or kill
/// the graph. Non-busy errors propagate unchanged.
pub(crate) fn persist_bookkeeping<T>(
    operation: &'static str,
    f: impl FnMut() -> Result<T>,
) -> Result<Option<T>> {
    match with_busy_retry(operation, f) {
        Ok(value) => Ok(Some(value)),
        Err(error) if is_busy_error(&error) => {
            tracing::warn!("{error:#}; continuing with in-memory state");
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Backstop for the spec loop: a busy error escaping a cursor step becomes
/// a routed Fail step instead of an engine-fatal graph verdict. `run_id` is
/// `None` (the ensemble-join shape — downstream skips a `None` run), and
/// per-site settles already persisted what they could. Non-busy results
/// pass through untouched.
pub(crate) fn busy_backstop_flow(
    result: Result<SpecStepFlow>,
    from_node_id: &str,
) -> Result<SpecStepFlow> {
    match result {
        Err(error) if is_busy_error(&error) => {
            tracing::warn!("run-path database busy exhausted; routing fail edge: {error:#}");
            Ok(SpecStepFlow::Routed(NodeStepResult {
                execution: busy_fail_execution(&error),
                from_node_id: from_node_id.to_string(),
                run_id: None,
                had_infra_crash: false,
            }))
        }
        other => other,
    }
}

impl GraphEngine {
    /// A spec that terminates because a FAILING node had no outgoing edge
    /// leaves no blocker anywhere a human can see unless something writes
    /// one — see the module-level defect this closes. Writes a derived
    /// blocker onto the terminating run's `output.blocker`, the exact key
    /// `graph_run_blocker` (daemon/handler.rs) already reads for `graph`
    /// and `graph`'s `blocked`/`blocker` fields, so no reader needs to
    /// change. Never invoked for a PASSING termination (the normal, correct
    /// end of a spec — see `Check committed`) and never overwrites a
    /// blocker `graph_report_blocker` already recorded, since that text is
    /// more specific than anything derived here.
    pub(crate) fn record_terminal_blocker(
        &self,
        run_id: &str,
        node_name: &str,
        final_execution: &NodeExecution,
    ) -> anyhow::Result<()> {
        if final_execution.output.get("blocker").is_some() {
            return Ok(());
        }
        let blocker = format!(
            "Spec terminated: node '{}' ended '{}' with no outgoing edge for that status. {}",
            node_name,
            final_execution.status.as_str(),
            final_execution.summary,
        );
        let mut output = final_execution.output.clone();
        match output.as_object_mut() {
            Some(map) => {
                map.insert("blocker".to_string(), Value::String(blocker));
            }
            None => output = serde_json::json!({ "blocker": blocker }),
        }
        let _ = persist_bookkeeping("update_graph_run_result", || {
            self.db
                .update_graph_run_result(run_id, final_execution.status, Some(&output), None)
        })?;
        Ok(())
    }

    /// Overwrite (not merge-and-preserve, unlike [`Self::record_terminal_blocker`])
    /// `run_id`'s `output.blocker` with C19's cross-run budget text. Called
    /// only once [`Self::record_spec_attempt`] has confirmed the budget is
    /// actually exceeded, at which point this attempt's blocker is strictly
    /// more informative than whatever dead-end text (if any) is already
    /// there — naming the spec, the attempt count, and the last failure
    /// rather than just this one node.
    pub(crate) fn set_run_blocker(
        &self,
        run_id: &str,
        status: GraphRunStatus,
        output: &Value,
        blocker: &str,
    ) -> Result<()> {
        let mut merged = output.clone();
        match merged.as_object_mut() {
            Some(map) => {
                map.insert("blocker".to_string(), Value::String(blocker.to_string()));
            }
            None => merged = serde_json::json!({ "blocker": blocker }),
        }
        let _ = persist_bookkeeping("update_graph_run_result", || {
            self.db
                .update_graph_run_result(run_id, status, Some(&merged), None)
        })?;
        Ok(())
    }
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
        let Some(granted) = gate_db_op(db, run_id, "try_acquire_mutex_set", || {
            db.try_acquire_mutex_set(run_id, &lp.id, &node.name, resources)
        })?
        else {
            return Ok(false);
        };
        if granted {
            // Granted — unless someone finalized the run between the grant
            // and this mark (an interrupt racing us), in which case the
            // leases go back and this dispatch stops.
            let Some(started) =
                gate_db_op(db, run_id, "mark_graph_run_started_from_waiting", || {
                    db.mark_graph_run_started_from_waiting(run_id)
                })?
            else {
                return Ok(false);
            };
            if !started {
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
        let run = match gate_db_op(db, run_id, "get_graph_run", || db.get_graph_run(run_id))? {
            None => return Ok(false),
            Some(None) => return Ok(false),
            Some(Some(run)) => run,
        };
        if run.status != GraphRunStatus::Waiting {
            return Ok(false);
        }
        if shutdown.load(Ordering::SeqCst) {
            let _ = gate_db_op(db, run_id, "update_graph_run_result", || {
                db.update_graph_run_result(
                    run_id,
                    GraphRunStatus::Fail,
                    Some(&super::routing::shutdown_interrupted_output()),
                    Some(chrono::Utc::now()),
                )
            })?;
            return Ok(false);
        }
        let graph_live = match gate_db_op(db, run_id, "get_graph", || db.get_graph(&lp.id))? {
            None => return Ok(false),
            Some(maybe) => maybe.is_some_and(|current| {
                matches!(current.status, GraphStatus::Running | GraphStatus::Pausing)
            }),
        };
        if !graph_live {
            let _ = gate_db_op(db, run_id, "update_graph_run_result", || {
                db.update_graph_run_result(
                    run_id,
                    GraphRunStatus::Fail,
                    Some(&mutex_wait_terminated_output(
                        "graph stopped while this node was waiting for its mutex",
                    )),
                    Some(chrono::Utc::now()),
                )
            })?;
            return Ok(false);
        }
        if !announced_waiting {
            announced_waiting = true;
            if !announce_mutex_wait(db, lp, spec_id, node, run_id, resources)? {
                return Ok(false);
            }
        }
        let _ = tokio::time::timeout(Duration::from_secs(30), db.mutex_notified()).await;
    }
}

/// Record the one `mutex_waiting` event for a wait, carrying who holds each
/// requested resource (the same `waiting_for` shape the read surfaces show).
/// Returns `false` when an exhausted-busy settle stopped the wait — the
/// caller must stop without executing.
fn announce_mutex_wait(
    db: &Database,
    lp: &Graph,
    spec_id: &str,
    node: &GraphNode,
    run_id: &str,
    resources: &[String],
) -> Result<bool> {
    let Some(waiting_for) = gate_db_op(db, run_id, "get_graph_run_waiting_for", || {
        db.get_graph_run_waiting_for(run_id)
    })?
    else {
        return Ok(false);
    };
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
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::tests::{graph_fixture, insert_run_row};

    fn busy_error() -> anyhow::Error {
        anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy,
                extended_code: 5,
            },
            None,
        ))
    }

    /// A busy error carrying the retry helper's context keeps its
    /// operation-naming message as the Fail verdict.
    #[test]
    fn busy_fail_execution_keeps_op_context() {
        let error = anyhow::anyhow!(
            "transient database busy during update_graph_run_result after 3 attempts; safe to retry the node"
        );
        let execution = busy_fail_execution(&error);
        assert_eq!(execution.status, GraphRunStatus::Fail);
        assert!(
            execution.summary.contains("update_graph_run_result"),
            "must name the operation: {}",
            execution.summary
        );
        assert!(
            execution.summary.contains("transient"),
            "must say transient: {}",
            execution.summary
        );
        assert_eq!(
            execution.output.get("error").and_then(Value::as_str),
            Some(execution.summary.as_str())
        );
    }

    /// A raw busy (an unwrapped run-path read) is normalized to the same
    /// transient shape instead of leaking "database is locked".
    #[test]
    fn busy_fail_execution_normalizes_raw_busy() {
        let execution = busy_fail_execution(&busy_error());
        assert_eq!(execution.status, GraphRunStatus::Fail);
        assert!(
            execution.summary.starts_with("transient database busy"),
            "must be transient-shaped: {}",
            execution.summary
        );
        assert!(
            execution.summary.contains("safe to retry"),
            "must say safe to retry: {}",
            execution.summary
        );
    }

    /// An exhausted busy on a verdict settle becomes a Fail verdict carrying
    /// the operation and transient wording — never `Err` — and the retried
    /// best-effort Fail row lands once the holder releases.
    #[test]
    fn settle_node_verdict_converts_exhausted_busy_to_fail() {
        let (dir, db, _engine, graph_id, spec_id) = graph_fixture().unwrap();
        insert_run_row(&db, &graph_id, &spec_id, "node-settle", 0, 1).unwrap();
        let run_id = format!("run-{spec_id}-1");
        let db_path = dir.path().join("test.db");
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.busy_timeout(std::time::Duration::from_secs(30))
                .unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            let _ = held_tx.send(());
            std::thread::sleep(std::time::Duration::from_secs(20));
            conn.execute_batch("ROLLBACK").unwrap();
        });
        held_rx.recv().expect("holder took the write lock");
        // Under the hold every attempt waits out the 5 s busy timeout, so
        // the ~15 s budget exhausts deterministically inside the 20 s hold.
        let execution = NodeExecution {
            status: GraphRunStatus::Pass,
            output: serde_json::json!({}),
            summary: "pass".to_string(),
        };
        let settled =
            settle_node_verdict(&db, &run_id, execution, "update_graph_run_result").unwrap();
        assert_eq!(settled.status, GraphRunStatus::Fail);
        assert!(
            settled.summary.contains("update_graph_run_result"),
            "busy Fail must name the operation: {}",
            settled.summary
        );
        assert!(
            settled.summary.contains("transient"),
            "busy Fail must say transient: {}",
            settled.summary
        );
        holder.join().unwrap();
        let row = db.get_graph_run(&run_id).unwrap().unwrap();
        assert_eq!(row.status, GraphRunStatus::Fail);
    }

    /// Bookkeeping warns and continues past an exhausted busy.
    #[test]
    fn persist_bookkeeping_continues_past_exhausted_busy() {
        let mut calls = 0;
        let result = persist_bookkeeping("test_op", || -> Result<i32> {
            calls += 1;
            Err(busy_error())
        });
        assert!(result.unwrap().is_none());
        assert_eq!(calls, 3);
    }

    /// Non-busy bookkeeping errors propagate unchanged, with no retry.
    #[test]
    fn persist_bookkeeping_passes_non_busy_through() {
        let mut calls = 0;
        let result = persist_bookkeeping("test_op", || -> Result<i32> {
            calls += 1;
            Err(anyhow::anyhow!("boom"))
        });
        assert!(result.unwrap_err().to_string().contains("boom"));
        assert_eq!(calls, 1);
    }

    /// A busy error escaping a cursor step becomes a routed Fail step from
    /// the same node — the loop backstop, never an engine-fatal error.
    #[test]
    fn busy_backstop_flow_routes_fail_on_busy() {
        let error = with_busy_retry("insert_graph_run", || -> Result<()> { Err(busy_error()) })
            .unwrap_err();
        let flow = busy_backstop_flow(Err(error), "node-1").unwrap();
        let SpecStepFlow::Routed(step) = flow else {
            panic!("a busy step error must route, not finish");
        };
        assert_eq!(step.from_node_id, "node-1");
        assert!(step.run_id.is_none());
        assert_eq!(step.execution.status, GraphRunStatus::Fail);
        assert!(
            step.execution.summary.contains("insert_graph_run"),
            "must name the operation: {}",
            step.execution.summary
        );
        assert!(
            step.execution.summary.contains("transient"),
            "must say transient: {}",
            step.execution.summary
        );
    }

    /// Non-busy step errors pass through the backstop untouched.
    #[test]
    fn busy_backstop_flow_passes_non_busy_through() {
        let result = busy_backstop_flow(Err(anyhow::anyhow!("boom")), "node-1");
        let Err(error) = result else {
            panic!("a non-busy step error must pass through");
        };
        assert!(error.to_string().contains("boom"));
    }
}
