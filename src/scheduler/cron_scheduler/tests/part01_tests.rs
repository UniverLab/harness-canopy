//! Part 01 of `tests` (split from `src/scheduler/cron_scheduler.rs`, pure move).
use super::*;

/// A future `autorun_at` must not launch the graph — it's a schedule, not
/// an immediate action.
#[tokio::test]
async fn fire_due_autorun_graphs_ignores_future_schedule() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    db.insert_graph(&sample_graph("future-autorun", GraphStatus::Failed))
        .unwrap();
    db.schedule_graph_autorun("future-autorun", Utc::now() + chrono::Duration::hours(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp = db.get_graph("future-autorun").unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed, "must not launch yet");
    assert!(
        lp.autorun_at.is_some(),
        "future autorun_at must remain pending"
    );
}

/// A past-due `autorun_at` on a fireable (`failed`) graph must launch it
/// once and clear the schedule — the one-shot semantics from the spec.
#[tokio::test]
async fn fire_due_autorun_graphs_fires_past_schedule_once_and_clears_it() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    db.insert_graph(&sample_graph("past-autorun", GraphStatus::Failed))
        .unwrap();
    db.schedule_graph_autorun("past-autorun", Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp = db.get_graph("past-autorun").unwrap().unwrap();
    assert!(
        lp.autorun_at.is_none(),
        "firing must clear autorun_at (one-shot)"
    );

    // Firing again must be a no-op: the schedule is already cleared, so
    // it must not appear among pending autorun graphs anymore.
    let pending = db.list_pending_autorun_graphs().unwrap();
    assert!(
        pending.iter().all(|l| l.id != "past-autorun"),
        "graph must not remain pending after firing once"
    );
}

/// B41: a schedule cancelled via `clear_graph_autorun` (the path
/// `graph_schedule_autorun` with `at` omitted takes) must not fire later,
/// even past its original due time — cancelling must actually prevent
/// the wake-up, not just delay it.
#[tokio::test]
async fn fire_due_autorun_graphs_never_fires_a_cancelled_schedule() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    db.insert_graph(&sample_graph("cancelled-autorun", GraphStatus::Failed))
        .unwrap();
    db.schedule_graph_autorun(
        "cancelled-autorun",
        Utc::now() - chrono::Duration::minutes(1),
    )
    .unwrap();
    db.clear_graph_autorun("cancelled-autorun").unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp = db.get_graph("cancelled-autorun").unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Failed,
        "a cancelled schedule must not auto-reset/resume the graph"
    );
    assert!(lp.autorun_at.is_none());
}

/// A `Running`/`Paused` graph must not be relaunched by its own
/// `autorun_at`, even if it's past due — that would spawn a duplicate
/// execution over the same graph.
#[tokio::test]
async fn fire_due_autorun_graphs_skips_running_and_paused_graphs() {
    use crate::domain::graphs::GraphStatus;

    for status in [GraphStatus::Running, GraphStatus::Paused] {
        let (db, scheduler) = test_scheduler_with_graphs();
        let id = format!("busy-autorun-{}", status.as_str());
        db.insert_graph(&sample_graph(&id, status)).unwrap();
        db.schedule_graph_autorun(&id, Utc::now() - chrono::Duration::minutes(1))
            .unwrap();

        scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

        let lp = db.get_graph(&id).unwrap().unwrap();
        assert_eq!(lp.status, status, "status must be untouched");
        assert!(
            lp.autorun_at.is_some(),
            "{status:?} graph must not have its autorun_at cleared"
        );
    }
}

