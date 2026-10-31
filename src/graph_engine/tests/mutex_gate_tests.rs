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

/// Hold a write lock on the test database for `secs` from a second
/// connection: `BEGIN IMMEDIATE` (its own 30 s busy timeout absorbs any
/// engine write in flight), signal once held, `ROLLBACK` at the end. Every
/// engine write during the hold waits out the 5 s `DB_BUSY_TIMEOUT`, then
/// fails busy — the transient contention the gate retry exists for.
fn spawn_db_write_holder(
    db_path: std::path::PathBuf,
    secs: u64,
) -> (
    tokio::sync::oneshot::Receiver<()>,
    std::thread::JoinHandle<()>,
) {
    let (held_tx, held_rx) = tokio::sync::oneshot::channel();
    let handle = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.busy_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        let _ = held_tx.send(());
        std::thread::sleep(std::time::Duration::from_secs(secs));
        conn.execute_batch("ROLLBACK").unwrap();
    });
    (held_rx, handle)
}

/// One plain (mutex-free) check node on `spec_id`.
fn insert_plain_check(db: &Database, spec_id: &str, node_id: &str, command: &str, position: i64) {
    db.insert_graph_node(&GraphNode {
        id: node_id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: node_id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": command,
            "success_condition": "exit_code_0",
        }),
        position,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

/// Thread-local tracing capture for the busy-retry warn line.
/// `#[tokio::test]` runs a current-thread runtime, so the engine tasks
/// observe this subscriber (precedent: `mcp_error_logging_tests`).
#[derive(Default, Clone)]
struct CapturedWarns(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

struct WarnLayer(CapturedWarns);

impl<S> tracing_subscriber::layer::Layer<S> for WarnLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Visitor(String);
        impl tracing::field::Visit for Visitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                self.0.push_str(&format!("{}={:?} ", field.name(), value));
            }
        }
        let mut visitor = Visitor(String::new());
        event.record(&mut visitor);
        self.0 .0.lock().unwrap().push(visitor.0);
    }
}

