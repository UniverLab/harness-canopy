//! Part 06 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// B18, end to end: the real incident. A queue-driven run's in-flight
/// member is left `running` by a daemon restart — a dangling node run
/// with no live process behind it — while another member sits `pending`
/// right behind it in the queue. G2 boot reconcile must mark the
/// in-flight member `Interrupted` (not `Pending` — the run was cut short
/// by something external, not a failure of the work) in the same pass it
/// interrupts the dangling run, and the resumed dispatch (what
/// `graph_control`'s `retry_current_node` triggers via
/// `resume_background`, simulated here by calling `run_graph_dispatch`
/// directly with `is_resume: true`) must pick the interrupted member up
/// FIRST — never skip straight past it to the next queued member, which
/// is exactly how it got orphaned in the 2026-07-14 incident.
#[tokio::test]
async fn graph_engine_restart_recovery_runs_interrupted_queue_spec_first() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let data_dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let initial_head = git_head(dir.path());

    let mut interrupted = standalone_spec("queue-interrupted", 1);
    interrupted.status = GraphSpecStatus::Running;
    let next = standalone_spec("queue-next", 2);
    db.insert_graph_spec(&interrupted).unwrap();
    db.insert_graph_spec(&next).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&interrupted.id, &next.id]);

    db.update_graph_status(
        &graph_id,
        GraphStatus::Running,
        Some(chrono::Utc::now()),
        None,
    )
    .unwrap();
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();

    db.insert_graph_node(&GraphNode {
            id: "graph-check".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": "echo committed >> log.txt && git add -A && git commit -q -m spec && printf APPROVED",
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

    // The daemon-restart artifact: a node run stuck `running` for the
    // in-flight spec, no live process behind it (no pid, no boot_id —
    // exactly what a dead daemon leaves for reconcile to find).
    db.insert_graph_run(&GraphNodeRun {
        id: "run-interrupted".to_string(),
        graph_id: graph_id.clone(),
        spec_id: interrupted.id.clone(),
        node_id: "graph-check".to_string(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    // G2 boot reconcile.
    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );
    let lp_after_reconcile = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp_after_reconcile.status, GraphStatus::Paused);
    let interrupted_after_reconcile = db.get_graph_spec(&interrupted.id).unwrap().unwrap();
    assert_eq!(
        interrupted_after_reconcile.status,
        GraphSpecStatus::Interrupted,
        "reconcile must mark the in-flight member interrupted, not leave it running"
    );

    // `graph_control { retry_current_node }`: resume with the graph's
    // persisted queue context, same as `resume_background`. The graph is left
    // `Paused` (as reconcile set it) — the dispatch's own atomic claim (B42)
    // owns the flip to `Running`, so no caller pre-flips it anymore.
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::resume(),
            None,
            None,
        )
        .await
        .unwrap();

    let lp_final = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp_final.status, GraphStatus::Completed);
    let interrupted_final = db.get_graph_spec(&interrupted.id).unwrap().unwrap();
    let next_final = db.get_graph_spec(&next.id).unwrap().unwrap();
    assert_eq!(interrupted_final.status, GraphSpecStatus::Completed);
    assert_eq!(next_final.status, GraphSpecStatus::Completed);

    // The interrupted spec ran FIRST — against the pre-existing HEAD,
    // before anything was committed — not skipped in favor of `next`.
    assert_eq!(
        interrupted_final.spec_start_head.as_deref(),
        Some(initial_head.as_str()),
        "the interrupted spec must be the first thing the resumed run picks up"
    );
    assert_ne!(
        next_final.spec_start_head.as_deref(),
        Some(initial_head.as_str()),
        "the next queued member must still run, but only after the interrupted one"
    );
}