/// `graph_run` refuses a `failed` graph directly, so firing autorun on one
/// must not call `start_background` on it as-is. Instead it must go
/// through the same reset transition as `graph_reset`
/// ([`Database::reset_graph`]) and then resume — the resilience pattern a
/// quota-failed graph relies on to revive itself unattended.
#[tokio::test]
async fn fire_due_autorun_graphs_resets_failed_graph_through_shared_path_and_resumes() {
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };

    let (db, scheduler) = test_scheduler_with_graphs();
    // A real, existing workdir: the resumed run's check node actually
    // spawns a shell in it, unlike the other autorun tests which only
    // assert on synchronous state and never let the graph engine run.
    let workdir = tempfile::tempdir().unwrap();
    let graph_id = "failed-autorun".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Autorun test graph".to_string(),
        description: None,
        workdir: workdir.path().to_string_lossy().to_string(),
        status: GraphStatus::Failed,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
            id: "spec-1".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "Spec 1".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Failed,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
})
        .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    // The reset happens synchronously, before the resumed run is spawned
    // in the background.
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(lp.autorun_at.is_none(), "firing must clear autorun_at");
    assert_ne!(
        lp.status,
        GraphStatus::Failed,
        "the graph must be reset off `failed` before resuming"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        if lp.status == GraphStatus::Completed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resumed run did not complete in time; graph status is {:?}",
            lp.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let spec = db.get_graph_spec("spec-1").unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Completed,
        "the auto-resumed run must have actually executed the spec's graph"
    );
}

/// CB27: the unattended quota autorun must preserve administratively
/// skipped specs the same way a manual blanket reset does.
#[tokio::test]
async fn fire_due_autorun_graphs_preserves_admin_skipped_specs() {
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };

    let (db, scheduler) = test_scheduler_with_graphs();
    let workdir = tempfile::tempdir().unwrap();
    let graph_id = "failed-autorun-admin-skip".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Autorun admin-skip test".to_string(),
        description: None,
        workdir: workdir.path().to_string_lossy().to_string(),
        status: GraphStatus::Failed,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    // One failed spec — will be reset and resumed.
    db.insert_graph_spec(&GraphSpec {
            id: "spec-failed".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "Failed spec".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Failed,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
})
        .unwrap();
    // One admin-skipped spec — must survive the autorun reset.
    db.insert_graph_spec(&GraphSpec {
        id: "spec-admin-skipped".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "Admin skipped spec".to_string(),
        description: None,
        position: 2,
        parallelizable: false,
        status: GraphSpecStatus::Skipped,
        started_at: None,
        completed_at: Some(Utc::now()),
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: Some("admin".to_string()),
        completed_via_reason: Some("premise false".to_string()),
        completed_via_at: Some(Utc::now()),
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    // Synchronous assertions — before the background resume can finish.
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(lp.autorun_at.is_none(), "firing must clear autorun_at");

    let admin_skip = db.get_graph_spec("spec-admin-skipped").unwrap().unwrap();
    assert_eq!(
        admin_skip.status,
        GraphSpecStatus::Skipped,
        "admin-skipped spec must survive the autorun reset"
    );
    assert_eq!(
        admin_skip.completed_via.as_deref(),
        Some("admin"),
        "admin provenance must be preserved"
    );

    let failed_spec = db.get_graph_spec("spec-failed").unwrap().unwrap();
    assert_ne!(
        failed_spec.status,
        GraphSpecStatus::Failed,
        "the failed spec must be reset off Failed"
    );

    // Wait for the resumed run to complete.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        if lp.status == GraphStatus::Completed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resumed run did not complete in time; graph status is {:?}",
            lp.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // Final assertion — the admin skip must still be skipped after the run.
    let admin_skip = db.get_graph_spec("spec-admin-skipped").unwrap().unwrap();
    assert_eq!(
        admin_skip.status,
        GraphSpecStatus::Skipped,
        "admin-skipped spec must remain skipped after the resumed run"
    );
}
/// `Running` when the daemon dies uncleanly; `reconcile_orphaned_graphs`
/// (as it would run at the next boot) pauses it and marks its dangling
/// run interrupted; the resilience node's `graph_schedule_autorun` is
/// already due by the time the scheduler next evaluates it. Before this
/// fix, `is_autorun_due` excluded every `Paused` graph unconditionally, so
/// this schedule would sit forever and never fire. It must fire now,
/// through the same `resume_background` path a manual `graph_continue`
/// takes, and actually finish the interrupted spec.
#[tokio::test]
async fn fire_due_autorun_graphs_fires_reconciliation_paused_graph() {
    use crate::domain::graphs::{GraphNodeRun, GraphRunStatus, GraphSpecStatus};

    let (db, scheduler) = test_scheduler_with_graphs();
    let workdir = tempfile::tempdir().unwrap();
    let data_dir = tempfile::tempdir().unwrap();
    let graph_id = "reconciled-autorun".to_string();

    let mut lp = sample_graph(&graph_id, GraphStatus::Running);
    lp.workdir = workdir.path().to_string_lossy().to_string();
    db.insert_graph(&lp).unwrap();

    let spec = crate::domain::graphs::GraphSpec {
            id: "spec-reconciled".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "Spec 1".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
};
    db.insert_graph_spec(&spec).unwrap();
    db.update_graph_spec_status(&spec.id, GraphSpecStatus::Running, Some(Utc::now()), None)
        .unwrap();

    let node = crate::domain::graphs::GraphNode {
        id: "node-reconciled".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: crate::domain::graphs::GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();

    // The run left dangling by the daemon that died uncleanly.
    db.insert_graph_run(&GraphNodeRun {
        id: "run-reconciled".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    // The next boot's reconciliation pass — this is what must leave the
    // graph resumable, not the test hand-setting `paused_by_reconciliation`.
    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );
    let lp_after_reconcile = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp_after_reconcile.status, GraphStatus::Paused);
    assert!(
        lp_after_reconcile.paused_by_reconciliation,
        "reconciliation must flag its own pause"
    );

    // The resilience node's scheduled resume, already due.
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp_fired = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        lp_fired.autorun_at.is_none(),
        "firing must clear autorun_at"
    );

    // Unlike the `failed` case, there's no synchronous reset step here —
    // `resume_background`'s spawned task is what actually claims the
    // graph off `Paused` (via `claim_graph_for_run`). Prove it left
    // `Paused` by waiting for the resumed run to actually complete.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        if lp.status == GraphStatus::Completed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resumed run did not complete in time; graph status is {:?}",
            lp.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(
        spec_after.status,
        GraphSpecStatus::Completed,
        "the auto-resumed run must have actually executed the interrupted spec"
    );
}

/// C1: firing on a reconciliation-paused graph must clear `autorun_at`
/// exactly like every other autorun fire — a second scheduler tick must
/// not resume it again.
#[tokio::test]
async fn fire_due_autorun_graphs_on_reconciliation_paused_graph_is_one_shot() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let graph_id = "reconciled-once".to_string();
    let mut lp = sample_graph(&graph_id, GraphStatus::Paused);
    lp.paused_by_reconciliation = true;
    db.insert_graph(&lp).unwrap();
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();
    let pending = db.list_pending_autorun_graphs().unwrap();
    assert!(
        pending.iter().all(|l| l.id != graph_id),
        "firing must clear autorun_at so a second tick can't resume it again"
    );
}

