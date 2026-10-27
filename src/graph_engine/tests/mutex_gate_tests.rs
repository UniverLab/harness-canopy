//! Named-mutex gate tests: end-to-end engine runs proving heavy nodes
//! serialize across graphs, a waiting run's timeout starts at the grant,
//! and every terminal path releases the lease.
use super::super::*;
use super::fixtures_a::*;
use std::time::Duration;

/// A second graph + spec beside the fixture's, sharing the temp workdir
/// (check nodes touch no git state, so sharing is safe here).
fn insert_second_graph(
    db: &Database,
    workdir: &str,
    tag: &str,
) -> anyhow::Result<(String, String)> {
    let graph_id = format!("graph-{tag}");
    let spec_id = format!("spec-{tag}");
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        require_spec_type: false,
        id: graph_id.clone(),
        name: format!("Graph {tag}"),
        description: None,
        workdir: workdir.to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })?;
    db.insert_graph_spec(&GraphSpec {
        id: spec_id.clone(),
        graph_id: Some(graph_id.clone()),
        name: format!("Spec {tag}"),
        description: Some(
            "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G"
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
    })?;
    Ok((graph_id, spec_id))
}

/// One `cpu` check node on `spec_id`.
fn insert_cpu_check(
    db: &Database,
    spec_id: &str,
    node_id: &str,
    command: &str,
    timeout_seconds: Option<u64>,
) {
    let mut config = serde_json::json!({
        "command": command,
        "success_condition": "exit_code_0",
        "mutex": ["cpu"],
    });
    if let Some(secs) = timeout_seconds {
        config["timeout_seconds"] = serde_json::json!(secs);
    }
    db.insert_graph_node(&GraphNode {
        id: node_id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: node_id.to_string(),
        kind: GraphNodeKind::Check,
        config,
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

/// Poll until `graph_id` holds `resource`, or the deadline passes.
async fn poll_lease_for(db: &Database, graph_id: &str, resource: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(state) = db.list_mutex_state() {
            if state
                .leases
                .iter()
                .any(|lease| lease.graph_id == graph_id && lease.resource == resource)
            {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Poll until `graph_id` has a queued waiter, or the deadline passes.
async fn poll_waiter_for(db: &Database, graph_id: &str) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        if let Ok(state) = db.list_mutex_state() {
            if state
                .waiters
                .iter()
                .any(|waiter| waiter.graph_id == graph_id)
            {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

fn latest_run_for_spec(db: &Database, spec_id: &str) -> GraphNodeRun {
    db.list_graph_runs_for_spec(spec_id)
        .unwrap()
        .pop()
        .expect("a run row for the spec")
}

fn assert_mutex_tables_empty(db: &Database) {
    let state = db.list_mutex_state().unwrap();
    assert!(state.leases.is_empty(), "leases: {:?}", state.leases);
    assert!(state.waiters.is_empty(), "waiters: {:?}", state.waiters);
}

/// Two graphs whose `cpu` nodes race still run those nodes strictly one at
/// a time — the loser's run waits (visible as `waiting` with its holder)
/// and starts only after the winner releases.
#[tokio::test]
async fn two_graphs_cpu_serial() {
    let (dir, db, engine, graph_a, spec_a) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let (graph_b, spec_b) = insert_second_graph(&db, &workdir, "b").unwrap();
    let markers = dir.path().join("markers");
    insert_cpu_check(
        &db,
        &spec_a,
        "node-a",
        &format!(
            "echo start-a >> '{}'; sleep 2; echo end-a >> '{}'",
            markers.display(),
            markers.display()
        ),
        None,
    );
    insert_cpu_check(
        &db,
        &spec_b,
        "node-b",
        &format!(
            "echo start-b >> '{}'; sleep 2; echo end-b >> '{}'",
            markers.display(),
            markers.display()
        ),
        None,
    );
    let engine = std::sync::Arc::new(engine);

    let engine_a = Arc::clone(&engine);
    let graph_a_run = graph_a.clone();
    let handle_a = tokio::spawn(async move {
        engine_a
            .run_graph(graph_a_run, None, None, None, None, None)
            .await
    });
    // Let graph A win the race deterministically, then start B into the wait.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let engine_b = Arc::clone(&engine);
    let graph_b_run = graph_b.clone();
    let handle_b = tokio::spawn(async move {
        engine_b
            .run_graph(graph_b_run, None, None, None, None, None)
            .await
    });

    assert!(
        poll_waiter_for(&db, &graph_b).await,
        "graph B must queue behind graph A's cpu lease"
    );
    // While queued, B's run is `waiting` and names A as the holder of `cpu`.
    let waiting_run = db
        .list_graph_runs_for_spec(&spec_b)
        .unwrap()
        .into_iter()
        .find(|run| run.status == GraphRunStatus::Waiting)
        .expect("a waiting run row for graph B");
    let waiting_for = db.get_graph_run_waiting_for(&waiting_run.id).unwrap();
    assert_eq!(waiting_for.len(), 1);
    assert_eq!(waiting_for[0].resource, "cpu");
    assert_eq!(
        waiting_for[0].holder_graph.as_deref(),
        Some(graph_a.as_str())
    );

    handle_a.await.unwrap().unwrap();
    handle_b.await.unwrap().unwrap();

    let text = std::fs::read_to_string(&markers).unwrap();
    let order: Vec<&str> = text.lines().collect();
    assert!(
        order == ["start-a", "end-a", "start-b", "end-b"]
            || order == ["start-b", "end-b", "start-a", "end-a"],
        "cpu nodes overlapped: {order:?}"
    );
    assert_eq!(
        latest_run_for_spec(&db, &spec_a).status,
        GraphRunStatus::Pass
    );
    assert_eq!(
        latest_run_for_spec(&db, &spec_b).status,
        GraphRunStatus::Pass
    );
    assert_mutex_tables_empty(&db);
}

/// A waiting run's timeout starts at the grant, not at the enqueue: the
/// waiter below waits ~4 s for `cpu` yet its 2 s timeout still passes,
/// because only the ~1 s after the grant counts.
#[tokio::test]
async fn timeout_starts_after_grant() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let (graph_b, spec_b) = insert_second_graph(&db, &workdir, "b").unwrap();
    insert_cpu_check(&db, &spec_id, "holder", "sleep 4", None);
    insert_cpu_check(&db, &spec_b, "waiter", "sleep 1", Some(2));
    let engine = std::sync::Arc::new(engine);

    let engine_h = Arc::clone(&engine);
    let graph_h = graph_id.clone();
    let holder = tokio::spawn(async move {
        engine_h
            .run_graph(graph_h, None, None, None, None, None)
            .await
    });
    assert!(
        poll_lease_for(&db, &graph_id, "cpu").await,
        "holder must grab cpu first"
    );
    let engine_w = Arc::clone(&engine);
    let graph_w = graph_b.clone();
    let waiter = tokio::spawn(async move {
        engine_w
            .run_graph(graph_w, None, None, None, None, None)
            .await
    });

    holder.await.unwrap().unwrap();
    waiter.await.unwrap().unwrap();

    let holder_run = latest_run_for_spec(&db, &spec_id);
    let waiter_run = latest_run_for_spec(&db, &spec_b);
    assert_eq!(holder_run.status, GraphRunStatus::Pass);
    // Had the 2 s timeout counted the ~4 s wait, this would be a timeout Fail.
    assert_eq!(
        waiter_run.status,
        GraphRunStatus::Pass,
        "waiter output: {:?}",
        waiter_run.output
    );
    assert!(
        waiter_run.started_at >= holder_run.completed_at.unwrap_or(holder_run.started_at),
        "waiter's clock must start at the grant (after the holder released)"
    );
    assert_mutex_tables_empty(&db);
}

/// The lease is released when the run ends for any reason: a failing node,
/// a timed-out node, and an interrupted holder all free `cpu`.
#[tokio::test]
async fn release_on_fail_timeout_interrupt() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();

    // Fail: the node exits 1 holding `cpu` — the lease must not leak.
    insert_cpu_check(&db, &spec_id, "failer", "exit 1", None);
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(
        latest_run_for_spec(&db, &spec_id).status,
        GraphRunStatus::Fail
    );
    assert_mutex_tables_empty(&db);

    // Timeout: a 30 s command with a 2 s timeout fails holding `cpu`.
    let (graph_t, spec_t) = insert_second_graph(&db, &workdir, "timeout").unwrap();
    insert_cpu_check(&db, &spec_t, "sleeper", "sleep 30", Some(2));
    engine
        .run_graph(graph_t.clone(), None, None, None, None, None)
        .await
        .unwrap();
    let timed_out = latest_run_for_spec(&db, &spec_t);
    assert_eq!(timed_out.status, GraphRunStatus::Fail);
    assert!(
        serde_json::to_string(&timed_out.output)
            .unwrap()
            .contains("timed out"),
        "expected a timeout verdict, got {:?}",
        timed_out.output
    );
    assert_mutex_tables_empty(&db);

    // Interrupt: a holder mid-sleep is interrupted; its waiter proceeds.
    let (graph_h, spec_h) = insert_second_graph(&db, &workdir, "holder").unwrap();
    let (graph_w, spec_w) = insert_second_graph(&db, &workdir, "waiter").unwrap();
    insert_cpu_check(&db, &spec_h, "holder", "sleep 3", None);
    insert_cpu_check(&db, &spec_w, "waiter", "true", None);
    let engine = std::sync::Arc::new(engine);
    let engine_h = Arc::clone(&engine);
    let graph_h_run = graph_h.clone();
    let holder = tokio::spawn(async move {
        engine_h
            .run_graph(graph_h_run, None, None, None, None, None)
            .await
    });
    assert!(
        poll_lease_for(&db, &graph_h, "cpu").await,
        "holder must grab cpu first"
    );
    let engine_w = Arc::clone(&engine);
    let graph_w_run = graph_w.clone();
    let waiter = tokio::spawn(async move {
        engine_w
            .run_graph(graph_w_run, None, None, None, None, None)
            .await
    });
    assert!(
        poll_waiter_for(&db, &graph_w).await,
        "waiter must queue behind the holder"
    );
    let holder_run = latest_run_for_spec(&db, &spec_h);
    assert_eq!(holder_run.status, GraphRunStatus::Running);
    assert!(db
        .interrupt_graph_run(&holder_run.id, "test interrupt")
        .unwrap());

    holder.await.unwrap().unwrap();
    waiter.await.unwrap().unwrap();

    assert_eq!(
        db.get_graph_run(&holder_run.id).unwrap().unwrap().status,
        GraphRunStatus::Interrupted
    );
    assert_eq!(
        latest_run_for_spec(&db, &spec_w).status,
        GraphRunStatus::Pass,
        "the waiter must proceed once the interrupt releases cpu"
    );
    assert_mutex_tables_empty(&db);
}