/// FR2: a queue run whose failed predecessor left the worktree dirty pauses
/// before the next member starts; the blocker names the dirty path and the
/// ways out, and the graph is paused rather than failed.
#[tokio::test]
async fn failed_dirty_queue_spec_pauses_before_next_spec() {
    let (_dir, db, engine, graph_id, next_id) = dirty_queue_fixture(true, false).unwrap();
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::fresh(),
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "the guard pauses, never fails"
    );
    assert_eq!(
        db.get_graph_spec(&next_id).unwrap().unwrap().status,
        GraphSpecStatus::Pending,
        "the next spec must not start on a dirty tree"
    );
    let blocker = db
        .list_recent_activity_log_entries(50)
        .unwrap()
        .into_iter()
        .map(|entry| entry.message)
        .find(|message| message.starts_with("Blocked: "))
        .unwrap_or_default();
    assert!(
        blocker.contains("x.txt"),
        "blocker lists dirty paths: {blocker}"
    );
    for way_out in ["run_from_node", "skip_next_spec", "allow_dirty_start"] {
        assert!(
            blocker.contains(way_out),
            "blocker names {way_out}: {blocker}"
        );
    }
}

/// FR2: with `allow_dirty_start` on the graph the same dirty queue run
/// advances and completes the next member.
#[tokio::test]
async fn dirty_queue_guard_allow_dirty_start_advances() {
    let (_dir, db, engine, graph_id, next_id) = dirty_queue_fixture(true, true).unwrap();
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::fresh(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        db.get_graph_spec(&next_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "allow_dirty_start lets the next member run"
    );
}

/// FR2: a failed predecessor that left a clean tree does not block the next
/// queue member.
#[tokio::test]
async fn failed_clean_queue_spec_advances() {
    let (_dir, db, engine, graph_id, next_id) = dirty_queue_fixture(false, false).unwrap();
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::fresh(),
            None,
            None,
        )
        .await
        .unwrap();

    assert_eq!(
        db.get_graph_spec(&next_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "a clean tree lets the next member run"
    );
}

/// FR1: a directed run on a spec at its execution limit runs its first
/// node. The engine zeroes the count when the directed start begins, so the
/// node's failure is the first genuine verdict (count 1), not the one that
/// reaches the limit again (count 4, blocked).
#[tokio::test]
async fn directed_run_at_limit_resets_count_and_runs_first_node() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());
    let spec = standalone_spec("limit-directed", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for _ in 0..3 {
        db.increment_graph_spec_cross_run_attempts(&spec.id)
            .unwrap();
    }
    assert_eq!(db.get_graph_spec_cross_run_attempts(&spec.id).unwrap(), 3);

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::fresh_from(DirectedStart {
                spec_id: spec.id.clone(),
                node_arg: "graph-check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 1, "the first node ran: {runs:?}");
    assert_eq!(runs[0].node_id, "graph-check");
    assert_eq!(
        db.get_graph_spec_cross_run_attempts(&spec.id).unwrap(),
        1,
        "the directed start zeroed the count before this failure"
    );
}

/// FR2 way out: on a paused queue the dirty-tree guard held back the second
/// member, and `skip_next_spec` skips that held member. The `failed`
/// predecessor keeps its status.
#[test]
fn skip_next_spec_on_held_queue_skips_held_spec_not_failed_predecessor() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let failed = standalone_spec("held-failed", 1);
    let held = standalone_spec("held-next", 2);
    db.insert_graph_spec(&failed).unwrap();
    db.insert_graph_spec(&held).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&failed.id, &held.id]);
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();
    db.update_graph_spec_status(&failed.id, GraphSpecStatus::Failed, None, None)
        .unwrap();
    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();

    crate::daemon::handler_run_exec::handle_skip_next_spec(&db, &graph_id).unwrap();

    assert_eq!(
        db.get_graph_spec(&held.id).unwrap().unwrap().status,
        GraphSpecStatus::Skipped,
        "the held member is skipped"
    );
    assert_eq!(
        db.get_graph_spec(&failed.id).unwrap().unwrap().status,
        GraphSpecStatus::Failed,
        "the failed predecessor keeps its failure"
    );
}

/// C19/FR3: the execution-limit blocker names the named-spec `graph_control
/// reset` that clears the count, says the worktree is untouched, and gives
/// the relaunch verbs.
#[test]
fn execution_limit_blocker_names_graph_control_reset_and_spec_id() {
    let (_dir, db, engine, _graph_id) = bare_graph_fixture().unwrap();
    let spec = standalone_spec("limit-blocker", 1);
    db.insert_graph_spec(&spec).unwrap();
    let mut blocker = None;
    for _ in 0..engine.spec_attempt_limit {
        blocker = engine.record_spec_attempt(&spec, "boom", false).unwrap();
    }
    let blocker = blocker.expect("the limit is reached");
    assert!(blocker.contains("`graph_control reset`"), "{blocker}");
    assert!(
        blocker.contains(&format!("specs: [{}]", spec.id)),
        "names the spec id: {blocker}"
    );
    assert!(blocker.contains("worktree is not touched"), "{blocker}");
    assert!(blocker.contains("`graph_control run`"), "{blocker}");
    assert!(
        blocker.contains("`graph_control continue` with mode `run_from_node`"),
        "{blocker}"
    );
}