/// C1: a due `autorun_at` on a graph the operator paused (not
/// reconciliation) must never fire — `graph_pause`/`graph_report_blocker`
/// both route through `update_graph_status`, which clears
/// `paused_by_reconciliation`, so this is the default state of any
/// `Paused` graph that didn't come through reconciliation.
#[tokio::test]
async fn fire_due_autorun_graphs_never_fires_on_an_operator_paused_graph() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let graph_id = "operator-paused-autorun".to_string();
    let lp = sample_graph(&graph_id, GraphStatus::Paused);
    assert!(!lp.paused_by_reconciliation);
    db.insert_graph(&lp).unwrap();
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp_after = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp_after.status, GraphStatus::Paused, "must stay paused");
    assert!(
        lp_after.autorun_at.is_some(),
        "operator pause must not be silently discarded either — the schedule stays pending"
    );
}

/// C19: a graph the engine itself paused because a spec exceeded its
/// persisted cross-run attempt budget must not be silently relaunched by
/// a pending autorun schedule either — it needs a human, exactly like
/// any other blocker. `GraphEngine::block_graph` reaches this state
/// through the exact same `update_graph_status(..., Paused, ...)` path
/// as an operator pause (clearing `paused_by_reconciliation` on every
/// call), so `is_autorun_due` already refuses it — this exercises that
/// through real execution rather than a hand-built `Graph` row.
#[tokio::test]
async fn fire_due_autorun_graphs_never_fires_a_c19_blocked_graph() {
    use crate::domain::graphs::{Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus};

    let (db, scheduler, graph_engine) = test_scheduler_and_engine();
    let workdir = tempfile::tempdir().unwrap();
    let graph_id = "c19-blocked-autorun".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "C19 blocked autorun test".to_string(),
        description: None,
        workdir: workdir.path().to_string_lossy().to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();

    let spec_id = "c19-blocked-spec".to_string();
    db.insert_graph_spec(&GraphSpec {
        id: spec_id.clone(),
        graph_id: Some(graph_id.clone()),
        name: "Spec".to_string(),
        description: Some(
            "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\n\
                 Objective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\n\
                 Out of Scope:\n- G"
                .to_string(),
        ),
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "dead-end".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "dead-end".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0",
        }),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    // No outgoing edge from "dead-end": one genuine Fail is enough to
    // exceed the attempt limit of 1 this fixture set up.

    graph_engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "must be blocked, not failed"
    );
    assert!(!lp.paused_by_reconciliation);

    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp_after = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp_after.status,
        GraphStatus::Paused,
        "a C19-blocked graph must not autorun"
    );
    assert!(
        lp_after.autorun_at.is_some(),
        "the pending schedule stays pending until the blocker is cleared"
    );
}

