//! Part 07 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

#[tokio::test]
async fn graph_engine_queue_run_settles_completed_when_all_done() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let spec_a = standalone_spec("queue-a", 1);
    let spec_b = standalone_spec("queue-b", 2);
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec_a.id, &spec_b.id]);

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

    let marker = dir.path().join("completed-hook.marker");
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
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnCompleted,
        vec![hook],
    );
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
    assert_eq!(lp.status, GraphStatus::Completed);

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(
        hook_runs[0].event,
        crate::domain::graphs::GraphHookEvent::OnCompleted
    );

    assert!(db.list_running_graph_runs(&graph_id).unwrap().is_empty());
}

#[tokio::test]
async fn graph_engine_queue_run_picks_up_spec_appended_mid_run() {
    // A spec appended to the queue while the run is in flight must still
    // get executed before the run ends: the engine re-queries the queue
    // for its next pending member at each spec boundary instead of
    // iterating a list frozen at launch.
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let started_marker = dir.path().join("started.marker");
    let go_marker = dir.path().join("go.marker");
    let order_log = dir.path().join("order.log");

    let spec_a = standalone_spec("queue-spec-a", 1);
    let spec_b = standalone_spec("queue-spec-b", 2);
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    // spec_b exists in the DB but is NOT yet in the queue — it's appended
    // below, while spec_a is mid-run.
    insert_queue_with_members(&db, "queue-1", &[&spec_a.id]);

    db.insert_graph_node(&touch_gate_node(
        "node-a",
        &spec_a.id,
        &started_marker,
        &go_marker,
    ))
    .unwrap();
    db.insert_graph_node(&record_node("node-b", &spec_b.id, &order_log, "spec-b"))
        .unwrap();

    let run_engine = engine.clone();
    let run_graph_id = graph_id.clone();
    let handle = tokio::spawn(async move {
        run_engine
            .run_graph(
                run_graph_id,
                Some("queue-1".to_string()),
                None,
                None,
                None,
                None,
            )
            .await
    });

    wait_for_file(&started_marker).await;
    // spec_a is mid-run (blocked on the gate). Append spec_b to the queue
    // now, while the run is in flight.
    db.append_queue_member("queue-1", &spec_b.id, None).unwrap();
    std::fs::write(&go_marker, "").unwrap();

    handle.await.unwrap().unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec_a_after = db.get_graph_spec(&spec_a.id).unwrap().unwrap();
    let spec_b_after = db.get_graph_spec(&spec_b.id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec_a_after.status, GraphSpecStatus::Completed);
    assert_eq!(
        spec_b_after.status,
        GraphSpecStatus::Completed,
        "spec appended mid-run must still be executed before the run ends"
    );
    assert_eq!(db.list_graph_runs_for_spec(&spec_b.id).unwrap().len(), 1);
}

#[tokio::test]
async fn graph_engine_queue_run_reorder_changes_pick_order_mid_run() {
    // Reordering the queue's PENDING members while a run is in flight
    // must change which one the engine picks next — proving the pick is
    // a live, fresh query, not a list captured at launch.
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let started_marker = dir.path().join("started.marker");
    let go_marker = dir.path().join("go.marker");
    let order_log = dir.path().join("order.log");

    let spec_a = standalone_spec("queue-spec-a", 1);
    let spec_b = standalone_spec("queue-spec-b", 2);
    let spec_c = standalone_spec("queue-spec-c", 3);
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_spec(&spec_c).unwrap();
    // Queue order at launch: a, b, c.
    insert_queue_with_members(&db, "queue-1", &[&spec_a.id, &spec_b.id, &spec_c.id]);

    db.insert_graph_node(&touch_gate_node(
        "node-a",
        &spec_a.id,
        &started_marker,
        &go_marker,
    ))
    .unwrap();
    db.insert_graph_node(&record_node("node-b", &spec_b.id, &order_log, "spec-b"))
        .unwrap();
    db.insert_graph_node(&record_node("node-c", &spec_c.id, &order_log, "spec-c"))
        .unwrap();

    let run_engine = engine.clone();
    let run_graph_id = graph_id.clone();
    let handle = tokio::spawn(async move {
        run_engine
            .run_graph(
                run_graph_id,
                Some("queue-1".to_string()),
                None,
                None,
                None,
                None,
            )
            .await
    });

    wait_for_file(&started_marker).await;
    // spec_a is mid-run. Swap the two PENDING members' order: c before b.
    db.reorder_queue_members(
        "queue-1",
        &[spec_a.id.clone(), spec_c.id.clone(), spec_b.id.clone()],
    )
    .unwrap();
    std::fs::write(&go_marker, "").unwrap();

    handle.await.unwrap().unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let order = std::fs::read_to_string(&order_log).unwrap();
    let lines: Vec<&str> = order.lines().collect();
    assert_eq!(
        lines,
        vec!["spec-c", "spec-b"],
        "reorder mid-run must change which pending spec runs next"
    );
}

