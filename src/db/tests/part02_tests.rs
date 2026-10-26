//! Part 02 of `db::tests` top-level tests (split from `src/db/tests.rs`, pure move).
use super::*;

#[test]
fn graph_run_roundtrip_preserves_json_payloads() {
    let db = test_db();
    let lp = sample_graph("wf-2");
    let spec = sample_graph_spec(&lp.id, "spec-run", 1);
    let node = sample_graph_node(&spec.id, "node-run", 1);
    let run = GraphNodeRun {
        id: "run-1".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Pass,
        input: Some(serde_json::json!({"feedback": "previous"})),
        output: Some(serde_json::json!({"summary": "ok"})),
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        iteration: 2,
        pid: Some(4242),
        boot_id: Some("boot-abc".to_string()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();

    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].iteration, 2);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert_eq!(runs[0].pid, Some(4242));
    assert_eq!(runs[0].boot_id.as_deref(), Some("boot-abc"));
    assert_eq!(
        runs[0]
            .input
            .as_ref()
            .and_then(|value| value.get("feedback")),
        Some(&serde_json::json!("previous"))
    );
    assert_eq!(
        runs[0]
            .output
            .as_ref()
            .and_then(|value| value.get("summary")),
        Some(&serde_json::json!("ok"))
    );
}