/// A BUSY raised on a run-path write while a node waits in the gate is
/// retried: the graph survives and the node proceeds once the holder
/// releases. Without the retry the gate write fails past the 5 s busy
/// timeout and the graph dies "database is locked".
#[tokio::test]
async fn busy_gate_waits_then_proceeds() {
    use tracing_subscriber::layer::SubscriberExt;

    let (dir, db, engine, graph_a, spec_a) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let (graph_b, spec_b) = insert_second_graph(&db, &workdir, "b").unwrap();
    insert_cpu_check(&db, &spec_a, "holder", "sleep 10", None);
    insert_cpu_check(&db, &spec_b, "waiter", "true", None);
    let engine = std::sync::Arc::new(engine);

    let captured = CapturedWarns::default();
    let subscriber = tracing_subscriber::registry().with(WarnLayer(captured.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);

    // A wins cpu; B parks in the gate behind it.
    let engine_h = Arc::clone(&engine);
    let graph_h = graph_a.clone();
    let holder = tokio::spawn(async move {
        engine_h
            .run_graph(graph_h, None, None, None, None, None)
            .await
    });
    assert!(
        poll_lease_for(&db, &graph_a, "cpu").await,
        "holder must grab cpu first"
    );
    let engine_w = Arc::clone(&engine);
    let graph_w = graph_b.clone();
    let waiter = tokio::spawn(async move {
        engine_w
            .run_graph(graph_w, None, None, None, None, None)
            .await
    });
    assert!(
        poll_waiter_for(&db, &graph_b).await,
        "waiter must queue behind the holder"
    );

    // A second connection holds a write lock for 6 s — past the 5 s busy
    // timeout — while B's gate waits. The gate is woken in the last 300 ms
    // of the hold: `try_acquire_mutex_set` runs in an explicit transaction
    // (read-then-write), which fails busy INSTANTLY under a write holder
    // instead of waiting out the busy timeout, so the 3-attempt budget
    // spans ~300 ms — the first attempt(s) warn and the last lands after
    // the release. (Waking at the start of the hold would exhaust the
    // budget instead; that is the persistent-busy test below.)
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let (held_rx, holder_thread) = spawn_db_write_holder(db_path, 6);
    held_rx.await.expect("holder took the write lock");
    tokio::time::sleep(Duration::from_millis(5800)).await;
    db.notify_mutex_waiters();

    waiter.await.unwrap().unwrap();
    holder.await.unwrap().unwrap();
    holder_thread.join().unwrap();

    assert_eq!(
        latest_run_for_spec(&db, &spec_b).status,
        GraphRunStatus::Pass,
        "the waiter must proceed once the lock holder releases"
    );
    assert_eq!(
        db.get_graph(&graph_b).unwrap().unwrap().status,
        GraphStatus::Completed
    );
    let warns = captured.0.lock().unwrap().join("\n");
    assert!(
        warns.contains("db busy during try_acquire_mutex_set attempt 1/3"),
        "expected a retry-then-success warn, got: {warns}"
    );
    assert_mutex_tables_empty(&db);
}

/// A BUSY that persists past the retry budget becomes a node Fail carrying
/// the busy message — routable through the normal fail edge — never an
/// engine-fatal "database is locked" graph verdict.
#[tokio::test]
async fn persistent_busy_is_node_fail() {
    let (dir, db, engine, graph_a, spec_a) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let (graph_b, spec_b) = insert_second_graph(&db, &workdir, "b").unwrap();
    insert_cpu_check(&db, &spec_a, "holder", "sleep 25", None);
    insert_cpu_check(&db, &spec_b, "waiter", "true", None);
    let marker = dir.path().join("triage.marker");
    insert_plain_check(
        &db,
        &spec_b,
        "triage",
        &format!("touch '{}'", marker.display()),
        2,
    );
    db.insert_graph_edge(&GraphEdge {
        id: "edge-waiter-triage".to_string(),
        spec_id: Some(spec_b.clone()),
        graph_id: None,
        from_node: "waiter".to_string(),
        to_node: "triage".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();
    let engine = std::sync::Arc::new(engine);

    // A wins cpu; B parks in the gate behind it.
    let engine_h = Arc::clone(&engine);
    let graph_h = graph_a.clone();
    let holder = tokio::spawn(async move {
        engine_h
            .run_graph(graph_h, None, None, None, None, None)
            .await
    });
    assert!(
        poll_lease_for(&db, &graph_a, "cpu").await,
        "holder must grab cpu first"
    );
    let engine_w = Arc::clone(&engine);
    let graph_w = graph_b.clone();
    let waiter = tokio::spawn(async move {
        engine_w
            .run_graph(graph_w, None, None, None, None, None)
            .await
    });
    assert!(
        poll_waiter_for(&db, &graph_b).await,
        "waiter must queue behind the holder"
    );

    // The write lock outlives the retry budget: the gate is woken 15 s
    // into the 20 s hold, so its attempts (which fail busy instantly — see
    // above) exhaust and the best-effort Fail settle's 5 s window covers
    // the release and lands.
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let (held_rx, holder_thread) = spawn_db_write_holder(db_path, 20);
    held_rx.await.expect("holder took the write lock");
    tokio::time::sleep(Duration::from_secs(15)).await;
    db.notify_mutex_waiters();

    waiter.await.unwrap().unwrap();
    holder_thread.join().unwrap();
    // End the cpu holder promptly now that B's verdict is in.
    let holder_run = latest_run_for_spec(&db, &spec_a);
    assert!(db.interrupt_graph_run(&holder_run.id, "test done").unwrap());
    holder.await.unwrap().unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_b).unwrap();
    let waiter_run = runs
        .iter()
        .find(|run| run.node_id == "waiter")
        .expect("a run row for the waiter node");
    assert_eq!(waiter_run.status, GraphRunStatus::Fail);
    let message = serde_json::to_string(&waiter_run.output).unwrap();
    assert!(
        message.contains("try_acquire_mutex_set"),
        "busy Fail must name the operation: {message}"
    );
    assert!(
        message.contains("transient"),
        "busy Fail must say transient: {message}"
    );
    assert_ne!(
        db.get_graph(&graph_b).unwrap().unwrap().status,
        GraphStatus::Failed,
        "a persistent busy must never fail the graph"
    );
    let triage_run = runs
        .iter()
        .find(|run| run.node_id == "triage")
        .expect("the fail edge must have routed to triage");
    assert_eq!(triage_run.status, GraphRunStatus::Pass);
    assert!(marker.exists(), "triage must have executed");
    assert_mutex_tables_empty(&db);
}

/// A BUSY that persists past the retry budget on a settle-path write
/// becomes a node Fail carrying the busy message — routable through the
/// normal fail edge — never an engine-fatal "database is locked" verdict.
#[tokio::test]
async fn settle_path_persistent_busy_routes_fail_edge() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    insert_plain_check(&db, &spec_id, "worker", "sleep 6", 1);
    let marker = dir.path().join("settle-triage.marker");
    insert_plain_check(
        &db,
        &spec_id,
        "triage",
        &format!("touch '{}'", marker.display()),
        2,
    );
    db.insert_graph_edge(&GraphEdge {
        id: "edge-worker-triage".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "triage".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();
    let engine = std::sync::Arc::new(engine);

    let engine_run = Arc::clone(&engine);
    let graph_run = graph_id.clone();
    let run = tokio::spawn(async move {
        engine_run
            .run_graph(graph_run, None, None, None, None, None)
            .await
    });
    // Start the hold only once the worker row exists: everything before the
    // settle (baseline, spec status, dispatch insert) lands uncontended, so
    // the exhaustion below is deterministically the settle write's. The
    // worker settles ~11 s in (6 s sleep plus the best-effort pid/tail
    // writes each waiting out one 5 s busy timeout); each of the 3 settle
    // attempts then waits out its own 5 s timeout and fails inside the
    // 35 s hold, the verdict becomes a busy Fail, and the retried
    // best-effort settle lands after the release.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let dispatched = db
            .list_graph_runs_for_spec(&spec_id)
            .is_ok_and(|runs| runs.iter().any(|run| run.node_id == "worker"));
        if dispatched || tokio::time::Instant::now() >= deadline {
            assert!(dispatched, "worker must dispatch before the hold starts");
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let (held_rx, holder_thread) = spawn_db_write_holder(db_path, 35);
    held_rx.await.expect("holder took the write lock");

    run.await.unwrap().unwrap();
    holder_thread.join().unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let worker_run = runs
        .iter()
        .find(|run| run.node_id == "worker")
        .expect("a run row for the worker node");
    assert_eq!(worker_run.status, GraphRunStatus::Fail);
    let message = serde_json::to_string(&worker_run.output).unwrap();
    assert!(
        message.contains("update_graph_run_result"),
        "busy Fail must name the operation: {message}"
    );
    assert!(
        message.contains("transient"),
        "busy Fail must say transient: {message}"
    );
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed,
        "a persistent busy must never fail the graph"
    );
    let triage_run = runs
        .iter()
        .find(|run| run.node_id == "triage")
        .expect("the fail edge must have routed to triage");
    assert_eq!(triage_run.status, GraphRunStatus::Pass);
    assert!(marker.exists(), "triage must have executed");
}