/// The exact incident this spec fixes: a graph was launched with
/// `graph_run { queue_id }`, failed mid-queue (e.g. a quota error), and its
/// `graph_schedule_autorun` fired to revive it. Before this fix, autorun
/// resumed the graph with its own bound specs — empty for a queue run — so
/// the engine found nothing to do and marked the graph `completed` with
/// queue members still pending. Firing autorun now must reset and resume
/// against the *same queue*, in queue order, until it's genuinely done.
#[tokio::test]
async fn fire_due_autorun_graphs_resumes_same_queue_after_failed_run() {
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    use crate::domain::queues::Queue;

    let (db, scheduler) = test_scheduler_with_graphs();
    let workdir = tempfile::tempdir().unwrap();
    let graph_id = "failed-queue-autorun".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Autorun queue test graph".to_string(),
        description: None,
        workdir: workdir.path().to_string_lossy().to_string(),
        status: GraphStatus::Failed,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: Some("queue-1".to_string()),
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();

    let standalone = |id: &str, position: i64, status: GraphSpecStatus| GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: None,
        position,
        parallelizable: false,
        status,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    };
    db.insert_graph_spec(&standalone("queue-done", 1, GraphSpecStatus::Completed))
        .unwrap();
    // Left `failed` by the run that hit quota mid-queue — never explicitly
    // reset, unlike the graph's own status.
    db.insert_graph_spec(&standalone("queue-failed", 2, GraphSpecStatus::Failed))
        .unwrap();
    db.insert_graph_spec(&standalone("queue-pending", 3, GraphSpecStatus::Pending))
        .unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    for spec_id in ["queue-done", "queue-failed", "queue-pending"] {
        db.append_queue_member("queue-1", spec_id, None).unwrap();
    }
    // No bound specs on the graph itself — this is what the real incident
    // hit: a `graph_run { queue_id }` launch never binds specs to the graph.
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(lp.autorun_at.is_none(), "firing must clear autorun_at");
    assert_ne!(
        lp.status,
        GraphStatus::Failed,
        "the graph must be reset off `failed` before resuming"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        if lp.status == GraphStatus::Completed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resumed queue run did not complete in time; graph status is {:?}",
            lp.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // All three queue members ran to completion via the *same* queue — not
    // a false completion with them left pending.
    for spec_id in ["queue-done", "queue-failed", "queue-pending"] {
        let spec = db.get_graph_spec(spec_id).unwrap().unwrap();
        assert_eq!(
            spec.status,
            GraphSpecStatus::Completed,
            "spec '{spec_id}' should have completed via the resumed queue run"
        );
    }
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "the resumed queue run must reach genuine completion"
    );
    // B31: the run context survives genuine completion as last-run data
    // so `graph list` / `graph info` keep rendering the finished graph's
    // real n/n queue progress. B8's anti-pollution guarantee is upheld at
    // launch time (every path re-persists this before the first spec),
    // not by clearing it on completion.
    assert_eq!(
        lp.active_run_queue_id.as_deref(),
        Some("queue-1"),
        "a genuinely finished queue run keeps the persisted run context for progress display"
    );
}