/// FR6: a completed graph's continue refusal names the status and the
/// `graph_control` way out.
#[test]
fn completed_graph_continue_refusal_names_graph_reset() {
    let refusal = crate::daemon::handler_run_directive::not_continuable_refusal(
        "wf-test",
        GraphStatus::Completed,
    );
    assert!(refusal.contains("'completed'"), "{refusal}");
    assert!(refusal.contains("graph_control"), "{refusal}");
    assert!(refusal.contains("run_from_node"), "{refusal}");
}

/// FR2: the blocker lists at most the first 20 dirty paths and says how
/// many more there are.
#[test]
fn dirty_advance_blocker_caps_listed_paths() {
    let paths: Vec<String> = (0..25).map(|i| format!("p{i}.txt")).collect();
    let blocker = launch_check::dirty_advance_blocker("next", "dirty", &paths);
    assert!(blocker.contains("p19.txt"), "{blocker}");
    assert!(!blocker.contains("p20.txt"), "{blocker}");
    assert!(blocker.contains("+5 more"), "{blocker}");
}

/// FR6: `retry_current_node` on a failed graph restarts the failed spec at
/// the node it failed on, and that node runs again.
#[tokio::test]
async fn retry_current_node_failed_graph_retries_failed_node() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());
    let spec = standalone_spec("retry-failed", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();
    seed_stopped_at_check_node(&db, &graph_id, &spec.id);
    db.update_graph_spec_status(&spec.id, GraphSpecStatus::Failed, None, None)
        .unwrap();
    db.update_graph_status(&graph_id, GraphStatus::Failed, None, None)
        .unwrap();

    let node = run_retry_restart(&db, &engine, &graph_id).await;
    assert_eq!(
        node.as_deref(),
        Some("graph-check"),
        "the failed node is the target"
    );
    assert_eq!(
        db.get_graph_spec(&spec.id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "the failed node ran again and passed"
    );
    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 2, "the seeded run plus the re-run: {runs:?}");
}

/// FR7: a paused graph whose current spec is `pending` with a recorded
/// current node resumes with `retry_current_node`: the current node runs
/// first and the spec goes through `running` to completion.
#[tokio::test]
async fn retry_pending_current_spec_with_cursor_resumes_at_current_node() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());
    let spec = standalone_spec("pending-cursor", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();
    seed_stopped_at_check_node(&db, &graph_id, &spec.id);
    db.update_graph_spec_status(&spec.id, GraphSpecStatus::Pending, None, None)
        .unwrap();
    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();

    let node = run_retry_restart(&db, &engine, &graph_id).await;
    assert_eq!(
        node.as_deref(),
        Some("graph-check"),
        "current_node is the restart target"
    );
    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 2, "current_node ran first: {runs:?}");
    assert_eq!(runs.last().unwrap().node_id, "graph-check");
    assert_eq!(
        db.get_graph_spec(&spec.id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
}

/// FR7 guard: a spec that is `running` on a paused graph keeps the
/// resumable path, so no directed restart is chosen for it.
#[test]
fn running_spec_keeps_resumable_path_not_directed_restart() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let spec = standalone_spec("running-spec", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();
    seed_stopped_at_check_node(&db, &graph_id, &spec.id);
    db.update_graph_spec_status(&spec.id, GraphSpecStatus::Running, None, None)
        .unwrap();
    db.update_graph_status(&graph_id, GraphStatus::Paused, None, None)
        .unwrap();
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert!(
        crate::daemon::handler_run_directive::retry_restart_target(&db, &lp)
            .unwrap()
            .is_none()
    );
}

/// End to end: a directed `graph_control run_from_node` on a
/// queue-driven graph targets the operator-named spec even when another
/// member is mistakenly still `running` — it must not be displaced by the
/// queue's `running` cursor (`queue_running_spec_id`, which
/// `run_resumed_queue_spec` consulted first and resumed the wrong member).
/// The directed spec runs; the stale running member is reset to `Pending`
/// so the directed run takes over the graph.
#[tokio::test]
async fn directed_queue_resume_resets_stray_running_member() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());

    let directed = {
        let mut s = standalone_spec("queue-directed", 1);
        s.status = GraphSpecStatus::Failed;
        s
    };
    let stale = {
        let mut s = standalone_spec("queue-stale-running", 2);
        s.status = GraphSpecStatus::Running;
        s
    };
    db.insert_graph_spec(&directed).unwrap();
    db.insert_graph_spec(&stale).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&directed.id, &stale.id]);

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
                spec_id: directed.id.clone(),
                node_arg: "check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let directed_after = db.get_graph_spec(&directed.id).unwrap().unwrap();
    let stale_after = db.get_graph_spec(&stale.id).unwrap().unwrap();
    assert_eq!(
        directed_after.status,
        GraphSpecStatus::Completed,
        "the directed spec must be the one that ran, not the stale running member"
    );
    assert_eq!(
        stale_after.status,
        GraphSpecStatus::Pending,
        "the stale running member must be reset to Pending so the directed run takes over"
    );
    assert!(
        db.list_graph_runs_for_spec(&stale.id).unwrap().is_empty(),
        "the stale member must get no new run row from this dispatch"
    );
    assert!(
        !db.list_graph_runs_for_spec(&directed.id)
            .unwrap()
            .is_empty(),
        "the directed spec must have run"
    );
}