#[test]
fn graph_updates_persist_metadata_and_positions() {
    let db = test_db();
    let lp = sample_graph("wf-update");
    let spec = sample_graph_spec(&lp.id, "spec-update", 1);
    let node = sample_graph_node(&spec.id, "node-update", 1);
    let edge = GraphEdge {
        id: "edge-update".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        from_node: node.id.clone(),
        to_node: node.id.clone(),
        condition: GraphEdgeCondition::Always,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_edge(&edge).unwrap();

    db.update_graph_details(
        &lp.id,
        Some("Auth refresh graph"),
        Some(Some("Updated description")),
        Some("/tmp/other-project"),
    )
    .unwrap();
    db.update_graph_spec_details(
        &spec.id,
        Some("Spec updated"),
        Some("Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G"),
        Some(3),
        Some(true),
    )
    .unwrap();
    db.update_graph_node_details(
        &node.id,
        Some("Verification node"),
        Some(GraphNodeKind::Gate),
        Some(&serde_json::json!({"evaluate": "output_contains", "value": "APPROVED"})),
        Some(4),
    )
    .unwrap();
    db.update_graph_edge_condition(&edge.id, &GraphEdgeCondition::Fail)
        .unwrap();

    let lp = db.get_graph(&lp.id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec.id).unwrap().unwrap();
    let node = db.get_graph_node(&node.id).unwrap().unwrap();
    let edge = db.get_graph_edge(&edge.id).unwrap().unwrap();

    assert_eq!(lp.name, "Auth refresh graph");
    assert_eq!(lp.description.as_deref(), Some("Updated description"));
    assert_eq!(lp.workdir, "/tmp/other-project");
    assert_eq!(spec.name, "Spec updated");
    assert_eq!(spec.position, 3);
    assert!(spec.parallelizable);
    assert_eq!(node.name, "Verification node");
    assert_eq!(node.kind, GraphNodeKind::Gate);
    assert_eq!(node.position, 4);
    assert_eq!(
        node.config.get("evaluate"),
        Some(&serde_json::json!("output_contains"))
    );
    assert_eq!(edge.condition, GraphEdgeCondition::Fail);
}

#[test]
fn graph_spec_start_head_persists_through_reread() {
    // G4: spec_start_head must survive a fresh read from the DB (e.g. after a
    // daemon restart), not just live on the in-memory GraphSpec that set it.
    let db = test_db();
    let lp = sample_graph("wf-start-head");
    let spec = sample_graph_spec(&lp.id, "spec-start-head", 1);

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();

    let fresh = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(fresh.spec_start_head, None);

    assert!(db
        .set_graph_spec_start_head(&spec.id, Some("e1c134b"))
        .unwrap());

    let reread = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(reread.spec_start_head.as_deref(), Some("e1c134b"));

    let listed = db
        .list_graph_specs(&lp.id)
        .unwrap()
        .into_iter()
        .find(|item| item.id == spec.id)
        .unwrap();
    assert_eq!(listed.spec_start_head.as_deref(), Some("e1c134b"));

    assert!(db.set_graph_spec_start_head(&spec.id, None).unwrap());
    let cleared = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(cleared.spec_start_head, None);
}

#[test]
fn reconcile_orphaned_graphs_pauses_running_graph_and_interrupts_its_run() {
    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan");
    lp.status = GraphStatus::Running;
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan", 1);
    spec.status = GraphSpecStatus::Running;
    let node = sample_graph_node(&spec.id, "node-orphan", 1);
    let run = GraphNodeRun {
        id: "run-orphan".to_string(),
        graph_id: lp.id.clone(),
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
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();
    // Prove the reconcile pass actually clears these, not that they were
    // never set.
    db.update_graph_spec_status(&spec.id, GraphSpecStatus::Running, Some(Utc::now()), None)
        .unwrap();
    db.set_graph_spec_start_head(&spec.id, Some("deadbeef"))
        .unwrap();

    let reconciled = db.reconcile_orphaned_graphs(data_dir.path()).unwrap();
    assert_eq!(reconciled.len(), 1);

    // Test 1: the graph is paused, and its dangling run is no longer `running`.
    let lp_after = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(lp_after.status, GraphStatus::Paused);
    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_ne!(run_after.status, GraphRunStatus::Running);
    assert_eq!(
        run_after
            .output
            .as_ref()
            .and_then(|value| value.get("interrupted")),
        Some(&serde_json::json!(true))
    );

    // Test 2 (B18): the spec is marked `interrupted` in the same pass — not
    // reset to `pending`, since the run was cut short by something external,
    // not a failure of the work. Its completed work is preserved by the
    // worktree/commits, not by its status, and leaving it `running` would
    // make it invisible to queue selection (`queue_next_pending_spec_id`
    // picks `pending` and `interrupted` alike).
    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Interrupted);
    assert_eq!(spec_after.started_at, None);
    assert_eq!(spec_after.spec_start_head, None);
}

/// B18: the real incident — a queue-driven run's in-flight member (a
/// standalone spec, `graph_id: None`, never bound to the graph that's
/// currently running it) must be marked `interrupted` exactly like a
/// graph-bound spec is. Left `running`, it would be invisible to
/// `queue_next_pending_spec_id` forever — the orphan this whole fix exists
/// to prevent — and `queue_next_pending_spec_id` must pick an `interrupted`
/// member right back up, in the same position, exactly as it would a
/// `pending` one.
#[test]
fn reconcile_orphaned_graphs_marks_queue_member_spec_interrupted() {
    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-queue");
    lp.status = GraphStatus::Running;
    lp.active_run_queue_id = Some("queue-1".to_string());
    db.insert_graph(&lp).unwrap();

    let mut spec = sample_graph_spec("unused-graph-id", "spec-orphan-queue", 1);
    spec.graph_id = None; // queue membership never binds the spec to a graph
    spec.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&spec).unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &spec.id, None).unwrap();

    let node = sample_graph_node(&spec.id, "node-orphan-queue", 1);
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-orphan-queue".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id,
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

    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );

    let lp_after = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(lp_after.status, GraphStatus::Paused);
    let spec_after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert_eq!(spec_after.status, GraphSpecStatus::Interrupted);
    // The queue's live pick can now find it again, exactly as it would a
    // `pending` member.
    assert_eq!(
        db.queue_next_pending_spec_id("queue-1").unwrap().as_deref(),
        Some(spec.id.as_str())
    );
}