/// CM29: a predecessor spec left the shared workdir dirty on a failed
/// attempt; autorun on a *different* graph pointed at the same workdir
/// must refuse to launch rather than pile onto the mess. The one-shot
/// schedule is still cleared (never retried automatically) — a human
/// must relaunch manually once the tree is resolved.
#[tokio::test]
async fn fire_due_autorun_graphs_refuses_dirty_predecessor_and_stays_cleared() {
    use crate::domain::graphs::{GraphSpec, GraphSpecStatus, GraphStatus};

    let (db, scheduler) = test_scheduler_with_graphs();
    let git_dir = tempfile::tempdir().unwrap();
    init_git_repo(git_dir.path());
    let workdir = git_dir.path().to_string_lossy().to_string();

    // A predecessor spec, standalone, tagged with the shared workdir,
    // left `failed` by an earlier (unrelated) attempt.
    db.insert_graph_spec(&GraphSpec {
        id: "pred-spec".to_string(),
        graph_id: None,
        name: "Predecessor".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Failed,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: Some(workdir.clone()),
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    std::fs::write(git_dir.path().join("dirty.txt"), "dirty").unwrap();

    let mut lp = sample_graph("dirty-autorun", GraphStatus::Draft);
    lp.workdir = workdir;
    db.insert_graph(&lp).unwrap();
    db.schedule_graph_autorun(&lp.id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    let after = db.get_graph(&lp.id).unwrap().unwrap();
    assert!(
        after.autorun_at.is_none(),
        "the one-shot schedule must still be cleared, refused or not"
    );
    assert_eq!(
        after.status,
        GraphStatus::Draft,
        "a refused autorun must never have launched (status would have moved off Draft)"
    );
}

/// Firing autorun on an already-`completed` graph must not silently
/// re-run it — that's a human decision via `graph_reset` + `graph_run`.
/// The scheduler should warn and clear the schedule instead.
#[tokio::test]
async fn fire_due_autorun_graphs_on_completed_graph_warns_and_does_not_run() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    let graph_id = "completed-autorun".to_string();
    db.insert_graph(&sample_graph(&graph_id, GraphStatus::Completed))
        .unwrap();
    db.schedule_graph_autorun(&graph_id, Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_autorun_graphs(Utc::now()).await.unwrap();

    // Give any (unexpected) spawned background run a chance to run.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        lp.autorun_at.is_none(),
        "the one-shot schedule must still be cleared"
    );
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "a completed graph must not be re-run by its own autorun"
    );
}

/// A future `auto_continue_at` must not resume the graph — it's a
/// schedule, not an immediate action.
#[tokio::test]
async fn fire_due_auto_continue_graphs_ignores_future_schedule() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    db.insert_graph(&sample_graph("future-auto-continue", GraphStatus::Paused))
        .unwrap();
    db.schedule_graph_auto_continue(
        "future-auto-continue",
        Utc::now() + chrono::Duration::hours(1),
        None,
    )
    .unwrap();

    scheduler.fire_due_auto_continue_graphs(Utc::now()).unwrap();

    let lp = db.get_graph("future-auto-continue").unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Paused, "must not resume yet");
    assert!(
        lp.auto_continue_at.is_some(),
        "future auto_continue_at must remain pending"
    );
}

/// A schedule cancelled via `clear_graph_auto_continue` must not fire
/// later, even past its original due time.
#[tokio::test]
async fn fire_due_auto_continue_graphs_never_fires_a_cancelled_schedule() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    db.insert_graph(&sample_graph(
        "cancelled-auto-continue",
        GraphStatus::Paused,
    ))
    .unwrap();
    db.schedule_graph_auto_continue(
        "cancelled-auto-continue",
        Utc::now() - chrono::Duration::minutes(1),
        None,
    )
    .unwrap();
    db.clear_graph_auto_continue("cancelled-auto-continue")
        .unwrap();

    scheduler.fire_due_auto_continue_graphs(Utc::now()).unwrap();

    let lp = db.get_graph("cancelled-auto-continue").unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "a cancelled schedule must not resume the graph"
    );
    assert!(lp.auto_continue_at.is_none());
}