/// Regression: a queue-driven graph Start -> Check -> Reviewer, where
/// Check's last run failed. A directed dispatch at Check must re-execute
/// Check (a new run row), and Reviewer must not run unless the new Check
/// passes. The historical Check output must not be reused/reported, and
/// the start message must name Check as the first executed node.
#[tokio::test]
async fn directed_queue_resume_reruns_failed_check_and_gates_reviewer_fail() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());

    let directed = {
        let mut s = standalone_spec("queue-directed", 1);
        s.status = GraphSpecStatus::Failed;
        s
    };
    db.insert_graph_spec(&directed).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&directed.id]);

    for (id, name, cmd) in [
        ("graph-start", "Start", "printf START"),
        ("graph-check", "Check", "printf FAIL; exit 1"),
        ("graph-reviewer", "Reviewer", "printf REVIEWED"),
    ] {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: name.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": cmd,
                "success_condition": "exit_code_0"
            }),
            position: match name {
                "Start" => 0,
                "Check" => 1,
                _ => 2,
            },
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }
    db.insert_graph_edge(&GraphEdge {
        id: "e-start-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "graph-start".to_string(),
        to_node: "graph-check".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-check-reviewer".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "graph-check".to_string(),
        to_node: "graph-reviewer".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();

    // Seed Check's historical Fail run (must not be reused).
    db.insert_graph_run(&GraphNodeRun {
        id: "run-check-old".to_string(),
        graph_id: graph_id.clone(),
        spec_id: directed.id.clone(),
        node_id: "graph-check".to_string(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"old": true})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
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
                spec_id: directed.id.clone(),
                node_arg: "Check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&directed.id).unwrap();
    let check_runs: Vec<_> = runs.iter().filter(|r| r.node_id == "graph-check").collect();
    // Historical + new run.
    assert!(
        check_runs.len() >= 2,
        "Check must be re-executed: {:?}",
        check_runs
    );
    let reviewer_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "graph-reviewer")
        .collect();
    assert!(
        reviewer_runs.is_empty(),
        "Reviewer must not run when the new Check fails: {:?}",
        reviewer_runs
    );

    // Start message names Check.
    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    assert!(
        events
            .iter()
            .any(|e| { e.message.contains("starts at node 'Check'") }),
        "start message must name Check: {:?}",
        events.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
    // Historical Check output must not be reported as reused.
    assert!(
        !events
            .iter()
            .any(|e| { e.message.contains("run-check-old") }),
        "historical Check run must not be reported/reused: {:?}",
        events.iter().map(|e| &e.message).collect::<Vec<_>>()
    );
}

/// Regression (passing variant): when Check passes on rerun, Reviewer runs.
#[tokio::test]
async fn directed_queue_resume_reruns_failed_check_and_gates_reviewer_pass() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());

    let directed = {
        let mut s = standalone_spec("queue-directed", 1);
        s.status = GraphSpecStatus::Failed;
        s
    };
    db.insert_graph_spec(&directed).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&directed.id]);

    for (id, name, cmd) in [
        ("graph-start", "Start", "printf START"),
        ("graph-check", "Check", "printf APPROVED"),
        ("graph-reviewer", "Reviewer", "printf REVIEWED"),
    ] {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: name.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": cmd,
                "success_condition": "exit_code_0"
            }),
            position: match name {
                "Start" => 0,
                "Check" => 1,
                _ => 2,
            },
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }
    db.insert_graph_edge(&GraphEdge {
        id: "e-start-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "graph-start".to_string(),
        to_node: "graph-check".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-check-reviewer".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "graph-check".to_string(),
        to_node: "graph-reviewer".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();

    db.insert_graph_run(&GraphNodeRun {
        id: "run-check-old".to_string(),
        graph_id: graph_id.clone(),
        spec_id: directed.id.clone(),
        node_id: "graph-check".to_string(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"old": true})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
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
                spec_id: directed.id.clone(),
                node_arg: "Check".to_string(),
            }),
            None,
            None,
        )
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&directed.id).unwrap();
    let reviewer_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "graph-reviewer")
        .collect();
    assert!(
        !reviewer_runs.is_empty(),
        "Reviewer must run after the new Check passes: {:?}",
        reviewer_runs
    );
}

