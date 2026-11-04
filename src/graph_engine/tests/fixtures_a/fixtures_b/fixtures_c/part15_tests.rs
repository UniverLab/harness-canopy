//! Part 15 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// Guideline 5: a node already on its last allowed iteration survives a
/// pause and continue and still gets that attempt — it is not pushed over
/// `graphs.max_iterations_per_node` by the operator's pause.
#[tokio::test]
async fn node_at_last_iteration_survives_pause_and_continue() {
    let (_dir, db, engine, graph_id, spec_id) = cb31_fixture("sleep 2 && true");
    db.update_graph_spec_status(
        &spec_id,
        GraphSpecStatus::Running,
        Some(chrono::Utc::now()),
        None,
    )
    .unwrap();
    // Four genuine prior attempts: the node's next attempt is iteration 5,
    // the last one `graphs.max_iterations_per_node` allows.
    for i in 1..=4 {
        db.insert_graph_run(&GraphNodeRun {
            id: format!("cb31-seed-{i}"),
            graph_id: graph_id.clone(),
            spec_id: spec_id.clone(),
            node_id: "cb31-node".to_string(),
            status: GraphRunStatus::Fail,
            input: None,
            output: Some(serde_json::json!({ "seed": i })),
            started_at: chrono::Utc::now() - chrono::Duration::seconds(20 - i as i64),
            completed_at: Some(chrono::Utc::now() - chrono::Duration::seconds(19 - i as i64)),
            iteration: i as i64,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    let disp = {
        let (e, id) = (Arc::clone(&engine), graph_id.clone());
        tokio::spawn(async move {
            e.run_graph_dispatch(id, None, None, DispatchMode::resume(), None, None)
                .await
        })
    };
    cb31_wait_for_running_run(&db, &graph_id).await;
    assert!(engine.request_pause(&graph_id, false).unwrap());
    disp.await.unwrap().unwrap();

    let paused_run = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .max_by_key(|r| r.started_at)
        .unwrap();
    assert_eq!(
        paused_run.iteration, 5,
        "the paused attempt was iteration 5"
    );

    // Continue: the node must get to run iteration 5 again, not be blocked
    // by a spurious iteration 6.
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

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        runs.iter().all(|r| r.iteration <= 5),
        "continue must not push the node to iteration 6; saw {:?}",
        runs.iter().map(|r| r.iteration).collect::<Vec<_>>()
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "the spec completes on the surviving last attempt, not blocked on an unearned ceiling"
    );
}

/// Req 6: pausing a graph with nothing in flight is immediate.
#[tokio::test]
async fn pause_with_no_node_in_flight_is_immediate() {
    let (_dir, db, engine, graph_id, _spec_id) = cb31_fixture("true");
    db.claim_graph_for_run(&graph_id, chrono::Utc::now())
        .unwrap();
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Running
    );

    assert!(engine.request_pause(&graph_id, false).unwrap());

    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Paused,
        "with no node running there is nothing to wait for — the pause is immediate"
    );
}

// ── CB5: check node failure captures output and truncates ──────────