/// If the graph is no longer `Paused` by the scheduled time (already
/// continued manually, failed, completed, or running), the schedule must
/// be cleared without resuming it — never double-run.
#[tokio::test]
async fn fire_due_auto_continue_graphs_clears_without_firing_when_not_paused() {
    use crate::domain::graphs::GraphStatus;

    for status in [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ] {
        let (db, scheduler) = test_scheduler_with_graphs();
        let id = format!("not-paused-auto-continue-{}", status.as_str());
        db.insert_graph(&sample_graph(&id, status)).unwrap();
        db.schedule_graph_auto_continue(&id, Utc::now() - chrono::Duration::minutes(1), None)
            .unwrap();

        scheduler.fire_due_auto_continue_graphs(Utc::now()).unwrap();

        let lp = db.get_graph(&id).unwrap().unwrap();
        assert_eq!(
            lp.status, status,
            "{status:?} graph's status must be untouched"
        );
        assert!(
            lp.auto_continue_at.is_none(),
            "{status:?} graph's stale schedule must still be cleared, not left pending forever"
        );
    }
}

/// The functional core of this feature: a `Paused` graph's
/// `auto_continue_at` firing must go straight through the
/// `graph_continue`/`resume_background` path — never `graph_reset` and
/// never a fresh dispatch — preserving the paused cursor. Verified by
/// checking the in-flight spec's status is untouched *synchronously*,
/// right after firing (a reset would flip it to `Pending` immediately,
/// before any background dispatch runs), then letting the real resumed
/// dispatch run to completion.
#[tokio::test]
async fn fire_due_auto_continue_graphs_resumes_paused_graph_without_reset_or_relaunch() {
    use crate::domain::graphs::{
        Graph, GraphNode, GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus,
    };

    let (db, scheduler) = test_scheduler_with_graphs();
    let workdir = tempfile::tempdir().unwrap();
    let graph_id = "paused-auto-continue".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Auto-continue test graph".to_string(),
        description: None,
        workdir: workdir.path().to_string_lossy().to_string(),
        status: GraphStatus::Paused,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
            id: "spec-1".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "Spec 1".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
            ),
            position: 1,
            parallelizable: false,
            // A graph paused mid-spec leaves that spec `running` — `graph_pause`
            // never touches spec status, only the graph's own.
            status: GraphSpecStatus::Running,
            started_at: Some(Utc::now()),
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
})
        .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.schedule_graph_auto_continue(
        &graph_id,
        Utc::now() - chrono::Duration::minutes(1),
        Some("retry_current_node"),
    )
    .unwrap();

    scheduler.fire_due_auto_continue_graphs(Utc::now()).unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        lp.auto_continue_at.is_none(),
        "firing must clear auto_continue_at"
    );

    // `retry_current_node` never mutates the spec, and a reset would have
    // flipped it to `Pending` synchronously, before the background
    // dispatch even starts — so `Running` here proves no reset happened.
    let spec = db.get_graph_spec("spec-1").unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Running,
        "auto-continue must not reset the in-flight spec"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        if lp.status == GraphStatus::Completed {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "resumed run did not complete in time; graph status is {:?}",
            lp.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let spec = db.get_graph_spec("spec-1").unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Completed,
        "the auto-continued run must have actually executed the spec's graph"
    );
}