/// CB50 FR2+FR3, end to end: a daemon crash mid-ensemble (one member
/// still `running`, no pid/boot_id — reconcile's dangling-run signature)
/// followed by a resume (`run_graph_dispatch(is_resume: true)`, what
/// `graph_control`'s `retry_current_node` triggers via
/// `resume_background`) must re-dispatch the WHOLE ensemble — every
/// current member, not just the one that was still running (FR3) — and
/// must not re-run "kickoff", which had already completed before the
/// crash. The two new member runs must share one iteration number (F1:
/// one ensemble dispatch is one iteration, not N).
#[tokio::test]
async fn graph_engine_resume_redispatches_full_ensemble_after_interrupt() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let data_dir = tempdir().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-ok-a",
            &write_member_script(dir.path(), "a.sh", "printf ok"),
        ),
        (
            "member-ok-b",
            &write_member_script(dir.path(), "b.sh", "printf ok"),
        ),
    ]);
    db.insert_graph_node(&touch_marker_node(
        "on-pass",
        &spec_id,
        &dir.path().join("pass.marker"),
        100,
    ))
    .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[("m-a", "member-ok-a"), ("m-b", "member-ok-b")],
        1,
        Some(1),
        None,
        "on-pass",
        None,
    );

    // Graph + spec mid-run: kickoff and member m-a already finished,
    // m-b is the crash artifact (running, no pid/boot_id — exactly what
    // a dead daemon leaves for reconcile to find). Explicit, well-spread
    // `started_at` values so `list_graph_runs_for_spec`'s
    // `ORDER BY started_at ASC` deterministically puts m-b last.
    db.update_graph_status(
        &graph_id,
        GraphStatus::Running,
        Some(chrono::Utc::now()),
        None,
    )
    .unwrap();
    db.update_graph_spec_status(
        &spec_id,
        GraphSpecStatus::Running,
        Some(chrono::Utc::now()),
        None,
    )
    .unwrap();
    let t0 = chrono::Utc::now() - chrono::Duration::seconds(3);
    db.insert_graph_run(&GraphNodeRun {
        id: "run-kickoff".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec_id.clone(),
        node_id: "kickoff".to_string(),
        status: GraphRunStatus::Pass,
        input: None,
        output: Some(serde_json::json!({"ok": true})),
        started_at: t0,
        completed_at: Some(t0),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
    let t1 = t0 + chrono::Duration::seconds(1);
    db.insert_graph_run(&GraphNodeRun {
        id: "run-m-a-1".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec_id.clone(),
        node_id: "m-a".to_string(),
        status: GraphRunStatus::Pass,
        input: Some(serde_json::json!({})),
        output: Some(serde_json::json!({"ok": true})),
        started_at: t1,
        completed_at: Some(t1),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
    let t2 = t1 + chrono::Duration::seconds(1);
    db.insert_graph_run(&GraphNodeRun {
        id: "run-m-b-1".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec_id.clone(),
        node_id: "m-b".to_string(),
        status: GraphRunStatus::Running,
        input: Some(serde_json::json!({})),
        output: None,
        started_at: t2,
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    // Boot reconcile: marks m-b's dangling run Fail (with an
    // "interrupted" marker output), the spec Interrupted, the graph
    // Paused. This is exactly what a real daemon restart mid-ensemble
    // leaves behind.
    assert_eq!(
        db.reconcile_orphaned_graphs(data_dir.path()).unwrap().len(),
        1
    );
    let spec_after_reconcile = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec_after_reconcile.status, GraphSpecStatus::Interrupted);

    // Resume: same call `resume_background` makes internally with
    // `is_resume: true`.
    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph_dispatch(
            graph_id.clone(),
            None,
            None,
            DispatchMode::resume(),
            None,
            None,
        )
        .await
        .unwrap();
    drop(_home);

    let kickoff_runs: Vec<_> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "kickoff")
        .collect();
    assert_eq!(
        kickoff_runs.len(),
        1,
        "kickoff had already completed before the crash and must not re-run on resume"
    );

    let m_a_runs: Vec<_> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "m-a")
        .collect();
    let m_b_runs: Vec<_> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "m-b")
        .collect();
    // The member scripts are unreported (CM13: script-backed test doubles
    // never call `graph_complete_node`) and exit 0 with output, so each
    // fresh dispatch earns the one reporting nudge before the ensemble
    // gives up on it: 2 rows (initial + nudge) per member, all sharing the
    // resumed dispatch's one iteration (`begin_infra_retry` passes the same
    // `iteration` through every retry).
    assert_eq!(
        m_a_runs.len(),
        3,
        "m-a already had one Pass run before the crash — resume must dispatch the whole \
             ensemble (FR3), so it gets 2 fresh (nudged) runs even though it wasn't the \
             member that was still running when the daemon died"
    );
    assert_eq!(
        m_b_runs.len(),
        3,
        "m-b's run count: the original (now Fail, from reconcile) plus 2 fresh \
             (nudged) runs from the resumed dispatch"
    );
    let new_m_a_iteration = m_a_runs.last().unwrap().iteration;
    let new_m_b_iteration = m_b_runs.last().unwrap().iteration;
    assert_eq!(
        new_m_a_iteration, new_m_b_iteration,
        "the two members re-dispatched together on resume must share one iteration number (F1)"
    );
}

#[tokio::test]
async fn graph_engine_run_workdir_override_is_used_as_check_node_cwd() {
    // The graph's own workdir must be left untouched by an override — only
    // the check node's actual working directory should change.
    let db_dir = tempdir().unwrap();
    let graph_workdir = tempdir().unwrap();
    let override_workdir = tempdir().unwrap();
    let db = Arc::new(Database::new(&db_dir.path().join("test.db")).unwrap());
    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));

    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-workdir".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: graph_workdir.path().to_string_lossy().to_string(),
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
        require_spec_type: false,
    };
    db.insert_graph(&lp).unwrap();
    let spec = standalone_spec("bound-spec", 1);
    let mut bound_spec = spec.clone();
    bound_spec.graph_id = Some(lp.id.clone());
    db.insert_graph_spec(&bound_spec).unwrap();

    let override_path = override_workdir.path().to_string_lossy().to_string();
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!("test \"$(pwd)\" = \"{override_path}\""),
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(
            lp.id.clone(),
            None,
            Some(override_path.clone()),
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let lp_after = db.get_graph(&lp.id).unwrap().unwrap();
    let spec_after = db.get_graph_spec(&bound_spec.id).unwrap().unwrap();
    assert_eq!(lp_after.status, GraphStatus::Completed);
    assert_eq!(spec_after.status, GraphSpecStatus::Completed);
    // The graph's own workdir is unchanged by the run-level override.
    assert_eq!(
        lp_after.workdir,
        graph_workdir.path().to_string_lossy().to_string()
    );
}