/// R3 (B18): the defensive selection safety net. A queue member left
/// `running` with no `graph_runs` row proving it's still live in *this*
/// daemon's lifetime must be flagged as stale — but a member whose `running`
/// node run really does carry the current boot id (i.e. genuinely still in
/// flight right now) must not be.
#[test]
fn queue_stale_running_members_flags_only_the_member_with_no_live_run() {
    let db = test_db();
    let lp = sample_graph("wf-queue-stale");
    db.insert_graph(&lp).unwrap();

    let mut stale = sample_graph_spec("unused-graph-id", "spec-stale", 1);
    stale.graph_id = None;
    stale.status = GraphSpecStatus::Running;
    let mut live = sample_graph_spec("unused-graph-id", "spec-live", 2);
    live.graph_id = None;
    live.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&stale).unwrap();
    db.insert_graph_spec(&live).unwrap();

    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", &stale.id, None).unwrap();
    db.append_queue_member("queue-1", &live.id, None).unwrap();

    // `live`'s node run genuinely belongs to the current daemon's boot.
    let node = sample_graph_node(&live.id, "node-live", 1);
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-live".to_string(),
        graph_id: lp.id,
        spec_id: live.id,
        node_id: node.id,
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: Some("boot-current".to_string()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    let stale_members = db
        .queue_stale_running_members("queue-1", Some("boot-current"))
        .unwrap();
    assert_eq!(stale_members, vec![stale.id]);
}

#[test]
fn reconcile_orphaned_graphs_is_idempotent() {
    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-idempotent");
    lp.status = GraphStatus::Running;
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-idempotent", 1);
    spec.status = GraphSpecStatus::Running;
    let node = sample_graph_node(&spec.id, "node-orphan-idempotent", 1);
    let run = GraphNodeRun {
        id: "run-orphan-idempotent".to_string(),
        graph_id: lp.id.clone(),
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
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    let first_pass = db.reconcile_orphaned_graphs(data_dir.path()).unwrap();
    assert_eq!(first_pass.len(), 1);
    let lp_after_first = db.get_graph(&lp.id).unwrap().unwrap();
    let run_after_first = db.get_graph_run(&run.id).unwrap().unwrap();

    // Test 3: a second reconcile pass finds nothing left to reconcile, and
    // leaves the already-paused graph/run untouched.
    let second_pass = db.reconcile_orphaned_graphs(data_dir.path()).unwrap();
    assert_eq!(second_pass.len(), 0);
    let lp_after_second = db.get_graph(&lp.id).unwrap().unwrap();
    let run_after_second = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(lp_after_second.status, lp_after_first.status);
    assert_eq!(run_after_second.status, run_after_first.status);
    assert_eq!(run_after_second.completed_at, run_after_first.completed_at);
}

/// B12: reconciliation at daemon boot can't have held a `Child` for a run
/// that predates it, but if the dangling run's `pid`/`boot_id` were
/// persisted by the process that spawned it, and the machine hasn't
/// rebooted since (same `boot_id`), reconciliation should still attempt a
/// best-effort kill of the survivor instead of just abandoning it.
#[tokio::test]
async fn reconcile_orphaned_graphs_kills_survivor_pid_from_same_boot() {
    let Some(current_boot_id) = crate::system::boot_id() else {
        // Non-Linux host (or /proc unavailable): boot_id is never known, so
        // the same-boot check can never match — nothing to test here.
        return;
    };

    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-survivor");
    lp.status = GraphStatus::Running;
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-survivor", 1);
    spec.status = GraphSpecStatus::Running;
    let node = sample_graph_node(&spec.id, "node-orphan-survivor", 1);

    // A real, still-running process group leader to stand in for a `mimo
    // run` that outlived the daemon that spawned it. Must be its own
    // process-group leader (as every real spawn site is, via
    // `.process_group(0)`) for `killpg` to reach it rather than the test
    // process's own group.
    let mut command = std::process::Command::new("sleep");
    command.arg("30");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().expect("spawn survivor process");
    let pid = child.id() as i64;

    let run = GraphNodeRun {
        id: "run-orphan-survivor".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: Some(pid),
        boot_id: Some(current_boot_id),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );

    // The kill is fired via a detached task (see
    // `terminate_process_group_async`); poll briefly for the SIGTERM to
    // land instead of asserting immediately.
    let killed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);

    assert!(killed, "survivor process from the same boot must be killed");
}