#[tokio::test]
async fn check_node_failure_captures_stdout_stderr() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("recovered.marker");

    db.insert_graph_node(&GraphNode {
        id: "node-check-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "failing-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "echo 'FAIL_LINE_STDOUT'; echo 'FAIL_LINE_STDERR' >&2; exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-recovery".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "recovery".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!("touch \"{}\"", marker.display()),
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check-fail".to_string(),
        to_node: "node-recovery".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs
        .iter()
        .find(|r| r.node_id == "node-check-fail")
        .expect("check run must exist");
    assert_eq!(check_run.status, GraphRunStatus::Fail);
    let out = check_run
        .output
        .as_ref()
        .expect("check output must be persisted");
    assert_eq!(out["passed"], serde_json::Value::Bool(false));
    assert_eq!(out["exit_code"], serde_json::json!(1));
    assert!(
        out["stdout"].as_str().unwrap().contains("FAIL_LINE_STDOUT"),
        "stdout must contain FAIL_LINE_STDOUT, got: {:?}",
        out["stdout"]
    );
    assert!(
        out["stderr"].as_str().unwrap().contains("FAIL_LINE_STDERR"),
        "stderr must contain FAIL_LINE_STDERR, got: {:?}",
        out["stderr"]
    );
    // Verify persistence via graph equivalent
    let fetched = db.get_graph_run(&check_run.id).unwrap().unwrap();
    assert_eq!(fetched.output, check_run.output);

    assert!(
        marker.exists(),
        "fail edge must have been traversed to recovery node"
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
}

#[tokio::test]
async fn check_node_failure_output_reaches_next_node() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "failing-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "echo 'DISTINCT_FAIL_OUTPUT_42'; exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-next".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "next".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 0",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check-fail".to_string(),
        to_node: "node-next".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs
        .iter()
        .find(|r| r.node_id == "node-check-fail")
        .unwrap();
    let next_run = runs.iter().find(|r| r.node_id == "node-next").unwrap();

    let check_stdout = check_run
        .output
        .as_ref()
        .unwrap()
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap();
    assert!(
        check_stdout.contains("DISTINCT_FAIL_OUTPUT_42"),
        "check stdout must contain distinctive output"
    );

    let next_input = next_run
        .input
        .as_ref()
        .expect("next node input must be set");
    let input_str = serde_json::to_string(next_input).unwrap();
    assert!(
        input_str.contains("DISTINCT_FAIL_OUTPUT_42"),
        "next node input must contain previous check stdout, got: {input_str}"
    );
    assert_eq!(next_input["stdout"].as_str().unwrap(), check_stdout);
}

#[tokio::test]
async fn check_node_long_output_is_truncated() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-long".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "long-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "head -c 70000 /dev/zero | tr '\\0' 'A'; printf 'TAIL_END'; exit 1",
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

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.node_id == "node-check-long")
        .unwrap();
    assert_eq!(run.status, GraphRunStatus::Fail);
    let out = run.output.as_ref().unwrap();
    assert_eq!(out["truncated"], serde_json::Value::Bool(true));
    let stdout = out["stdout"].as_str().unwrap();
    assert!(
        stdout.contains("TAIL_END"),
        "truncated stdout must keep tail containing TAIL_END"
    );
    assert!(
        stdout.starts_with("[...truncated"),
        "truncated stdout must start with truncation marker, got: {}",
        &stdout[..80.min(stdout.len())]
    );
    // Stored stdout should be bounded: marker + 64KB
    assert!(
        stdout.len() <= 64 * 1024 + 100,
        "truncated stdout must be bounded, got len {}",
        stdout.len()
    );
}

/// A check node's `success_condition` is evaluated against the command's
/// *full* output, even when that output is large enough to be truncated
/// for storage. Regression guard for the CH2 refactor that routed check
/// nodes through `execute_shell_command`: if the condition were checked
/// against the stored (tail-only) `stdout` instead, this marker — emitted
/// first, then buried under >64KB — would be gone and the check would
/// wrongly fail.
#[tokio::test]
async fn check_node_success_condition_sees_full_output_not_just_truncated_tail() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-early-marker".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "early-marker-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf 'EARLY_MARKER_77 '; head -c 70000 /dev/zero | tr '\\0' 'A'",
            "success_condition": "exit_code_0_and_output_contains:EARLY_MARKER_77"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.node_id == "node-check-early-marker")
        .unwrap();
    assert_eq!(
            run.status,
            GraphRunStatus::Pass,
            "condition must match the marker in the full output even though it is truncated for storage"
        );
    let out = run.output.as_ref().unwrap();
    assert_eq!(out["truncated"], serde_json::Value::Bool(true));
    assert_eq!(out["passed"], serde_json::Value::Bool(true));
    // The stored stdout has lost the early marker to truncation — proof
    // the pass above came from evaluating the full output, not this.
    let stored_stdout = out["stdout"].as_str().unwrap();
    assert!(!stored_stdout.contains("EARLY_MARKER_77"));
}

// ── CT3: live tailing — streaming, timeout, zero-output, truncation ──