#[tokio::test]
async fn graph_engine_legacy_run_without_queue_id_only_touches_bound_specs() {
    // A standalone spec exists in the DB (e.g. queue backlog) but isn't
    // added to any queue and isn't bound to this graph. Calling run_graph
    // without queue_id must behave exactly as before queues existed: only
    // the graph's own bound specs are touched.
    let (_dir, db, engine, graph_id, bound_spec_id) = graph_fixture().unwrap();
    let untouched = standalone_spec("untouched-standalone", 99);
    db.insert_graph_spec(&untouched).unwrap();

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

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let bound = db.get_graph_spec(&bound_spec_id).unwrap().unwrap();
    let untouched_after = db.get_graph_spec(&untouched.id).unwrap().unwrap();

    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(bound.status, GraphSpecStatus::Completed);
    assert_eq!(untouched_after.status, GraphSpecStatus::Pending);
    assert!(db
        .list_graph_runs_for_spec(&untouched.id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn graph_engine_queue_run_skips_already_completed_members() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let mut done = standalone_spec("queue-done", 1);
    done.status = GraphSpecStatus::Completed;
    let pending = standalone_spec("queue-pending", 2);
    db.insert_graph_spec(&done).unwrap();
    db.insert_graph_spec(&pending).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&done.id, &pending.id]);

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

    engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let done_after = db.get_graph_spec(&done.id).unwrap().unwrap();
    let pending_after = db.get_graph_spec(&pending.id).unwrap().unwrap();

    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(done_after.status, GraphSpecStatus::Completed);
    assert_eq!(pending_after.status, GraphSpecStatus::Completed);
    // The already-completed spec was skipped outright: no run recorded.
    assert!(db.list_graph_runs_for_spec(&done.id).unwrap().is_empty());
    assert_eq!(db.list_graph_runs_for_spec(&pending.id).unwrap().len(), 1);
}