/// The mirror case: a dangling run whose `boot_id` does NOT match the
/// current machine boot must be left alone — the pid may have been recycled
/// by an unrelated process since the reboot, so killing it would be
/// dangerous, not just useless.
#[test]
fn reconcile_orphaned_graphs_skips_kill_for_mismatched_boot_id() {
    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-orphan-stale-boot");
    lp.status = GraphStatus::Running;
    let mut spec = sample_graph_spec(&lp.id, "spec-orphan-stale-boot", 1);
    spec.status = GraphSpecStatus::Running;
    let node = sample_graph_node(&spec.id, "node-orphan-stale-boot", 1);
    let run = GraphNodeRun {
        id: "run-orphan-stale-boot".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        // A pid from a previous boot — never a real live process on this
        // machine right now, but also never allowed to be signaled.
        pid: Some(1),
        boot_id: Some("some-other-boot-that-is-not-current".to_string()),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    // Must not panic or error even though pid 1 is a real (unkillable by
    // us) process — the boot_id mismatch must short-circuit before any
    // signal is ever attempted.
    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );
    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_ne!(run_after.status, GraphRunStatus::Running);
}

#[test]
fn reconcile_orphaned_graphs_leaves_completed_graph_untouched() {
    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-completed");
    lp.status = GraphStatus::Completed;
    db.insert_graph(&lp).unwrap();

    // Test 4: a `Completed` graph is not reconciled.
    let reconciled = db.reconcile_orphaned_graphs(data_dir.path()).unwrap();
    assert_eq!(reconciled.len(), 0);
    let lp_after = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(lp_after.status, GraphStatus::Completed);
}

/// The ownership gate (second line of defence, alongside never calling this
/// from `run_stdio_server` at all — see the doc comment on
/// `reconcile_orphaned_graphs`): when the on-disk `daemon.pid` names a *live*
/// process that isn't this one, some other process owns the daemon
/// lifecycle right now, and this call must not touch graph state at all —
/// no signal, no pause, no interrupted-run marking.
#[tokio::test]
async fn reconcile_orphaned_graphs_skips_everything_when_foreign_daemon_pid_is_live() {
    let Some(current_boot_id) = crate::system::boot_id() else {
        return;
    };

    let data_dir = tempdir().unwrap();
    // Stand in for "a live daemon other than us": a real, still-running
    // process whose pid is guaranteed not to equal this test process's own.
    let mut foreign_daemon = std::process::Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn stand-in foreign daemon process");
    std::fs::write(
        data_dir.path().join("daemon.pid"),
        foreign_daemon.id().to_string(),
    )
    .unwrap();

    let db = test_db();
    let mut lp = sample_graph("wf-foreign-daemon-owned");
    lp.status = GraphStatus::Running;
    let mut spec = sample_graph_spec(&lp.id, "spec-foreign-daemon-owned", 1);
    spec.status = GraphSpecStatus::Running;
    let node = sample_graph_node(&spec.id, "node-foreign-daemon-owned", 1);
    // A pid/boot_id that WOULD be killed if the gate failed to short-circuit
    // (same boot, matching the same-boot kill precondition tested elsewhere).
    let run = GraphNodeRun {
        id: "run-foreign-daemon-owned".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: Some(foreign_daemon.id() as i64),
        boot_id: Some(current_boot_id),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    let reconciled = db.reconcile_orphaned_graphs(data_dir.path()).unwrap();
    assert_eq!(
        reconciled.len(),
        0,
        "must not report any graph reconciled while a foreign live daemon owns the pid file"
    );

    let lp_after = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(
        lp_after.status,
        GraphStatus::Running,
        "the graph must be left exactly as found, not paused"
    );
    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(
        run_after.status,
        GraphRunStatus::Running,
        "the run must not be reported as interrupted by a restart that never happened"
    );

    // The pid named in the "foreign daemon" run must never have been
    // signaled — prove it's still alive rather than merely unreaped.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        foreign_daemon.try_wait().unwrap(),
        None,
        "the gate must never signal the pid it protects"
    );
    let _ = foreign_daemon.kill();
    let _ = foreign_daemon.wait();
}