/// `skip_next_spec` must be honored too, not just the default
/// `retry_current_node` — the scheduler must pass the configured action
/// through exactly like the `graph_continue` MCP tool would.
#[tokio::test]
async fn fire_due_auto_continue_graphs_applies_skip_next_spec_action() {
    use crate::domain::graphs::{Graph, GraphSpec, GraphSpecStatus, GraphStatus};

    let (db, scheduler) = test_scheduler_with_graphs();
    let workdir = tempfile::tempdir().unwrap();
    let graph_id = "paused-auto-continue-skip".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Auto-continue skip test graph".to_string(),
        description: None,
        workdir: workdir.path().to_string_lossy().to_string(),
        status: GraphStatus::Paused,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "spec-skip".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "Spec skip".to_string(),
        description: Some("desc".to_string()),
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Running,
        started_at: Some(Utc::now()),
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.schedule_graph_auto_continue(
        &graph_id,
        Utc::now() - chrono::Duration::minutes(1),
        Some("skip_next_spec"),
    )
    .unwrap();

    scheduler.fire_due_auto_continue_graphs(Utc::now()).unwrap();

    let spec = db.get_graph_spec("spec-skip").unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Skipped,
        "skip_next_spec must mark the in-flight spec skipped, synchronously"
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(lp.auto_continue_at.is_none(), "firing must be one-shot");
}

/// A future `enable_at` must not flip the agent to enabled — it's a
/// schedule, not an immediate action.
#[test]
fn fire_due_enable_at_ignores_future_schedule() {
    let (db, scheduler) = test_scheduler();
    db.upsert_agent(&manual_agent("future-wake", false))
        .unwrap();
    db.schedule_agent_enable("future-wake", Utc::now() + chrono::Duration::hours(1))
        .unwrap();

    scheduler.fire_due_enable_at(Utc::now()).unwrap();

    let agent = db.get_agent("future-wake").unwrap().unwrap();
    assert!(!agent.enabled, "future enable_at must not enable yet");
    assert!(
        agent.enable_at.is_some(),
        "future enable_at must remain pending"
    );
}

/// A past-due `enable_at` must enable the agent and clear the field —
/// the one-shot semantics from the spec.
#[test]
fn fire_due_enable_at_activates_past_schedule_and_clears_it() {
    let (db, scheduler) = test_scheduler();
    db.upsert_agent(&manual_agent("past-wake", false)).unwrap();
    db.schedule_agent_enable("past-wake", Utc::now() - chrono::Duration::minutes(1))
        .unwrap();

    scheduler.fire_due_enable_at(Utc::now()).unwrap();

    let agent = db.get_agent("past-wake").unwrap().unwrap();
    assert!(agent.enabled, "past-due enable_at must enable the agent");
    assert!(
        agent.enable_at.is_none(),
        "activation must clear enable_at (one-shot)"
    );
}

/// `next_sleep_duration` must wake the scheduler for a pending
/// `enable_at`, not just cron expressions — otherwise the one-shot
/// enable would rely on the 60s reconcile fallback instead of firing on
/// time.
#[test]
fn next_sleep_duration_wakes_for_pending_enable_at() {
    let (db, scheduler) = test_scheduler();
    db.upsert_agent(&manual_agent("soon-wake", false)).unwrap();
    db.schedule_agent_enable("soon-wake", Utc::now() + chrono::Duration::seconds(5))
        .unwrap();

    let dur = scheduler.next_sleep_duration();
    assert!(
        dur <= std::time::Duration::from_secs(6),
        "expected to wake in ~5s for the pending enable_at, got {:?}",
        dur
    );
}

/// `next_sleep_duration` must also wake for a pending `auto_continue_at`,
/// same as `autorun_at` — otherwise a deferred paused-graph resume would
/// rely on the reconcile fallback instead of firing on time.
#[test]
fn next_sleep_duration_wakes_for_pending_auto_continue_at() {
    use crate::domain::graphs::GraphStatus;

    let (db, scheduler) = test_scheduler_with_graphs();
    db.insert_graph(&sample_graph("soon-auto-continue", GraphStatus::Paused))
        .unwrap();
    db.schedule_graph_auto_continue(
        "soon-auto-continue",
        Utc::now() + chrono::Duration::seconds(5),
        None,
    )
    .unwrap();

    let dur = scheduler.next_sleep_duration();
    assert!(
        dur <= std::time::Duration::from_secs(6),
        "expected to wake in ~5s for the pending auto_continue_at, got {:?}",
        dur
    );
}