#[tokio::test]
async fn graph_engine_queue_run_retains_context_on_genuine_completion_for_progress() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let spec = standalone_spec("queue-spec", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);

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

    engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    // B31: a genuinely finished queue run keeps its `active_run_queue_id`
    // as last-run context so `graph list` / `graph info` can still render
    // its real progress instead of a misleading `0/0`. B8's
    // anti-pollution guarantee is upheld elsewhere: every launch path
    // re-persists this field before the first spec runs, so a later
    // fresh `graph_control` against a different queue overwrites it.
    assert_eq!(
        lp.active_run_queue_id.as_deref(),
        Some("queue-1"),
        "a genuinely finished queue run must keep the run context so its queue progress \
             stays queryable"
    );
    // The progress the CLI/MCP surfaces (mirrored by `graph_progress` in
    // `daemon/graph_cli.rs`) is a real `1/1`, not `0/0`.
    assert_eq!(
        engine
            .spec_progress(&graph_id, lp.active_run_queue_id.as_deref())
            .unwrap(),
        (1, 1),
        "completed queue graph must report n/n progress, not 0/0"
    );
}

/// If `queue_next_pending_spec_id` finds no `pending` member to pick, but a
/// member is nonetheless left non-terminal (e.g. `running`, from a crash
/// mid-spec that never got reset), the queue isn't genuinely finished —
/// the graph must not be marked `completed` out from under it. This is
/// the guard that keeps a resumed queue run from repeating the incident's
/// false-completion (17 of 20 queue specs still pending, graph marked
/// completed anyway).
#[tokio::test]
async fn graph_engine_queue_run_does_not_complete_graph_while_member_left_running() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let mut stuck = standalone_spec("queue-stuck", 1);
    stuck.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&stuck).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&stuck.id]);

    engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Running,
        "must not be marked completed while a queue member is still non-terminal"
    );
    assert_eq!(
        lp.active_run_queue_id.as_deref(),
        Some("queue-1"),
        "the run context must survive so a later resume still knows the queue"
    );
}