#[tokio::test]
async fn check_node_streams_stdout_and_stderr_to_live_tail() {
    // The live tail must hold output *while the node still runs*: poll
    // mid-run and require the early line before the run completes.
    // Without the reader tasks the chunk table stays empty until the
    // completion snapshot, so this fails with the feature removed.
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-stream".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "stream-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "echo 'STREAM_EARLY_OUT'; echo 'STREAM_EARLY_ERR' >&2; sleep 3; exit 0",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let run_graph = engine.run_graph(graph_id.clone(), None, None, None, None, None);
    tokio::pin!(run_graph);
    // Poll the future without awaiting it yet: drive one step so the
    // check node spawns, then observe the live tail from this thread.
    let mut saw_early = false;
    for _ in 0..100 {
        if tokio::time::timeout(std::time::Duration::from_millis(100), &mut run_graph)
            .await
            .is_ok()
        {
            break;
        }
        let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
        if let Some(run) = runs.iter().find(|r| r.node_id == "node-check-stream") {
            let (stdout, stderr) = db.get_graph_run_tail(&run.id, 1000).unwrap();
            if stdout.contains("STREAM_EARLY_OUT") && stderr.contains("STREAM_EARLY_ERR") {
                saw_early = true;
                break;
            }
        }
    }
    assert!(
        saw_early,
        "live tail must show streamed output while the node still runs"
    );
    run_graph.await.unwrap();

    // Post-completion the snapshot keeps serving the tail.
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.node_id == "node-check-stream")
        .unwrap();
    assert_eq!(run.status, GraphRunStatus::Pass);
    let (stdout, stderr) = db.get_graph_run_tail(&run.id, 1000).unwrap();
    assert!(stdout.contains("STREAM_EARLY_OUT"), "got: {stdout}");
    assert!(stderr.contains("STREAM_EARLY_ERR"), "got: {stderr}");
    let out = run.output.as_ref().unwrap();
    assert_eq!(out["stdout"].as_str().unwrap(), "STREAM_EARLY_OUT");
    assert_eq!(out["stderr"].as_str().unwrap(), "STREAM_EARLY_ERR");
}

#[tokio::test]
async fn check_node_timeout_marks_tail_timed_out_with_partial_output() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-hang".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "hang-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "echo 'PARTIAL_BEFORE_HANG'; sleep 30",
            "success_condition": "exit_code_0",
            "timeout_seconds": 1
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.node_id == "node-check-hang")
        .unwrap();
    assert_eq!(run.status, GraphRunStatus::Fail);
    let out = run.output.as_ref().unwrap();
    assert_eq!(out["error"], serde_json::json!("timed out"));
    // Partial output captured before the kill must be visible in the
    // timeout record — a silent timeout shows nothing.
    assert!(
        out["stdout"]
            .as_str()
            .unwrap()
            .contains("PARTIAL_BEFORE_HANG"),
        "timeout output must carry partial stdout, got: {:?}",
        out["stdout"]
    );
    let (stdout, _) = db.get_graph_run_tail(&run.id, 1000).unwrap();
    assert!(
        stdout.contains("PARTIAL_BEFORE_HANG"),
        "live tail must keep partial output after timeout, got: {stdout}"
    );
}

#[tokio::test]
async fn check_node_zero_output_hang_leaves_empty_tail() {
    // A node that produces nothing before the timeout (the resilience
    // silence case): the tail is empty AND the run is marked timed out,
    // so the dialog can render "no output" + timeout banner instead of
    // an ambiguous blank.
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-silent".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "silent-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 30",
            "success_condition": "exit_code_0",
            "timeout_seconds": 1
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.node_id == "node-check-silent")
        .unwrap();
    assert_eq!(run.status, GraphRunStatus::Fail);
    let (stdout, stderr) = db.get_graph_run_tail(&run.id, 1000).unwrap();
    assert_eq!(stdout, "");
    assert_eq!(stderr, "");
}

#[tokio::test]
async fn check_node_large_output_tail_snapshot_stays_bounded() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check-flood".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "flood-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "head -c 100000 /dev/zero | tr '\\0' 'B'; printf 'FLOOD_TAIL'; exit 0",
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

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs
        .iter()
        .find(|r| r.node_id == "node-check-flood")
        .unwrap();
    assert_eq!(run.status, GraphRunStatus::Pass);
    let (stdout, _) = db.get_graph_run_tail(&run.id, 1000).unwrap();
    assert!(
        stdout.contains("FLOOD_TAIL"),
        "tail must keep the newest bytes, got len {}",
        stdout.len()
    );
    // Snapshot path reuses the 64KB truncation: the served tail must
    // stay bounded even though 100KB streamed through the chunk table.
    assert!(
        stdout.len() <= 64 * 1024 + 100,
        "served tail must be bounded, got len {}",
        stdout.len()
    );
}

#[test]
fn render_agent_prompt_refuses_template_with_unbindable_placeholder() {
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
        description: Some("do the thing".to_string()),
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
    let template = "Do this: {{spec_content}} and also {{custom_var}}";

    let result = render_agent_prompt(
        &lp,
        &spec,
        &node,
        template,
        None,
        "/tmp/project",
        "run-1",
        &HashMap::new(),
        &[],
        "",
    );

    assert!(
        result.is_err(),
        "template with unbindable placeholder must be refused"
    );
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("custom_var"),
        "error must name the unbindable placeholder: {err_msg}"
    );
    assert!(
        err_msg.contains("\\{{...}}"),
        "refusal must name the escape form: {err_msg}"
    );
}