#[cfg(target_os = "linux")]
#[test]
fn reconcile_orphaned_graphs_skips_kill_for_own_ancestor_pid() {
    let Some(current_boot_id) = crate::system::boot_id() else {
        return;
    };

    // Read our own PPid — the parent of this test process. This process's
    // own pid can't be used (it would be killed), but the parent is an
    // ancestor and must be skipped by the ancestor guard.
    let ppid: u32 = std::fs::read_to_string("/proc/self/status")
        .expect("read /proc/self/status")
        .lines()
        .find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|rest| rest.trim().parse::<u32>().ok())
        })
        .expect("PPid line must be present");
    assert!(ppid != 0 && ppid != 1);
    // Sanity: parent must be alive at test start.
    assert!(
        crate::daemon::process::is_process_running(ppid),
        "parent pid {ppid} must be alive"
    );

    let db = test_db();
    let data_dir = tempdir().unwrap();
    let mut lp = sample_graph("wf-ancestor-skip");
    lp.status = GraphStatus::Running;
    let mut spec = sample_graph_spec(&lp.id, "spec-ancestor-skip", 1);
    spec.status = GraphSpecStatus::Running;
    let node = sample_graph_node(&spec.id, "node-ancestor-skip", 1);
    let run = GraphNodeRun {
        id: "run-ancestor-skip".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: Some(ppid as i64),
        boot_id: Some(current_boot_id),
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };

    db.insert_graph(&lp).unwrap();
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&run).unwrap();

    let reconciled = db.reconcile_orphaned_graphs(data_dir.path()).unwrap();
    assert_eq!(reconciled.len(), 1);

    // Run must be marked interrupted in DB, but parent process must still be alive.
    let run_after = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_ne!(run_after.status, GraphRunStatus::Running);
    assert_eq!(
        run_after
            .output
            .as_ref()
            .and_then(|value| value.get("interrupted")),
        Some(&serde_json::json!(true))
    );
    assert!(
        crate::daemon::process::is_process_running(ppid),
        "ancestor pid {ppid} must still be alive — the guard must have skipped the kill"
    );
}

#[test]
fn new_safe_skips_migration_when_foreign_daemon_is_live() {
    let data_dir = tempdir().unwrap();
    // Simulate a live daemon by writing our own pid (guaranteed live).
    std::fs::write(
        data_dir.path().join("daemon.pid"),
        std::process::id().to_string(),
    )
    .unwrap();

    let db_path = data_dir.path().join("fresh.db");
    // Empty DB, but foreign daemon is live — new_safe must skip migrations.
    let _db = Database::new_safe(&db_path, data_dir.path())
        .expect("new_safe should open without migrating");
    drop(_db);
    // Verify no tables were created: `graphs` table must not exist.
    let graphs_exists: i32 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'graphs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        graphs_exists, 0,
        "new_safe with a live daemon must not have run migrations (graphs table must not exist)"
    );

    // Without the foreign daemon, the same path must migrate normally.
    std::fs::remove_file(data_dir.path().join("daemon.pid")).unwrap();
    let _db2 = Database::new_safe(&db_path, data_dir.path())
        .expect("new_safe without daemon should migrate");
    drop(_db2);
    let graphs_exists_after: i32 = rusqlite::Connection::open(&db_path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'graphs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        graphs_exists_after, 1,
        "new_safe without a live daemon must run migrations"
    );
}

#[test]
fn graph_trigger_round_trips_through_insert_and_get() {
    let db = test_db();
    let cron = graph_with_trigger(
        "wf-cron",
        Some(Trigger::Cron {
            schedule_expr: "30 8 * * *".to_string(),
        }),
    );
    db.insert_graph(&cron).unwrap();

    let fetched = db.get_graph("wf-cron").unwrap().unwrap();
    assert_eq!(fetched.schedule_expr(), Some("30 8 * * *"));
    assert!(fetched.is_cron());
}

#[test]
fn queue_and_members_round_trip_through_insert_and_get_details() {
    let db = test_db();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();

    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();
    db.append_queue_member("queue-1", "spec-c", None).unwrap();
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["spec-a", "spec-b", "spec-c"]
    );
    assert!(db.queue_has_member("queue-1", "spec-b").unwrap());

    let details = db.get_queue_details("queue-1").unwrap().unwrap();
    assert_eq!(details.queue.name, "queue-1 name");
    assert_eq!(
        details
            .members
            .iter()
            .map(|spec| spec.id.clone())
            .collect::<Vec<_>>(),
        vec!["spec-a", "spec-b", "spec-c"]
    );

    assert!(db.remove_queue_member("queue-1", "spec-b").unwrap());
    assert!(!db.queue_has_member("queue-1", "spec-b").unwrap());
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["spec-a", "spec-c"]
    );

    let names = db
        .list_queues()
        .unwrap()
        .into_iter()
        .map(|queue| queue.id)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["queue-1"]);
    assert!(db.get_queue("does-not-exist").unwrap().is_none());
}