/// B7: a single agent row with a malformed `trigger_config` (e.g. a raw
/// cron string `11 3 11 7 *` written directly to SQLite by an external
/// tool, where JSON `{"type":"cron","schedule_expr":"..."}` is expected)
/// must be quarantined — disabled with one WARN — not left to bail
/// `list_cron_agents` and repeat "Scheduler fire failed" every tick.
#[test]
fn quarantine_corrupt_agents_disables_corrupt_row_and_leaves_healthy_untouched() {
    let (db, scheduler) = test_scheduler();
    db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();
    db.upsert_agent(&manual_agent("healthy-1", true)).unwrap();

    scheduler
        .quarantine_corrupt_agents()
        .expect("must not bail on a corrupt row");

    let corrupt = db.list_corrupt_agents().unwrap();
    assert_eq!(corrupt.len(), 1);
    assert_eq!(corrupt[0].id, "corrupt-1");
    assert!(!corrupt[0].enabled, "corrupt row must be quarantined");

    let healthy = db.get_agent("healthy-1").unwrap().unwrap();
    assert!(healthy.enabled, "healthy agent must be left untouched");
}

/// Quarantining an already-disabled corrupt row must be a no-op — this is
/// what keeps a repeated tick from re-warning about the same row forever.
#[test]
fn quarantine_corrupt_agents_is_idempotent_for_an_already_disabled_row() {
    let (db, scheduler) = test_scheduler();
    db.insert_corrupt_agent_for_test("corrupt-1", false)
        .unwrap();

    scheduler.quarantine_corrupt_agents().unwrap();
    scheduler.quarantine_corrupt_agents().unwrap();

    let corrupt = db.list_corrupt_agents().unwrap();
    assert_eq!(corrupt.len(), 1, "the row itself is untouched, not deleted");
    assert!(!corrupt[0].enabled);
}

/// The actual incident: `fire_due_tasks` (the scheduler tick) must not
/// bail when a corrupt cron row is present — it must quarantine that row
/// and still evaluate/fire the remaining, healthy cron agents.
#[tokio::test]
async fn fire_due_tasks_quarantines_corrupt_row_and_keeps_scheduling_others() {
    let (db, scheduler) = test_scheduler();
    db.insert_corrupt_agent_for_test("corrupt-1", true).unwrap();

    let mut healthy = manual_agent("healthy-1", true);
    healthy.cli = Cli::new("definitely-not-a-real-cli-binary-xyz");
    healthy.trigger = Some(Trigger::Cron {
        schedule_expr: "* * * * *".to_string(),
    });
    db.upsert_agent(&healthy).unwrap();

    scheduler
        .fire_due_tasks()
        .await
        .expect("a corrupt row must not fail the whole tick");

    let corrupt = db.list_corrupt_agents().unwrap();
    assert_eq!(corrupt.len(), 1);
    assert!(!corrupt[0].enabled, "corrupt row must be quarantined");

    {
        let last_fired = scheduler.last_fired.lock().await;
        assert!(
            last_fired.contains_key("healthy-1"),
            "the healthy cron agent must still have been evaluated and fired \
                 despite the corrupt row"
        );
    }

    // A second tick must not re-touch the now-disabled corrupt row (no
    // repeated per-tick warning/write for the same row).
    scheduler.fire_due_tasks().await.unwrap();
    let corrupt_again = db.list_corrupt_agents().unwrap();
    assert_eq!(corrupt_again.len(), 1);
    assert!(!corrupt_again[0].enabled);
}

#[test]
fn test_to_7field_cron() {
    assert_eq!(to_7field_cron("*/5 * * * *"), "0 */5 * * * * *");
    assert_eq!(to_7field_cron("0 9 * * *"), "0 0 9 * * * *");
    assert_eq!(to_7field_cron("0 9 * * 1-5"), "0 0 9 * * 1-5 *");
}

#[test]
fn test_retry_policy_defaults() {
    let d = RetryPolicy::default();
    assert!(d.enabled);
    assert_eq!(d.delay_minutes, 60);
    assert_eq!(d.max_retries, 3);
}
