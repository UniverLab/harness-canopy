//! Part 03 of `tests`: cron ticks on spec-less graphs with a
//! `trigger_prompt` (FR3/FR4) — a due tick launches the idea run, and an
//! overlapping tick skips while a run is still in flight.
use super::*;

/// A spec-less cron graph with a `trigger_prompt` and one top-level
/// check node running `command`.
fn tickable_graph(db: &Database, workdir: &str, command: &str) -> crate::domain::graphs::Graph {
    let mut lp = sample_graph("tick-graph", crate::domain::graphs::GraphStatus::Draft);
    lp.workdir = workdir.to_string();
    lp.trigger = Some(Trigger::Cron {
        schedule_expr: "* * * * *".to_string(),
    });
    lp.trigger_prompt = Some("PING-FROM-TICK sweep the inbox for news".to_string());
    db.insert_graph(&lp).unwrap();
    db.insert_graph_node(&crate::domain::graphs::GraphNode {
        id: "tick-check".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "sweep".to_string(),
        kind: crate::domain::graphs::GraphNodeKind::Check,
        config: serde_json::json!({"command": command, "success_condition": "exit_code_0"}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    lp
}

/// A due tick on a spec-less cron graph with a `trigger_prompt` launches
/// it through the idea-run path: the check node runs and the tick run
/// completes (idle here, since `exit 1` finds nothing to do).
#[tokio::test]
async fn due_tick_launches_spec_less_graph_with_prompt() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let lp = tickable_graph(&db, &workdir, "exit 1");

    scheduler
        .try_fire_graph(&lp, chrono::Local::now())
        .await
        .unwrap();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let status = loop {
        let current = db
            .get_graph(&lp.id)
            .unwrap()
            .expect("graph still exists")
            .status;
        if !matches!(
            current,
            crate::domain::graphs::GraphStatus::Running | crate::domain::graphs::GraphStatus::Draft
        ) {
            break current;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the tick run never settled"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    assert_eq!(status, crate::domain::graphs::GraphStatus::Completed);
    let events = db.list_graph_events(&lp.id, None, None, None, 200).unwrap();
    let completed = events
        .iter()
        .find(|event| event.kind == "graph_completed")
        .expect("a completed tick run records graph_completed");
    let payload: serde_json::Value =
        serde_json::from_str(completed.payload.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(payload["outcome"], "idle");
}

/// A tick never starts a second run while the previous one is still
/// running: with a `running` node row in flight the tick skips — no
/// placeholder spec, no status change.
#[tokio::test]
async fn overlapping_tick_skips_while_run_in_flight() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let dir = tempfile::tempdir().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let lp = tickable_graph(&db, &workdir, "exit 1");
    db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
        intent: None,
        id: "spec-in-flight".to_string(),
        graph_id: Some(lp.id.clone()),
        name: String::new(),
        description: None,
        position: 1,
        parallelizable: false,
        status: crate::domain::graphs::GraphSpecStatus::Running,
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
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
        created_by_session: None,
    })
    .unwrap();
    db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
        id: "run-in-flight".to_string(),
        graph_id: lp.id.clone(),
        spec_id: "spec-in-flight".to_string(),
        node_id: "tick-check".to_string(),
        status: crate::domain::graphs::GraphRunStatus::Running,
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

    scheduler
        .try_fire_graph(&lp, chrono::Local::now())
        .await
        .unwrap();

    let lp = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        crate::domain::graphs::GraphStatus::Draft,
        "an overlapping tick leaves the graph untouched"
    );
    assert_eq!(
        db.list_graph_specs(&lp.id)
            .unwrap()
            .iter()
            .map(|spec| spec.id.clone())
            .collect::<Vec<_>>(),
        vec!["spec-in-flight".to_string()],
        "an overlapping tick creates no placeholder spec"
    );
}