#[test]
fn render_agent_prompt_spawns_with_an_escaped_unknown_marker() {
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
        description: Some("do the thing".to_string()),
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
        name: "Final review (kilo)".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let template = "Fail when an unsubstituted template marker like \\{{something}} \
            appears in a touched file. Spec: {{spec_content}}";

    let prompt = render_agent_prompt(
        &lp,
        &spec,
        &node,
        template,
        None,
        "/tmp/project",
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();

    assert!(
        prompt.contains("{{something}}"),
        "rendered prompt must contain the literal marker: {prompt}"
    );
    assert!(
        !prompt.contains("\\{{something}}"),
        "escaping backslash must be removed: {prompt}"
    );
    assert!(
        prompt.contains("do the thing"),
        "real bindings must still substitute: {prompt}"
    );
}

#[test]
fn render_agent_prompt_escape_wins_over_a_real_binding() {
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
        description: Some("SPEC-BODY".to_string()),
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
        "Literal \\{{spec_content}} here.",
        None,
        "/tmp/project",
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();

    assert!(
        prompt.contains("{{spec_content}}"),
        "escaped binding must render as literal text: {prompt}"
    );
    assert!(
        !prompt.contains("SPEC-BODY here."),
        "escaped binding must not substitute the spec: {prompt}"
    );
}

#[test]
fn escaped_output_marker_stays_literal_while_unescaped_still_substitutes() {
    let mut node_outputs = HashMap::new();
    node_outputs.insert(
        "Architect".to_string(),
        serde_json::json!({"plan": "build-it"}),
    );
    let all_nodes = vec!["Architect".to_string(), "Implementer".to_string()];

    let escaped = substitute_named_outputs(
        "Literal \\{{output:Architect}} here.",
        &node_outputs,
        &all_nodes,
    );
    assert_eq!(escaped, "Literal {{output:Architect}} here.");

    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
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
        graph_id: Some(lp.id.clone()),
        name: "Spec".to_string(),
        description: Some("task".to_string()),
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
        id: "node-impl".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let prompt = render_agent_prompt(
        &lp,
        &spec,
        &node,
        "Follow this design: {{output:Architect}}",
        None,
        &lp.workdir,
        "run-1",
        &node_outputs,
        &all_nodes,
        "",
    )
    .unwrap();
    assert!(
        prompt.contains("\"plan\": \"build-it\""),
        "unescaped output marker must still substitute: {prompt}"
    );
}

#[test]
fn named_output_substitution_replaces_marker_with_node_output() {
    let mut node_outputs = HashMap::new();
    node_outputs.insert(
        "Architect".to_string(),
        serde_json::json!({"design": "foo"}),
    );
    let all_nodes = vec!["Architect".to_string(), "Implementer".to_string()];

    let template = "Build {{output:Architect}}";
    let result = substitute_named_outputs(template, &node_outputs, &all_nodes);

    assert!(result.contains("\"design\": \"foo\""));
    assert!(!result.contains("{{output:Architect}}"));
}

#[test]
fn named_output_substitution_marks_unexecuted_node() {
    let node_outputs = HashMap::new();
    let all_nodes = vec!["Architect".to_string(), "Implementer".to_string()];

    let template = "Build {{output:Architect}}";
    let result = substitute_named_outputs(template, &node_outputs, &all_nodes);

    assert!(
        result.contains("(not yet executed)"),
        "expected '(not yet executed)' in: {result}"
    );
}

#[test]
fn named_output_substitution_preserves_multiple_markers() {
    let mut node_outputs = HashMap::new();
    node_outputs.insert(
        "Architect".to_string(),
        serde_json::json!({"design": "plan-a"}),
    );
    node_outputs.insert(
        "Reviewer".to_string(),
        serde_json::json!({"feedback": "approve"}),
    );
    let all_nodes = vec![
        "Architect".to_string(),
        "Reviewer".to_string(),
        "Implementer".to_string(),
    ];

    let template = "Design: {{output:Architect}} Review: {{output:Reviewer}}";
    let result = substitute_named_outputs(template, &node_outputs, &all_nodes);

    assert!(result.contains("\"design\": \"plan-a\""));
    assert!(result.contains("\"feedback\": \"approve\""));
}

#[test]
fn render_agent_prompt_substitutes_named_output_marker() {
    let dir = tempdir().unwrap();
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
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
        graph_id: Some(lp.id.clone()),
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
        id: "node-impl".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };

    let mut node_outputs = HashMap::new();
    node_outputs.insert(
        "Architect".to_string(),
        serde_json::json!({"plan": "build-it"}),
    );
    let all_nodes = vec!["Architect".to_string(), "Implementer".to_string()];

    let prompt = render_agent_prompt(
        &lp,
        &spec,
        &node,
        "Follow this design: {{output:Architect}}",
        None,
        &lp.workdir,
        "run-1",
        &node_outputs,
        &all_nodes,
        "",
    )
    .unwrap();

    assert!(
        prompt.contains("\"plan\": \"build-it\""),
        "prompt must contain architect's output: {prompt}"
    );
    assert!(
        !prompt.contains("{{output:Architect}}"),
        "prompt must not contain unsubstituted marker: {prompt}"
    );
}