#[test]
fn reorder_queue_members_replaces_positions_in_given_order() {
    let db = test_db();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.insert_graph_spec(&sample_standalone_spec(id, None))
            .unwrap();
    }
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.append_queue_member("queue-1", id, None).unwrap();
    }

    let order = vec![
        "spec-c".to_string(),
        "spec-a".to_string(),
        "spec-b".to_string(),
    ];
    db.reorder_queue_members("queue-1", &order).unwrap();

    assert_eq!(db.list_queue_member_spec_ids("queue-1").unwrap(), order);
}

#[test]
fn append_queue_member_rejects_nonexistent_spec() {
    let db = test_db();
    db.insert_queue(&sample_queue("queue-1")).unwrap();

    let error = db
        .append_queue_member("queue-1", "ghost-spec", None)
        .unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("foreign key"),
        "{error}"
    );
}

#[test]
fn deleting_a_spec_cascades_its_queue_membership() {
    let db = test_db();
    db.insert_graph_spec(&sample_standalone_spec("spec-a", None))
        .unwrap();
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "spec-a", None).unwrap();

    db.delete_graph_spec("spec-a").unwrap();

    assert!(db.list_queue_member_spec_ids("queue-1").unwrap().is_empty());
}

#[test]
fn group_name_persists_through_add_and_reorder() {
    let db = rs3_fixture();
    db.append_queue_member("queue-1", "spec-a", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-b", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-c", None).unwrap();

    // Groups are readable per member and in list order.
    assert_eq!(
        db.queue_member_group("queue-1", "spec-a")
            .unwrap()
            .as_deref(),
        Some("ctx")
    );
    assert_eq!(db.queue_member_group("queue-1", "spec-c").unwrap(), None);
    assert_eq!(
        db.list_queue_member_groups("queue-1").unwrap(),
        vec![
            ("spec-a".to_string(), Some("ctx".to_string())),
            ("spec-b".to_string(), Some("ctx".to_string())),
            ("spec-c".to_string(), None),
        ]
    );

    // Reorder must move rows AND preserve each row's group_name.
    db.reorder_queue_members(
        "queue-1",
        &[
            "spec-c".to_string(),
            "spec-a".to_string(),
            "spec-b".to_string(),
        ],
    )
    .unwrap();
    assert_eq!(
        db.list_queue_member_groups("queue-1").unwrap(),
        vec![
            ("spec-c".to_string(), None),
            ("spec-a".to_string(), Some("ctx".to_string())),
            ("spec-b".to_string(), Some("ctx".to_string())),
        ]
    );
}