#[tokio::test]
async fn graph_engine_queue_run_settles_failed_when_member_failed() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let spec_a = standalone_spec("queue-a", 1);
    let mut spec_b = standalone_spec("queue-b", 2);
    spec_b.status = GraphSpecStatus::Failed;
    let spec_c = standalone_spec("queue-c", 3);
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_spec(&spec_c).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec_a.id, &spec_b.id, &spec_c.id]);

    // A stored blocker summary for the already-failed member.
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
    db.insert_graph_run(&GraphNodeRun {
        id: "run-b".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec_b.id.clone(),
        node_id: "graph-check".to_string(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"blocker": "boom"})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    let marker = dir.path().join("failed-hook.marker");
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!(
            "printf '%s' '{{{{completed_specs}}}}' > '{}'",
            marker.display()
        )),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(crate::domain::graphs::GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);

    let blocker_text = std::fs::read_to_string(&marker).unwrap_or_default();
    assert!(
        blocker_text.contains("queue-a"),
        "completed_specs must include A: {blocker_text}"
    );
    assert!(
        blocker_text.contains("queue-c"),
        "completed_specs must include C: {blocker_text}"
    );

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(
        hook_runs[0].event,
        crate::domain::graphs::GraphHookEvent::OnFailed
    );

    // No dangling running node runs remain.
    assert!(db.list_running_graph_runs(&graph_id).unwrap().is_empty());
}

/// A queue-advance guard pause stores the blocker on the graph row — so
/// `graph_get`/`graph_list` report it instead of `blocked: false` — fires
/// `on_blocked` exactly once, and names the previous spec by name only with
/// no doubled period. Resuming clears the stored blocker.
#[tokio::test]
async fn guard_pause_stores_blocker_and_fires_on_blocked_once() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    // A failed predecessor that ended dirty on this workdir, and the dirt
    // still in the tree.
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "pred-1",
        "PredOne",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.set_graph_spec_end_state("pred-1", Some(1), &["leftover.txt".to_string()])
        .unwrap();
    std::fs::write(dir.path().join("leftover.txt"), "x").unwrap();

    let next = standalone_spec("next-1", 1);
    db.insert_graph_spec(&next).unwrap();
    // The failed predecessor is a member of this queue run, so the guard
    // still weighs its dirt; a stale record from outside the run is ignored.
    insert_queue_with_members(&db, "queue-1", &["pred-1", &next.id]);

    let blocked_file = dir.path().join("blocked.txt");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnBlocked,
        vec![crate::domain::graphs::GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some(format!("printf blocked > {}", blocked_file.display())),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
            crew: None,
        }],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine
        .run_graph_dispatch(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            DispatchMode::fresh(),
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "the guard must pause, never fail"
    );
    let blocker = db
        .graph_blocker(&graph_id)
        .unwrap()
        .expect("a guard pause must store a blocker on the graph row");
    assert!(blocker.contains("leftover.txt"), "{blocker}");
    assert!(blocker.contains("spec 'PredOne'"), "{blocker}");
    assert!(
        !blocker.contains("(graph '"),
        "queue-driven text names the spec only: {blocker}"
    );
    assert!(!blocker.contains(".."), "no doubled period: {blocker}");

    // The stored text is the graph_blocked event's text (modulo its prefix).
    let events = db
        .list_graph_events(
            &graph_id,
            None,
            None,
            Some(&["graph_blocked".to_string()]),
            50,
        )
        .unwrap();
    assert_eq!(
        events.len(),
        1,
        "exactly one graph_blocked event, got {events:?}"
    );
    assert_eq!(events[0].message, format!("Blocked: {blocker}"));

    // The get/list surfaces show the same blocker.
    let summary = crate::daemon::handler_read::build_graph_summary_json(&db, &lp).unwrap();
    assert_eq!(summary["blocked"], serde_json::json!(true));
    assert_eq!(summary["blocker"], serde_json::json!(blocker));
    let details = db.get_graph_details(&graph_id).unwrap().unwrap();
    let details_json = crate::daemon::handler_read::graph_details_json(&db, &details).unwrap();
    assert_eq!(details_json["blocked"], serde_json::json!(true));
    assert_eq!(details_json["blocker"], serde_json::json!(blocker));

    // on_blocked fired exactly once.
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(
        hook_runs.len(),
        1,
        "on_blocked must fire once, got {hook_runs:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&blocked_file).unwrap_or_default(),
        "blocked"
    );

    // Resuming clears the blocker.
    assert!(db
        .claim_graph_for_run(&graph_id, chrono::Utc::now())
        .unwrap());
    assert_eq!(
        db.graph_blocker(&graph_id).unwrap(),
        None,
        "resume clears the blocker"
    );
}