#[test]
fn render_agent_prompt_accepts_output_marker_without_refusing() {
    let dir = tempdir().unwrap();
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
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
        graph_id: Some(lp.id.clone()),
        name: "Spec".to_string(),
        description: Some("task".to_string()),
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
        id: "node-impl".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let all_nodes = vec!["Architect".to_string(), "Implementer".to_string()];

    let result = render_agent_prompt(
        &lp,
        &spec,
        &node,
        "See {{output:Architect}}",
        None,
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &all_nodes,
        "",
    );

    assert!(
        result.is_ok(),
        "output:NodeName marker must be accepted: {:?}",
        result.err()
    );
}

#[test]
fn previous_feedback_still_works_without_named_outputs() {
    let dir = tempdir().unwrap();
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-test".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
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
        graph_id: Some(lp.id.clone()),
        name: "Spec".to_string(),
        description: Some("task".to_string()),
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
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Worker".to_string(),
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
        Some(&serde_json::json!({"status": "ok"})),
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();

    assert!(
        prompt.contains("\"status\": \"ok\""),
        "previous_feedback must still be substituted: {prompt}"
    );
}

#[tokio::test]
async fn execute_spec_retains_outputs_across_multiple_hops() {
    let fake_home = setup_prompt_capturing_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let capture_dir = dir.path().join("prompts");
    std::fs::create_dir_all(&capture_dir).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-architect".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "Architect".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "capture-cli",
            "prompt_template": "Design the system architecture",
            "timeout_minutes": 1,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-tester".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "Tester".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "capture-cli",
            "prompt_template": "Review the design for testability",
            "timeout_minutes": 1,
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-implementer".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "Implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "capture-cli",
            "prompt_template": "Implement based on this design: {{output:Architect}}",
            "timeout_minutes": 1,
        }),
        position: 3,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "edge-arch-test".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-architect".to_string(),
        to_node: "node-tester".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "edge-test-impl".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-tester".to_string(),
        to_node: "node-implementer".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Always,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let db_path = dir.path().join(TEST_DB_FILENAME);
    std::env::set_var("CAPTURE_DIR", capture_dir.to_str().unwrap());
    std::env::set_var("LINGER_SECONDS", "1");
    // CB57: the capture script never calls graph_complete_node, so it
    // self-reports its own Pass verdict (carrying each node's canned
    // stdout) as the last thing it does before exiting — see
    // `self_report_invocation`'s doc comment for why that can't race the
    // engine the way the old background-poller could.
    std::env::set_var("SELF_REPORT_NODE_1", "node-architect");
    std::env::set_var("SELF_REPORT_NODE_2", "node-tester");
    std::env::set_var("SELF_REPORT_NODE_3", "node-implementer");
    std::env::set_var("SELF_REPORT_EXE", self_report_exe());
    std::env::set_var("SELF_REPORT_DB", db_path.to_string_lossy().into_owned());

    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    std::env::remove_var("CAPTURE_DIR");
    std::env::remove_var("LINGER_SECONDS");
    std::env::remove_var("SELF_REPORT_NODE_1");
    std::env::remove_var("SELF_REPORT_NODE_2");
    std::env::remove_var("SELF_REPORT_NODE_3");
    std::env::remove_var("SELF_REPORT_EXE");
    std::env::remove_var("SELF_REPORT_DB");

    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let implementer_prompt_path = capture_dir.join("prompt_3");
    assert!(
        implementer_prompt_path.exists(),
        "Implementer's prompt must have been captured (expected prompt_3)"
    );
    let implementer_prompt = std::fs::read_to_string(&implementer_prompt_path).unwrap();

    assert!(
        implementer_prompt.contains("NODE_OUTPUT_DATA_1"),
        "Implementer's prompt must contain Architect's output (multi-hop retention). \
             Prompt was: {}",
        implementer_prompt
    );
    assert!(
        !implementer_prompt.contains("{{output:Architect}}"),
        "Implementer's prompt must not contain unsubstituted marker"
    );
}