#[test]
fn group_session_for_node_returns_completed_siblings_session() {
    let db = rs3_fixture();
    db.append_queue_member("queue-1", "spec-a", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-b", Some("ctx"))
        .unwrap();
    seed_group_sibling(
        &db,
        "wf-rs3",
        "spec-a",
        "node-impl",
        Some("ses-a"),
        GraphSpecStatus::Completed,
    );

    // spec-b's first visit to node-impl inherits spec-a's warm session.
    assert_eq!(
        db.group_session_for_node("queue-1", "ctx", "spec-b", "node-impl")
            .unwrap()
            .as_deref(),
        Some("ses-a")
    );
    // A node the sibling never ran → no session → cold start.
    assert_eq!(
        db.group_session_for_node("queue-1", "ctx", "spec-b", "node-review")
            .unwrap(),
        None
    );
}

#[test]
fn group_session_taint_on_failed_nearest_sibling() {
    let db = rs3_fixture();
    // Three grouped siblings: a completed, then a failed, then the current one.
    db.append_queue_member("queue-1", "spec-a", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-b", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-c", Some("ctx"))
        .unwrap();
    seed_group_sibling(
        &db,
        "wf-rs3",
        "spec-a",
        "node-impl",
        Some("ses-a"),
        GraphSpecStatus::Completed,
    );
    // The nearest sibling to spec-c FAILED (even if it captured a session).
    seed_group_sibling(
        &db,
        "wf-rs3",
        "spec-b",
        "node-impl",
        Some("ses-b"),
        GraphSpecStatus::Failed,
    );

    // Taint: the broken chain forces a cold start — the earlier completed
    // spec-a session is NOT resurrected across the failure.
    assert_eq!(
        db.group_session_for_node("queue-1", "ctx", "spec-c", "node-impl")
            .unwrap(),
        None
    );
}

#[test]
fn group_session_is_independent_per_node() {
    let db = rs3_fixture();
    db.append_queue_member("queue-1", "spec-a", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-b", Some("ctx"))
        .unwrap();
    // spec-a completed after capturing a DISTINCT session on each node.
    let run_impl = GraphNodeRun {
        id: "run-a-impl".to_string(),
        graph_id: "wf-rs3".to_string(),
        spec_id: "spec-a".to_string(),
        node_id: "node-impl".to_string(),
        status: GraphRunStatus::Pass,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: Some(Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: Some("ses-impl".to_string()),
        executed_platform: None,
        executed_model: None,
    };
    db.insert_graph_run(&run_impl).unwrap();
    let run_review = GraphNodeRun {
        id: "run-a-review".to_string(),
        node_id: "node-review".to_string(),
        session_id: Some("ses-review".to_string()),
        ..run_impl
    };
    db.insert_graph_run(&run_review).unwrap();
    db.update_graph_spec_status(
        "spec-a",
        GraphSpecStatus::Completed,
        Some(Utc::now()),
        Some(Utc::now()),
    )
    .unwrap();

    // Implementer and reviewer sessions stay independent.
    assert_eq!(
        db.group_session_for_node("queue-1", "ctx", "spec-b", "node-impl")
            .unwrap()
            .as_deref(),
        Some("ses-impl")
    );
    assert_eq!(
        db.group_session_for_node("queue-1", "ctx", "spec-b", "node-review")
            .unwrap()
            .as_deref(),
        Some("ses-review")
    );
}

#[test]
fn ungrouped_and_cross_group_members_never_cross_resume() {
    let db = rs3_fixture();
    // spec-a grouped "ctx", spec-b ungrouped, spec-c in a DIFFERENT group.
    db.append_queue_member("queue-1", "spec-a", Some("ctx"))
        .unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();
    db.append_queue_member("queue-1", "spec-c", Some("other"))
        .unwrap();
    seed_group_sibling(
        &db,
        "wf-rs3",
        "spec-a",
        "node-impl",
        Some("ses-a"),
        GraphSpecStatus::Completed,
    );

    // An ungrouped member never inherits (queried with its own — absent — group).
    assert_eq!(db.queue_member_group("queue-1", "spec-b").unwrap(), None);
    // A member in another group does not see "ctx"'s session.
    assert_eq!(
        db.group_session_for_node("queue-1", "other", "spec-c", "node-impl")
            .unwrap(),
        None
    );
}

#[test]
fn group_name_column_is_added_to_a_pre_rs3_queue_members_table() {
    // Simulate a pre-RS3 database whose `queue_members` predates `group_name`.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw db");
        conn.execute_batch(
            "CREATE TABLE graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER,
                spec_queue TEXT
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                workdir TEXT
             );
             CREATE TABLE queues (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             -- Pre-RS3 queue_members: no group_name column.
             CREATE TABLE queue_members (
                queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                PRIMARY KEY (queue_id, spec_id)
             );
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', NULL, 'Spec', 1, 'pending');
             INSERT INTO queues (id, name, created_at) VALUES ('queue-1', 'Queue', 0);
             INSERT INTO queue_members (queue_id, spec_id, position)
                 VALUES ('queue-1', 'legacy-spec', 1);",
        )
        .expect("seed pre-RS3 schema");
    }

    // Migration adds the nullable column; the pre-existing row reads back as
    // ungrouped (NULL), and re-running init stays safe (idempotent guard).
    let db = Database::new(&path).expect("open pre-RS3 db, running migration");
    assert_eq!(
        db.queue_member_group("queue-1", "legacy-spec").unwrap(),
        None
    );
    drop(db);
    let db = Database::new(&path).expect("re-open is idempotent");
    assert_eq!(
        db.queue_member_group("queue-1", "legacy-spec").unwrap(),
        None
    );
}

#[test]
fn queues_migration_is_idempotent_and_a_pre_r4_database_opens_cleanly() {
    // Simulate a pre-R4 database: `graphs` has the old `spec_queue` column
    // (7f2efdf) but no `queues`/`queue_members` tables at all.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER,
                spec_queue TEXT
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                workdir TEXT
             );
             INSERT INTO graphs (id, name, workdir, status, created_at, spec_queue)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'draft', 0, NULL);
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', 'legacy-graph', 'Spec', 1, 'pending');",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must succeed and add
    // the queues tables without disturbing existing rows.
    let db = Database::new(&path).expect("open pre-R4 db, running migration");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.name, "Legacy");
    db.insert_queue(&sample_queue("queue-1")).unwrap();
    db.append_queue_member("queue-1", "legacy-spec", None)
        .unwrap();
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["legacy-spec"]
    );
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    let db = Database::new(&path).expect("reopen db after migration already applied");
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["legacy-spec"]
    );

    // The retired `spec_queue` column is never written by current code: a
    // freshly inserted graph leaves it NULL.
    db.insert_graph(&sample_graph("fresh-graph")).unwrap();
    let raw: Option<String> = db
        .conn
        .lock()
        .unwrap()
        .query_row(
            "SELECT spec_queue FROM graphs WHERE id = 'fresh-graph'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, None);
}

