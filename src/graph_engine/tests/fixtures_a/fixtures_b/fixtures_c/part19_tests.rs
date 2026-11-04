//! Part 19 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

#[tokio::test]
async fn directed_queue_resume_runs_spec2_from_named_check_with_spec_id() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());

    let s1 = {
        let mut s = standalone_spec("s1", 1);
        s.status = GraphSpecStatus::Pending;
        s
    };
    let s2 = {
        let mut s = standalone_spec("s2", 2);
        s.status = GraphSpecStatus::Failed;
        s
    };
    let s3 = {
        let mut s = standalone_spec("s3", 3);
        s.status = GraphSpecStatus::Pending;
        s
    };
    db.insert_graph_spec(&s1).unwrap();
    db.insert_graph_spec(&s2).unwrap();
    db.insert_graph_spec(&s3).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&s1.id, &s2.id, &s3.id]);

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: s2.id.clone(),
                node_arg: "check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    // The FIRST node_started of the whole dispatch is the operator-named
    // check node on spec 2 — no earlier queue member may run first.
    let first_node_started = events
        .iter()
        .find(|e| e.kind == "node_started")
        .expect("the dispatch must start a node");
    assert_eq!(
        first_node_started.spec_id.as_deref(),
        Some(s2.id.as_str()),
        "the first node_started must be on the directed spec s2: {first_node_started:?}"
    );
    assert_eq!(
        first_node_started.node_id.as_deref(),
        Some("graph-check"),
        "the first node_started must be the operator-named check node: {first_node_started:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| { e.kind == "node_started" && e.spec_id.as_deref() == Some(s1.id.as_str()) }),
        "s1 sits before the directed spec in the queue and must never run"
    );
}

#[tokio::test]
async fn directed_queue_resume_runs_spec2_from_named_check_without_spec_id() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());

    let s1 = {
        let mut s = standalone_spec("s1", 1);
        s.status = GraphSpecStatus::Pending;
        s
    };
    let s2 = {
        let mut s = standalone_spec("s2", 2);
        s.status = GraphSpecStatus::Failed;
        s
    };
    let s3 = {
        let mut s = standalone_spec("s3", 3);
        s.status = GraphSpecStatus::Pending;
        s
    };
    db.insert_graph_spec(&s1).unwrap();
    db.insert_graph_spec(&s2).unwrap();
    db.insert_graph_spec(&s3).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&s1.id, &s2.id, &s3.id]);

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();

    // No spec_id supplied: the directed default must be the Failed member
    // (s2), not the first Pending one (s1), so a bare
    // `graph_control run_from_node <node>` restarts the failed spec.
    let resolved = crate::daemon::handler_run_exec::resolve_directed_spec(&db, &graph_id, None)
        .expect("resolve_directed_spec without spec_id returns the failed member");
    assert_eq!(
        resolved.id, s2.id,
        "the directed default must be the failed member s2"
    );

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: resolved.id,
                node_arg: "check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    // The FIRST node_started of the whole dispatch is the operator-named
    // check node on spec 2 — no earlier queue member may run first.
    let first_node_started = events
        .iter()
        .find(|e| e.kind == "node_started")
        .expect("the dispatch must start a node");
    assert_eq!(
        first_node_started.spec_id.as_deref(),
        Some(s2.id.as_str()),
        "the first node_started must be on the directed spec s2: {first_node_started:?}"
    );
    assert_eq!(
        first_node_started.node_id.as_deref(),
        Some("graph-check"),
        "the first node_started must be the operator-named check node: {first_node_started:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| { e.kind == "node_started" && e.spec_id.as_deref() == Some(s1.id.as_str()) }),
        "s1 sits before the directed spec in the queue and must never run"
    );
}

#[tokio::test]
async fn directed_queue_resume_runs_spec2_then_spec3_in_order() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());

    let s1 = {
        let mut s = standalone_spec("s1", 1);
        s.status = GraphSpecStatus::Pending;
        s
    };
    let s2 = {
        let mut s = standalone_spec("s2", 2);
        s.status = GraphSpecStatus::Failed;
        s
    };
    let s3 = {
        let mut s = standalone_spec("s3", 3);
        s.status = GraphSpecStatus::Pending;
        s
    };
    db.insert_graph_spec(&s1).unwrap();
    db.insert_graph_spec(&s2).unwrap();
    db.insert_graph_spec(&s3).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&s1.id, &s2.id, &s3.id]);

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::resume_from(DirectedStart {
                spec_id: s2.id.clone(),
                node_arg: "check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    // s2 emits two spec_started events (the plain one and the
    // directed-start one) — positions over the raw event list, so the
    // assertion is about the real dispatch order.
    let pos = |kind: &str, spec_id: &str| {
        events
            .iter()
            .position(|e| e.kind == kind && e.spec_id.as_deref() == Some(spec_id))
    };
    let s2_started = pos("spec_started", &s2.id).expect("s2 must have a spec_started event");
    let s3_started = pos("spec_started", &s3.id).expect("s3 must have a spec_started event");
    assert!(s2_started < s3_started, "s2 must start before s3");
    // Spec 3 starts only after spec 2 ENDS.
    let s2_ended = pos("spec_completed", &s2.id).expect("s2 must complete");
    assert!(
        s2_ended < s3_started,
        "spec 3 must start only after spec 2 ends: s2 completed at {s2_ended}, \
             s3 started at {s3_started}"
    );
    assert!(
        pos("spec_started", &s1.id).is_none(),
        "s1 must be skipped, not started"
    );

    let s1_after = db.get_graph_spec(&s1.id).unwrap().unwrap();
    assert_eq!(s1_after.status, GraphSpecStatus::Skipped);
    // Later queue members keep their order and status: both run to
    // completion, s2 first, s3 second.
    assert_eq!(
        db.get_graph_spec(&s2.id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "s2 must finish before the queue moves on"
    );
    assert_eq!(
        db.get_graph_spec(&s3.id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "s3 must run after s2 and complete"
    );
}
