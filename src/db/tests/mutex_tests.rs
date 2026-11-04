//! Named-mutex store tests: atomic multi-resource grants, FIFO waiter
//! order, release paths, and daemon-restart reconciliation.
use super::*;

/// A graph + spec + node + run row ready to wait for mutexes. Returns the
/// run id; the run starts `waiting` like the engine inserts it.
fn insert_waiting_run(db: &Database, tag: &str) -> String {
    let graph_id = format!("graph-{tag}");
    let spec_id = format!("spec-{tag}");
    let node_id = format!("node-{tag}");
    let run_id = format!("run-{tag}");
    let mut graph = sample_graph(&graph_id);
    graph.name = format!("Graph {tag}");
    db.insert_graph(&graph).unwrap();
    db.insert_graph_spec(&sample_graph_spec(&graph_id, &spec_id, 1))
        .unwrap();
    db.insert_graph_node(&sample_graph_node(&spec_id, &node_id, 1))
        .unwrap();
    let now = Utc::now();
    db.insert_graph_run(&GraphNodeRun {
        id: run_id.clone(),
        graph_id,
        spec_id,
        node_id,
        status: GraphRunStatus::Waiting,
        input: None,
        output: None,
        started_at: now,
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: Some("boot-test".to_string()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
    run_id
}

fn cpu() -> Vec<String> {
    vec!["cpu".to_string()]
}

fn cpu_gpu() -> Vec<String> {
    vec!["cpu".to_string(), "gpu".to_string()]
}

/// Two graphs whose nodes both need `cpu` never hold it at once: the second
/// grant waits until the first releases.
#[test]
fn cpu_nodes_never_overlap() {
    let db = test_db();
    let first = insert_waiting_run(&db, "first");
    let second = insert_waiting_run(&db, "second");

    assert!(db
        .try_acquire_mutex_set(&first, "graph-first", "gates", &cpu())
        .unwrap());
    assert!(
        !db.try_acquire_mutex_set(&second, "graph-second", "gates", &cpu())
            .unwrap(),
        "cpu is held — the second grant must wait"
    );

    let state = db.list_mutex_state().unwrap();
    assert_eq!(state.leases.len(), 1);
    assert_eq!(state.leases[0].resource, "cpu");
    assert_eq!(state.leases[0].holder_run_id, first);
    assert_eq!(state.waiters.len(), 1);
    assert_eq!(state.waiters[0].run_id, second);

    db.release_mutexes_for_run(&first).unwrap();
    assert!(db
        .try_acquire_mutex_set(&second, "graph-second", "gates", &cpu())
        .unwrap());
    let state = db.list_mutex_state().unwrap();
    assert_eq!(state.leases.len(), 1);
    assert_eq!(state.leases[0].holder_run_id, second);
    assert!(state.waiters.is_empty());
}

/// Grants follow waiter FIFO order: the earliest waiter whose full set is
/// free wins, and a later waiter for a different resource is never stuck
/// behind an earlier waiter blocked on a busy one.
#[test]
fn fifo_order() {
    let db = test_db();
    let holder = insert_waiting_run(&db, "holder");
    let waiter_cpu = insert_waiting_run(&db, "waiter-cpu");
    let waiter_gpu = insert_waiting_run(&db, "waiter-gpu");

    assert!(db
        .try_acquire_mutex_set(&holder, "graph-holder", "gates", &cpu())
        .unwrap());
    // Enqueue in order: cpu-waiter first, then gpu-waiter.
    assert!(!db
        .try_acquire_mutex_set(&waiter_cpu, "graph-waiter-cpu", "gates", &cpu())
        .unwrap());
    // `gpu` is free and the earlier waiter needs only `cpu`, so the later
    // gpu waiter grants immediately — no head-of-line blocking.
    assert!(db
        .try_acquire_mutex_set(
            &waiter_gpu,
            "graph-waiter-gpu",
            "render",
            &["gpu".to_string()],
        )
        .unwrap());

    db.release_mutexes_for_run(&holder).unwrap();
    assert!(db
        .try_acquire_mutex_set(&waiter_cpu, "graph-waiter-cpu", "gates", &cpu())
        .unwrap());
    let state = db.list_mutex_state().unwrap();
    assert!(state.waiters.is_empty());
}

/// A node needing `cpu`+`gpu` never holds one while waiting for the other:
/// the grant is all or none.
#[test]
fn no_partial_multi_hold() {
    let db = test_db();
    let gpu_holder = insert_waiting_run(&db, "gpu-holder");
    let both = insert_waiting_run(&db, "both");

    assert!(db
        .try_acquire_mutex_set(
            &gpu_holder,
            "graph-gpu-holder",
            "render",
            &["gpu".to_string()],
        )
        .unwrap());
    assert!(
        !db.try_acquire_mutex_set(&both, "graph-both", "build", &cpu_gpu())
            .unwrap(),
        "gpu is busy — the cpu+gpu grant must fail atomically"
    );

    // In particular nothing of the failed set leaked into the lease table.
    let state = db.list_mutex_state().unwrap();
    assert_eq!(state.leases.len(), 1);
    assert_eq!(state.leases[0].resource, "gpu");
    assert_eq!(state.leases[0].holder_run_id, gpu_holder);

    db.release_mutexes_for_run(&gpu_holder).unwrap();
    assert!(db
        .try_acquire_mutex_set(&both, "graph-both", "build", &cpu_gpu())
        .unwrap());
    let state = db.list_mutex_state().unwrap();
    assert_eq!(state.leases.len(), 2);
    assert!(state.leases.iter().all(|lease| lease.holder_run_id == both));
}

/// A terminal run transition releases its leases: fail, interrupt, and the
/// explicit release all free the resource for the next waiter.
#[test]
fn release_frees_lease() {
    let db = test_db();
    let failing = insert_waiting_run(&db, "failing");
    let next = insert_waiting_run(&db, "next");

    assert!(db
        .try_acquire_mutex_set(&failing, "graph-failing", "gates", &cpu())
        .unwrap());
    db.mark_graph_run_started_from_waiting(&failing).unwrap();
    db.update_graph_run_result(
        &failing,
        GraphRunStatus::Fail,
        Some(&serde_json::json!({"summary": "boom"})),
        Some(Utc::now()),
    )
    .unwrap();
    assert!(db
        .try_acquire_mutex_set(&next, "graph-next", "gates", &cpu())
        .unwrap());

    // Interrupting a running holder releases too.
    db.mark_graph_run_started_from_waiting(&next).unwrap();
    assert!(db.interrupt_graph_run(&next, "operator stop").unwrap());
    assert!(db.list_mutex_state().unwrap().leases.is_empty());

    // Interrupting a *waiting* run drops its queue entry.
    let waiter = insert_waiting_run(&db, "waiter");
    db.set_graph_run_waiting_for(&waiter, "graph-waiter", "gates", &cpu())
        .unwrap();
    assert!(db.interrupt_graph_run(&waiter, "operator stop").unwrap());
    let state = db.list_mutex_state().unwrap();
    assert!(state.leases.is_empty());
    assert!(state.waiters.is_empty());
}

/// Daemon-restart reconciliation releases leases whose holder run is not
/// `running` and drops waiters whose run row is gone — while a live holder
/// and a live waiter survive untouched.
#[test]
fn reconcile_drops_dead() {
    let db = test_db();
    let live = insert_waiting_run(&db, "live");
    let waiter = insert_waiting_run(&db, "waiter");
    assert!(db
        .try_acquire_mutex_set(&live, "graph-live", "gates", &cpu())
        .unwrap());
    db.mark_graph_run_started_from_waiting(&live).unwrap();
    db.set_graph_run_waiting_for(&waiter, "graph-waiter", "gates", &cpu())
        .unwrap();

    // Simulate the previous daemon dying mid-hold: a lease for a run that
    // no longer exists, one for a run that is only `waiting`, and a waiter
    // for a deleted run — plus a waiter row left on a running run, which a
    // crash between grant and dequeue could leave behind.
    {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mutex_leases (resource, holder_run_id, graph_id, node_name, acquired_at)
             VALUES ('gpu', 'run-gone', 'graph-gone', 'render', '2026-10-27T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mutex_leases (resource, holder_run_id, graph_id, node_name, acquired_at)
             VALUES ('tpu', ?1, 'graph-waiter', 'gates', '2026-10-27T00:00:00Z')",
            rusqlite::params![waiter],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO mutex_waiters (resources, run_id, graph_id, node_name, requested_at)
             VALUES ('[\"cpu\"]', 'run-gone', 'graph-gone', 'gates', '2026-10-27T00:00:00Z')",
            [],
        )
        .unwrap();
    }

    let (leases, waiters) = db.reconcile_mutex_leases().unwrap();
    assert_eq!((leases, waiters), (2, 1));

    let state = db.list_mutex_state().unwrap();
    assert_eq!(state.leases.len(), 1);
    assert_eq!(state.leases[0].holder_run_id, live);
    assert_eq!(state.waiters.len(), 1);
    assert_eq!(state.waiters[0].run_id, waiter);

    // Reconciliation is idempotent: a second pass drops nothing.
    assert_eq!(db.reconcile_mutex_leases().unwrap(), (0, 0));
}