#[test]
fn active_run_queue_id_migration_is_idempotent_and_a_pre_b8_database_opens_cleanly() {
    // Simulate a pre-B8 database: `graphs` has `autorun_at` and `queues`/
    // `queue_members` already exist, but `graphs` predates `active_run_queue_id`.
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);

    {
        let conn = rusqlite::Connection::open(&path).expect("open raw legacy db");
        conn.execute_batch(
            "CREATE TABLE graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER,
                spec_queue TEXT
             );
             CREATE TABLE graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                workdir TEXT
             );
             CREATE TABLE queues (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
             );
             CREATE TABLE queue_members (
                queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                PRIMARY KEY (queue_id, spec_id)
             );
             INSERT INTO graphs (id, name, workdir, status, created_at)
                 VALUES ('legacy-graph', 'Legacy', '/tmp', 'failed', 0);
             INSERT INTO graph_specs (id, graph_id, name, position, status)
                 VALUES ('legacy-spec', NULL, 'Spec', 1, 'pending');
             INSERT INTO queues (id, name, created_at) VALUES ('queue-1', 'queue-1', 0);
             INSERT INTO queue_members (queue_id, spec_id, position)
                 VALUES ('queue-1', 'legacy-spec', 1);",
        )
        .expect("seed legacy schema");
    }

    // Opening the DB (Database::new runs the migration) must succeed and add
    // `active_run_queue_id` without disturbing existing rows.
    let db = Database::new(&path).expect("open pre-B8 db, running migration");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.name, "Legacy");
    assert_eq!(lp.active_run_queue_id, None);

    // The new column is actually usable: persist a run context and reset
    // through the shared path picks up the queue's members.
    db.set_graph_active_run_queue("legacy-graph", Some("queue-1"))
        .unwrap();
    let outcome = db.reset_graph("legacy-graph", None).unwrap();
    assert_eq!(
        outcome,
        crate::domain::graphs::GraphResetOutcome::Reset {
            spec_count: 1,
            skipped_count: 0
        }
    );
    drop(db);

    // Reopening after the migration already ran must be a no-op: same data,
    // no error (idempotent).
    let db = Database::new(&path).expect("reopen db after migration already applied");
    let lp = db.get_graph("legacy-graph").unwrap().unwrap();
    assert_eq!(lp.active_run_queue_id.as_deref(), Some("queue-1"));
}