#[tokio::test]
async fn graph_engine_queue_run_ends_when_no_pending_members_remain() {
    // Sanity check underpinning both tests above: with no gating at all,
    // a queue run with N pending members ends after exactly N specs run,
    // and picks up an appended spec before completing.
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let order_log = dir.path().join("order.log");

    let spec_a = standalone_spec("queue-spec-a", 1);
    let spec_b = standalone_spec("queue-spec-b", 2);
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec_a.id, &spec_b.id]);

    db.insert_graph_node(&record_node("node-a", &spec_a.id, &order_log, "spec-a"))
        .unwrap();
    db.insert_graph_node(&record_node("node-b", &spec_b.id, &order_log, "spec-b"))
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
    assert!(db.queue_next_pending_spec_id("queue-1").unwrap().is_none());

    let order = std::fs::read_to_string(&order_log).unwrap();
    assert_eq!(order.lines().collect::<Vec<_>>(), vec!["spec-a", "spec-b"]);
}

// ── E2BIG resilience: spawn failure routes through fail edge ──────

/// Agent spawn failure produces the correct NodeExecution shape that
/// `select_next_step` can route. This tests the contract between
/// `run_agent_process` (which catches E2BIG / spawn errors) and the
/// graph router (which selects the next node based on status).
#[tokio::test]
async fn agent_spawn_failure_node_execution_is_routable() {
    let (_dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let mut strategy = sample_strategy("/nonexistent/somewhere/definitely-not-a-binary");
    strategy.prompt_via_stdin = false;
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .expect("spawn failure must not propagate as a hard error");

    // The execution must be a Fail — exactly what select_next_step matches
    // against the Fail edge condition.
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(execution.summary.contains("failed to spawn"));

    // Verify the output JSON has the fields the graph engine expects.
    let output = &execution.output;
    assert_eq!(output.get("kind").and_then(Value::as_str), Some("agent"));
    assert_eq!(
        output.get("node_id").and_then(Value::as_str),
        Some("node-agent")
    );
    assert!(output.get("error").is_some(), "must include error message");
}

/// The full E2BIG resilience path: prompt is built (with elision),
/// delivered via stdin (no argv cliff), and a spawn failure is caught
/// as a node-level failure. This exercises the three components that
/// together prevent the E2BIG incident from recurring:
/// 1. `bound_previous_feedback` — truncates large prior output
/// 2. `CliStrategy::build_command` with `prompt_via_stdin` — avoids argv
/// 3. `run_agent_process` — catches spawn errors as node failures
#[tokio::test]
async fn e2big_resilience_path_elision_stdin_and_spawn_failure() {
    // 1. Simulate a huge previous_feedback (like a 65KB cargo test log).
    let huge_log = "x".repeat(500 * 1024);
    let bounded = bound_previous_feedback(huge_log.clone());
    assert!(
        bounded.contains("bytes elided"),
        "large feedback must be elided"
    );
    assert!(
        bounded.len() < 200 * 1024,
        "elided feedback must be well under argv limit"
    );

    // 2. Render the full prompt — elision must survive composition.
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: "/tmp/project".to_string(),
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
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: Some("Do the thing".to_string()),
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
    };
    let node = GraphNode {
        id: "node-1".to_string(),
        spec_id: Some("spec".to_string()),
        graph_id: None,
        name: "Agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let prompt = render_agent_prompt(
        &lp,
        &spec,
        &node,
        "{{previous_feedback}}",
        Some(&serde_json::json!({"stdout": huge_log})),
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();
    assert!(
        prompt.contains("bytes elided"),
        "composed prompt must contain the elision marker"
    );
    assert!(
        prompt.len() < 300 * 1024,
        "composed prompt must stay well under argv limit"
    );

    // 3. Deliver via stdin — no E2BIG even for oversized prompts.
    //    `cat` echoes stdin to stdout; the full output must arrive intact
    //    (the output contains the prompt twice: once via {{previous_feedback}}
    //    template substitution, once as the explicit # [PREVIOUS FEEDBACK]
    //    section — so stdout != prompt; we check it arrives in full instead).
    let (_dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let mut strategy = sample_strategy("/bin/cat");
    strategy.prompt_via_stdin = true;
    let stdin_node = sample_agent_node();

    let execution = run_agent_process(
        &db,
        "run-test",
        &cli,
        &strategy,
        &stdin_node,
        &prompt,
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await;
    let result = execution.expect("stdin delivery must not fail");
    // CM13: cat never self-reports, so the run is infra (Fail).
    assert_eq!(result.status, GraphRunStatus::Fail);
    let stdout = result.output.get("stdout").and_then(Value::as_str).unwrap();
    assert!(
        stdout.contains("bytes elided"),
        "stdout from cat must contain the elision marker"
    );

    // 4. Spawn failure caught as node failure (not hard error).
    let mut fail_strategy = sample_strategy("/nonexistent/binary");
    fail_strategy.prompt_via_stdin = false;
    let fail_result = run_agent_process(
        &db,
        "run-test",
        &cli,
        &fail_strategy,
        &stdin_node,
        &prompt,
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .expect("spawn failure must not propagate as hard error");
    assert_eq!(fail_result.status, GraphRunStatus::Fail);
}

/// Full engine integration: a node failure must route through the graph's
/// fail edge and let the graph continue — never abort the entire graph run.
/// This proves the resilience contract that the E2BIG fix depends on:
/// when `run_agent_process` returns a failed `NodeExecution` (instead of
/// propagating `Err`), the engine routes it through the fail edge.
#[tokio::test]
async fn graph_engine_node_failure_routes_through_fail_edge_and_graph_continues() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // "implement" node: always fails (simulates any node failure,
    // including an agent spawn failure that's caught by
    // `run_agent_process`).
    db.insert_graph_node(&GraphNode {
        id: "node-implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // "review" node: runs after the failure, proving the graph survived.
    db.insert_graph_node(&GraphNode {
        id: "node-review".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "review".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // implement --fail--> review
    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-implement".to_string(),
        to_node: "node-review".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    // The graph must complete (not fail/abort), the spec must complete
    // (review passed), and both nodes must have run.
    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(runs.len(), 2);

    let implement_run = runs.iter().find(|r| r.node_id == "node-implement").unwrap();
    assert_eq!(implement_run.status, GraphRunStatus::Fail);

    let review_run = runs.iter().find(|r| r.node_id == "node-review").unwrap();
    assert_eq!(review_run.status, GraphRunStatus::Pass);
}

/// CM47: notes survive a fail-edge bounce (req 9.5, second half). A gate
/// fails once (bouncing back to a worker via a fail edge) then passes;
/// the note written before the run must survive the bounce.
#[cfg(unix)]
#[tokio::test]
async fn spec_notes_survive_a_fail_edge_bounce() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // worker: always passes.
    db.insert_graph_node(&GraphNode {
        id: "worker".into(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "worker".into(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command":"exit 0","success_condition":"exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // gate: fails on the 1st run, passes on the 2nd (a counter file in `dir`).
    let counter = dir.path().join("gate-counter");
    db.insert_graph_node(&GraphNode {
            id: "gate".into(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "gate".into(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": format!(
                    "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; [ \"$n\" -ge 2 ]",
                    c = counter.display()
                ),
                "success_condition": "exit_code_0"
            }),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

    // worker --Pass--> gate ; gate --Fail--> worker  (the bounce).
    db.insert_graph_edge(&GraphEdge {
        id: "e-w-g".into(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".into(),
        to_node: "gate".into(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-g-w".into(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "gate".into(),
        to_node: "worker".into(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    // A note already written for this spec, before the run.
    db.spec_note_write(crate::db::graphs::notes::NewSpecNote {
        spec_id: &spec_id,
        owner_kind: "node",
        owner_id: "worker",
        owner_name: "Worker",
        body: "keep me across the bounce",
        author_platform: None,
        author_model: None,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    // gate bounced once (fail) then passed: the note must survive.
    let notes = db.spec_notes_get(&spec_id).unwrap();
    assert_eq!(notes.len(), 1, "the bounce must not delete notes");
    assert_eq!(notes[0].body, "keep me across the bounce");
    let gate_runs = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "gate")
        .count();
    assert_eq!(
        gate_runs, 2,
        "gate must fail once and bounce back to worker"
    );
}

// ── B42: a superseded run is terminal and silent ─────────────────────

/// The core of the runaway: an in-flight node run superseded by a newer
/// attempt at the same node must traverse NO edge. Its graph has a fail
/// edge to a "resilience" node — exactly the shape that manufactured a
/// fresh Resilience run per killed implementer — and that node must never
/// run, because a supersede is engine bookkeeping, not a node failure.
#[cfg(unix)]
#[tokio::test]
async fn superseded_run_traverses_no_edge_and_creates_no_resilience_run() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // "implement": a long-running node we can supersede mid-flight. A
    // killed process exits nonzero, so absent the fix its `Fail` would
    // route straight down the fail edge below.
    db.insert_graph_node(&GraphNode {
        id: "implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 30",
            "success_condition": "exit_code_0",
            "timeout_seconds": 60,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // "resilience": the fail-edge target that must NEVER run for a supersede.
    db.insert_graph_node(&GraphNode {
        id: "resilience".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf DIAGNOSED",
            "success_condition": "exit_code_0",
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "implement".to_string(),
        to_node: "resilience".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    let engine = Arc::new(engine);
    let dispatch = {
        let engine = Arc::clone(&engine);
        let graph_id = graph_id.clone();
        tokio::spawn(async move {
            engine
                .run_graph(graph_id, None, None, None, None, None)
                .await
        })
    };

    // Once the implement run is live (has a pid), supersede it exactly as a
    // newer attempt at the same node does — the same
    // `terminate_run(&stale, SUPERSEDE_REASON)` the reap graph runs. Polling
    // to the pid makes the ordering deterministic: the row is finalized
    // superseded before the killed process's `wait` ever returns.
    let superseded_run_id = loop {
        if let Some(run) = db.get_active_graph_run_for_node("implement").unwrap() {
            if run.pid.is_some() {
                let workdir = crate::graph_engine::routing::workdir_for_run(&db, &run);
                terminate_run_row(&db, &run, SUPERSEDE_REASON, &workdir);
                break run.id;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };

    dispatch.await.unwrap().unwrap();

    // The superseded run is recorded terminated/superseded...
    let superseded = db.get_graph_run(&superseded_run_id).unwrap().unwrap();
    assert_eq!(superseded.status, GraphRunStatus::Fail);
    assert!(
        run_was_terminated_out_of_band(&superseded),
        "the run must carry the supersede marker"
    );

    // ...and it traversed no edge: NO resilience run was ever created, and
    // the only run for the spec is the one superseded implement run.
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        runs.iter().all(|r| r.node_id != "resilience"),
        "a superseded run must not route down the fail edge to the resilience node"
    );
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].node_id, "implement");

    // The dispatch stopped silently — it failed nothing and completed
    // nothing; the graph and spec are left for whoever now owns them.
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Running);
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Running);
}

/// The same mechanism, generalized (2026-08-05): a run terminated
/// out-of-band for ANY reason — not just a same-node supersede — must be
/// recognized and traverse no edge. `graph_reset` marks a run it kills
/// with reason `"spec reset"`, not `SUPERSEDE_REASON`; before the fix
/// this reason mismatch meant a spec reset out from under an executing
/// node let its late completion route the fail edge anyway.
#[cfg(unix)]
#[tokio::test]
async fn run_terminated_for_any_out_of_band_reason_also_traverses_no_edge() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 30",
            "success_condition": "exit_code_0",
            "timeout_seconds": 60,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "resilience".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf DIAGNOSED",
            "success_condition": "exit_code_0",
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "implement".to_string(),
        to_node: "resilience".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    let engine = Arc::new(engine);
    let dispatch = {
        let engine = Arc::clone(&engine);
        let graph_id = graph_id.clone();
        tokio::spawn(async move {
            engine
                .run_graph(graph_id, None, None, None, None, None)
                .await
        })
    };

    // Terminate the live run exactly as `Database::reset_graph` does when
    // it finds an in-flight run for a spec being reset: same
    // `{ "terminated": true, "reason": … }` shape, but a different
    // reason than the same-node supersede path uses.
    let terminated_run_id = loop {
        if let Some(run) = db.get_active_graph_run_for_node("implement").unwrap() {
            if run.pid.is_some() {
                let workdir = crate::graph_engine::routing::workdir_for_run(&db, &run);
                terminate_run_row(&db, &run, "spec reset", &workdir);
                break run.id;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };

    dispatch.await.unwrap().unwrap();

    let terminated = db.get_graph_run(&terminated_run_id).unwrap().unwrap();
    assert_eq!(terminated.status, GraphRunStatus::Fail);
    assert!(run_was_terminated_out_of_band(&terminated));

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        runs.iter().all(|r| r.node_id != "resilience"),
        "an out-of-band termination for any reason must not route down the fail edge"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Running);
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Running);
}

// ── terminal blocker: dead-ending on a failing node must self-explain ──

/// The defect this closes (2026-08-06, graph 824de730): a spec that dies
/// because a FAILING node has no outgoing edge left NOTHING visible
/// beyond a log line — no blocker anywhere `graph_list`/the TUI could
/// show. The engine must now derive one, naming the node and what it
/// reported, onto the terminating run's `output.blocker` — the exact
/// key `graph_run_blocker` (daemon/handler.rs) already reads.
#[tokio::test]
async fn terminal_fail_node_with_no_outgoing_edge_records_a_derived_blocker() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

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
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // No outgoing edge from "dead-end" for either status: the spec
    // dead-ends right here.

    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Failed);

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Fail);
    let blocker = runs[0]
        .output
        .as_ref()
        .and_then(|output| output.get("blocker"))
        .and_then(Value::as_str)
        .expect("a failing dead-end must record a blocker");
    assert!(blocker.contains("dead-end"), "blocker: {blocker}");
}

/// The mirror image: a PASSING dead-end (no outgoing edge for `Pass`)
/// is the normal, correct end of a spec — exactly what `Check
/// committed` does on every successful spec — and must record no
/// blocker at all. Getting this wrong would mark every healthy spec as
/// blocked.
#[tokio::test]
async fn terminal_pass_node_with_no_outgoing_edge_records_no_blocker() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "check-committed".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check-committed".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 0",
            "success_condition": "exit_code_0",
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert!(
        runs[0]
            .output
            .as_ref()
            .is_none_or(|output| output.get("blocker").is_none()),
        "a passing dead-end must never record a blocker"
    );
}

// ── C19: cross-run attempt budget ─────────────────────────────────

/// The regression test for the whole spec: a spec that keeps failing
/// with a genuine verdict — never an infra crash — across three
/// SEPARATE `run_graph` dispatches (not three bounces within one, which
/// is the pre-existing per-node `graphs.max_iterations_per_node`
/// budget) must end up `Blocked`, not `Failed`: the graph pauses, and the
/// terminating run's blocker names the spec and the attempt count.
#[tokio::test]
async fn cross_run_attempt_budget_blocks_graph_after_three_failed_executions() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

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
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // No outgoing edge from "dead-end": every execution dead-ends
    // there as a genuine (Check-node, never infra-classified) Fail.

    // Executions 1 and 2: ordinary Failed, not yet blocked.
    for expected_attempts in 1..=2 {
        engine
            .run_graph(graph_id.clone(), None, None, None, None, None)
            .await
            .unwrap();
        let lp = db.get_graph(&graph_id).unwrap().unwrap();
        assert_eq!(
            lp.status,
            GraphStatus::Failed,
            "attempt {expected_attempts}"
        );
        assert_eq!(
            db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(),
            expected_attempts,
            "attempt count must persist across separate executions"
        );
    }

    // Execution 3: the budget (default 3) is now exceeded — blocked.
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(), 3);
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "the third genuine failure must block the graph, not just fail it"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let blocker = runs
        .last()
        .unwrap()
        .output
        .as_ref()
        .and_then(|output| output.get("blocker"))
        .and_then(Value::as_str)
        .expect("the terminating run must carry the C19 blocker");
    assert!(
        blocker.contains(&spec_id) || blocker.contains("Spec"),
        "{blocker}"
    );
    assert!(
        blocker.contains('3'),
        "blocker must name the attempt count: {blocker}"
    );
}

/// Decision 2: an infrastructure failure (here, `no_output` — an agent
/// that exits 0 with nothing to say) never consumes the cross-run
/// budget. Verified directly against `execution_is_infra_failure` /
/// `record_spec_attempt` — the exact pair `run_spec` consults — rather
/// than fighting the test-cli harness into reproducing a raw process
/// crash end-to-end.
#[tokio::test]
async fn infra_marked_output_does_not_consume_the_cross_run_budget() {
    let (_dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();

    for infra_output in [
        serde_json::json!({"infra_crash": true, "infra_attempt": 0}),
        serde_json::json!({"no_output": true}),
        serde_json::json!({"failure_kind": "no_report"}),
        serde_json::json!({"failure_kind": "unreported"}),
    ] {
        assert!(execution_is_infra_failure(&infra_output), "{infra_output}");
        let blocked = engine
            .record_spec_attempt(&spec, "infra blip", true)
            .unwrap();
        assert!(blocked.is_none(), "an infra failure must never block");
    }
    assert_eq!(
        db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(),
        0,
        "none of the infra-flavoured failures above may have touched the counter"
    );

    // A genuine failure, by contrast, does.
    assert!(!execution_is_infra_failure(&serde_json::json!({})));
    engine
        .record_spec_attempt(&spec, "a real fail", false)
        .unwrap();
    assert_eq!(db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(), 1);
}

/// Decision 6: an EXPLICIT `graph_reset` of the specific spec that hit
/// the budget clears its count, so the next execution starts fresh
/// rather than being blocked on its very first fail. The counter must
/// still be shared across executions otherwise — this is the one
/// deliberate escape hatch, not a general amnesty.
#[tokio::test]
async fn explicit_spec_reset_clears_the_cross_run_attempt_count() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

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
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Two genuine failures — one short of the default budget of 3.
    for _ in 0..2 {
        engine
            .run_graph(graph_id.clone(), None, None, None, None, None)
            .await
            .unwrap();
    }
    assert_eq!(db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(), 2);

    // The operator names this spec explicitly — the "I fixed it" signal.
    let outcome = db
        .reset_graph(&graph_id, Some(std::slice::from_ref(&spec_id)))
        .unwrap();
    assert!(matches!(
        outcome,
        crate::domain::graphs::GraphResetOutcome::Reset {
            spec_count: 1,
            skipped_count: 0
        }
    ));
    assert_eq!(
        db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(),
        0,
        "an explicit reset of this exact spec must clear its count"
    );

    // The next execution starts fresh: one more genuine failure lands
    // at count 1, not 3 — still an ordinary Failed, not Blocked.
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(), 1);
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Failed,
        "must not be blocked so soon after reset"
    );
}

/// The mirror image, and decision 6's other half: a BLANKET
/// `graph_reset` (no `specs` named) resets the spec's status like any
/// other, but must NOT clear its attempt count — that's exactly the
/// "operator resets and relaunches without fixing anything" recovery
/// this whole spec exists to stop from silently resetting the budget.
#[tokio::test]
async fn blanket_graph_reset_does_not_clear_the_cross_run_attempt_count() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

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
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    assert_eq!(db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(), 1);

    db.reset_graph(&graph_id, None).unwrap();
    assert_eq!(
        db.get_graph_spec_cross_run_attempts(&spec_id).unwrap(),
        1,
        "a blanket reset must leave the persisted count untouched"
    );
}

/// A blocker already written by `graph_report_blocker` is more specific
/// than anything the engine can synthesise — `record_terminal_blocker`
/// must never overwrite it.
#[tokio::test]
async fn record_terminal_blocker_preserves_an_existing_blocker() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "node".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let run_id = "run-with-existing-blocker".to_string();
    let existing_output = serde_json::json!({ "blocker": "human already reported this" });
    db.insert_graph_run(&GraphNodeRun {
        id: run_id.clone(),
        graph_id,
        spec_id,
        node_id: "node".to_string(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(existing_output.clone()),
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

    let final_execution = NodeExecution {
        status: GraphRunStatus::Fail,
        output: existing_output,
        summary: "whatever the node reported".to_string(),
    };
    engine
        .record_terminal_blocker(&run_id, "node", &final_execution)
        .unwrap();

    let run = db.get_graph_run(&run_id).unwrap().unwrap();
    let blocker = run
        .output
        .as_ref()
        .and_then(|output| output.get("blocker"))
        .and_then(Value::as_str)
        .unwrap();
    assert_eq!(blocker, "human already reported this");
}

// ── fail_graph: scoped to the dispatch generation that failed ─────────

/// `fail_graph`'s sweep must never terminate a sibling dispatch's healthy
/// run — the exact way the 2026-08-05 incident took down a fresh,
/// correct dispatch that had claimed the graph 64 seconds after the one
/// that eventually failed. Simulates the race directly: dispatch A
/// claims, a reset + relaunch (dispatch B) claims again with a later
/// timestamp and starts its own run, and only then does dispatch A's
/// late failure arrive carrying its now-stale claim.
#[tokio::test]
async fn fail_graph_from_stale_dispatch_never_touches_a_newer_dispatchs_runs() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-a".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "node-a".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let claim_a = chrono::Utc::now();
    assert!(db.claim_graph_for_run(&graph_id, claim_a).unwrap());

    // A reset + relaunch out from under dispatch A: status drops out of
    // `running` (what `Database::reset_graph` does), then dispatch B
    // claims again with a strictly later timestamp.
    db.update_graph_status(&graph_id, GraphStatus::Draft, None, None)
        .unwrap();
    let claim_b = claim_a + chrono::Duration::seconds(5);
    assert!(db.claim_graph_for_run(&graph_id, claim_b).unwrap());

    // Dispatch B's own healthy, in-flight run.
    db.insert_graph_run(&GraphNodeRun {
        id: "run-b".to_string(),
        graph_id: graph_id.clone(),
        spec_id,
        node_id: "node-a".to_string(),
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

    // Dispatch A's late failure, carrying its now-stale claim.
    engine
        .fail_graph(
            &graph_id,
            Some(claim_a),
            Some("spec"),
            None,
            "dispatch A's late failure",
            &[],
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Running,
        "a stale dispatch's failure must not flip status out from under the current dispatch"
    );
    let run_b = db.get_graph_run("run-b").unwrap().unwrap();
    assert_eq!(
        run_b.status,
        GraphRunStatus::Running,
        "a stale dispatch's fail_graph sweep must never touch a newer dispatch's run"
    );
}

/// The ordinary, single-dispatch case is unchanged: when
/// `dispatch_started_at` still matches the graph's current claim,
/// `fail_graph` flips status and sweeps exactly as before.
#[tokio::test]
async fn fail_graph_from_current_dispatch_still_flips_status_and_sweeps() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-a".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "node-a".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let claim = chrono::Utc::now();
    assert!(db.claim_graph_for_run(&graph_id, claim).unwrap());
    db.insert_graph_run(&GraphNodeRun {
        id: "run-a".to_string(),
        graph_id: graph_id.clone(),
        spec_id,
        node_id: "node-a".to_string(),
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

    engine
        .fail_graph(
            &graph_id,
            Some(claim),
            Some("spec"),
            None,
            "genuine failure",
            &[],
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let run = db.get_graph_run("run-a").unwrap().unwrap();
    assert_eq!(run.status, GraphRunStatus::Fail);
}

/// A second launch against a graph that already has an in-flight run must be
/// a silent no-op — the atomic graph claim refuses it, so it can't start a
/// duplicate dispatch that would supersede the live run at the next node.
#[tokio::test]
async fn duplicate_launch_of_running_graph_is_a_noop() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf OK",
            "success_condition": "exit_code_0",
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Simulate a live dispatch: the graph is already `Running` with an
    // in-flight node run behind it.
    db.update_graph_status(
        &graph_id,
        GraphStatus::Running,
        Some(chrono::Utc::now()),
        None,
    )
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "inflight".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec_id.clone(),
        node_id: "implement".to_string(),
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

    // A second dispatch (autorun/resume racing the live one) must no-op.
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    // The graph is untouched and the in-flight run was neither superseded
    // nor duplicated.
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Running);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(
        runs.len(),
        1,
        "the duplicate launch must not create a second run"
    );
    assert_eq!(runs[0].status, GraphRunStatus::Running);
}
