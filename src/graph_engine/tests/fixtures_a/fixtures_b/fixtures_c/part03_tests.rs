//! Part 03 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

#[tokio::test]
async fn graph_engine_spec_committed_head_scoped_per_spec_not_shared() {
    // Test 4 (spec TESTS REQUIRED): the marker's value is scoped to the
    // run — two specs in one graph do not share it. A graph with two bound
    // specs runs both, in position order, within one `run_graph` call.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.insert_graph_node(&rights_node(
        &spec_id,
        "committer",
        COMMIT_CMD,
        Some(true),
        1,
    ))
    .unwrap();

    // A second spec in the same graph, with its own committer that also
    // commits.
    let spec_b = crate::domain::graphs::GraphSpec {
        id: "spec-b".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "Spec B".to_string(),
        description: Some("second spec".to_string()),
        position: 2,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_committed_head: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    };
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&rights_node(
        &spec_b.id,
        "committer-b",
        COMMIT_CMD,
        Some(true),
        1,
    ))
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec_a_after = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let spec_b_after = db.get_graph_spec(&spec_b.id).unwrap().unwrap();
    assert_eq!(spec_a_after.status, GraphSpecStatus::Completed);
    assert_eq!(spec_b_after.status, GraphSpecStatus::Completed);
    assert!(
        spec_a_after.spec_committed_head.is_some(),
        "spec A must have recorded its own commit"
    );
    assert!(
        spec_b_after.spec_committed_head.is_some(),
        "spec B must have recorded its own commit"
    );
    assert_ne!(
        spec_b_after.spec_committed_head, spec_a_after.spec_committed_head,
        "each spec must record its own commit, not share the other's"
    );
}

#[tokio::test]
async fn graph_engine_restart_mid_run_does_not_turn_committed_check_fail_into_pass() {
    // Test 5 (spec TESTS REQUIRED): if a restart path is reachable in a
    // test, a restart mid-run does not turn a fail into a pass. Mirrors
    // B10's `graph_engine_resume_dispatch_reuses_persisted_spec_start_head_even_if_stale`:
    // a spec left `running` (as a daemon restart would leave it) with no
    // `spec_committed_head` recorded yet must resume still lacking one —
    // a resume must never manufacture evidence of a commit that never
    // happened.
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let baseline = git_head(dir.path());

    db.update_graph_spec_status(&spec_id, GraphSpecStatus::Running, None, None)
        .unwrap();
    db.set_graph_spec_start_head(&spec_id, Some(&baseline))
        .unwrap();
    // Deliberately left unset, as an interrupted attempt that hadn't
    // committed yet would leave it.

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": COMMITTED_CHECK_CMD,
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

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

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.spec_committed_head, None);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let check_run = runs.iter().find(|r| r.node_id == "node-check").unwrap();
    assert_eq!(
        check_run.status,
        GraphRunStatus::Fail,
        "resuming an attempt that never committed must never read as a pass"
    );
}

#[tokio::test]
async fn graph_engine_completes_check_and_gate_spec() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let check = GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let gate = GraphNode {
        id: "node-gate".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "gate".to_string(),
        kind: GraphNodeKind::Gate,
        config: serde_json::json!({
            "evaluate": "output_contains",
            "value": "APPROVED",
            "pass_route": "next_spec"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    };

    db.insert_graph_node(&check).unwrap();
    db.insert_graph_node(&gate).unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: check.id.clone(),
        to_node: gate.id.clone(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();

    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(runs.len(), 2);
}

#[tokio::test]
async fn graph_engine_fails_spec_when_check_fails_without_route() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
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

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();

    assert_eq!(lp.status, GraphStatus::Failed);
    assert_eq!(spec.status, GraphSpecStatus::Failed);
}

#[tokio::test]
async fn activity_publishes_expected_sequence_for_passing_two_node_spec() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-check".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "printf APPROVED", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-gate".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "gate".to_string(),
            kind: GraphNodeKind::Gate,
            config: serde_json::json!({"evaluate": "output_contains", "value": "APPROVED", "pass_route": "next_spec"}),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check".to_string(),
        to_node: "node-gate".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let workdir = dir.path().to_string_lossy().to_string();
    let entries = db.list_activity_log_entries(&workdir, 50).unwrap();
    let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();

    // Exact sequence: no per-node entries at all, because both Check and Gate
    // passed (FR1: "a check/gate node only when it fails").
    assert_eq!(
        messages,
        vec![
            "Started (1 spec(s) pending).",
            "Spec 'Spec' started.",
            "Spec 'Spec' completed.",
            "Completed (1/1 specs).",
        ],
        "got: {messages:?}"
    );
    for entry in &entries {
        assert_eq!(entry.source, "graph:Graph");
        assert_eq!(entry.source_id.as_deref(), Some(graph_id.as_str()));
    }
}

/// SPECCODE: the main started/completed path carries the spec's code
/// before its name. Fails if `run_spec` or `spec_step` go back to the
/// bare name.
#[tokio::test]
async fn activity_spec_started_completed_carry_code_before_name() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.set_graph_spec_type(
        &spec_id,
        crate::domain::graphs::SpecType::Fix,
        None,
        Some("FIX-7"),
    )
    .unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-check".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "printf APPROVED", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-gate".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "gate".to_string(),
            kind: GraphNodeKind::Gate,
            config: serde_json::json!({"evaluate": "output_contains", "value": "APPROVED", "pass_route": "next_spec"}),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check".to_string(),
        to_node: "node-gate".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let workdir = dir.path().to_string_lossy().to_string();
    let entries = db.list_activity_log_entries(&workdir, 50).unwrap();
    let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        messages,
        vec![
            "Started (1 spec(s) pending).",
            "Spec [FIX-7] 'Spec' started.",
            "Spec [FIX-7] 'Spec' completed.",
            "Completed (1/1 specs).",
        ],
        "got: {messages:?}"
    );
}

/// SPECCODE: the spec-failed path carries the code looked up from the
/// store. Fails if `node_dispatch` goes back to the bare name.
#[tokio::test]
async fn activity_spec_failed_carries_code_before_name() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.set_graph_spec_type(
        &spec_id,
        crate::domain::graphs::SpecType::Fix,
        None,
        Some("FIX-7"),
    )
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "exit 3", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let workdir = dir.path().to_string_lossy().to_string();
    let entries = db.list_activity_log_entries(&workdir, 50).unwrap();
    let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Spec [FIX-7] 'Spec' failed:")),
        "expected a coded spec-failed entry, got: {messages:?}"
    );
}

#[tokio::test]
async fn activity_publishes_check_failure_with_exit_code() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "exit 3", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // No Fail edge: mirrors `graph_engine_fails_spec_when_check_fails_without_route`.

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let workdir = dir.path().to_string_lossy().to_string();
    let entries = db.list_activity_log_entries(&workdir, 50).unwrap();
    let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();

    assert!(
        messages
            .iter()
            .any(|m| m.contains("check") && m.contains("exit 3")),
        "expected a check-failure entry with exit code 3, got: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Spec 'Spec' failed:")),
        "expected a spec-failed entry, got: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.starts_with("Failed:")),
        "expected a graph-failed entry, got: {messages:?}"
    );
}

#[tokio::test]
async fn activity_publishes_hook_failure_with_exit_code() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-check".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "printf APPROVED", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("exit 2".to_string()),
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
    hooks.insert(GraphHookEvent::OnCompleted, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let workdir = dir.path().to_string_lossy().to_string();
    let entries = db.list_activity_log_entries(&workdir, 50).unwrap();
    let messages: Vec<&str> = entries.iter().map(|e| e.message.as_str()).collect();
    assert!(
        messages
            .iter()
            .any(|m| m.contains("on_completed") && m.contains("command") && m.contains("exit 2")),
        "expected a hook-failure entry naming the event, kind, and exit code, got: {messages:?}"
    );
}

/// CM46 req 7: a two-spec graph whose first node fails produces the
/// expected durable event sequence, in order — and the activity stream
/// for the same run is unchanged (CM25 regression, the neighbouring
/// `activity_publishes_*` tests are the byte-exact net for that).
#[tokio::test]
async fn graph_events_records_ordered_history_for_failing_spec() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    // Spec 1: a check node that fails, with no fail edge.
    db.insert_graph_node(&GraphNode {
        id: "node-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "exit 1", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // Spec 2: never reached, because spec 1 fails first.
    db.insert_graph_spec(&GraphSpec {
        id: "spec-two".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "Spec Two".to_string(),
        description: None,
        position: 2,
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
    // An `on_failed` command hook that itself fails, so the recorded
    // `hook_fired` payload carries `ok: false`.
    let hook = GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("exit 1".to_string()),
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
    hooks.insert(GraphHookEvent::OnFailed, vec![hook]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    let kinds: Vec<&str> = events.iter().map(|event| event.kind.as_str()).collect();
    let pos = |kind: &str| {
        kinds
            .iter()
            .position(|candidate| *candidate == kind)
            .unwrap_or_else(|| panic!("expected a '{kind}' event in {kinds:?}"))
    };
    assert!(pos("graph_started") < pos("spec_started"), "{kinds:?}");
    assert!(pos("spec_started") < pos("node_started"), "{kinds:?}");
    assert!(pos("node_started") < pos("node_finished"), "{kinds:?}");
    assert!(pos("node_finished") < pos("spec_failed"), "{kinds:?}");
    assert!(pos("spec_failed") < pos("hook_fired"), "{kinds:?}");

    let hook_event = events
        .iter()
        .find(|event| event.kind == "hook_fired")
        .expect("hook_fired event");
    let payload: serde_json::Value =
        serde_json::from_str(hook_event.payload.as_deref().unwrap()).unwrap();
    assert_eq!(payload["event"], "on_failed");
    assert_eq!(payload["ok"], false);
    assert_eq!(payload["mode"], "command");

    // CM25 regression: the same human-readable lines reached the activity
    // stream, unchanged.
    let workdir = dir.path().to_string_lossy().to_string();
    let messages: Vec<String> = db
        .list_activity_log_entries(&workdir, 50)
        .unwrap()
        .into_iter()
        .map(|entry| entry.message)
        .collect();
    assert!(
        messages.iter().any(|m| m == "Started (2 spec(s) pending)."),
        "got: {messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Spec 'Spec' failed:")),
        "got: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m.starts_with("Failed:")),
        "got: {messages:?}"
    );
}

#[test]
fn resolve_spec_start_retries_last_running_node() {
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: None,
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
    let details = crate::domain::graphs::GraphSpecDetails {
        spec: spec.clone(),
        nodes: vec![GraphNode {
            id: "node-1".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Node".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        }],
        edges: vec![],
    };
    let runs = vec![GraphNodeRun {
        id: "run".to_string(),
        graph_id: "wf".to_string(),
        spec_id: spec.id.clone(),
        node_id: "node-1".to_string(),
        status: GraphRunStatus::Fail,
        input: Some(serde_json::json!({"previous": "context"})),
        // Deliberately distinct from `input`: a resumed node must be fed
        // the value propagated into it, not its own verdict.
        output: Some(serde_json::json!({"verdict": "fail"})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }];

    // CB50 FR4: a `Running` spec resumes regardless of `is_resume` —
    // historic behaviour, unchanged. Pass `false` to prove it.
    let (cursor, node_outputs, resume_previous_output, iterations) = resolve_spec_start(
        &details.nodes,
        &details.edges,
        &spec,
        &runs,
        &[],
        false,
        None,
    )
    .unwrap();

    assert_eq!(cursor, SpecCursor::Node("node-1".to_string()));
    assert_eq!(iterations.get("node-1"), Some(&1));
    assert_eq!(
        resume_previous_output
            .as_ref()
            .and_then(|value| value.get("previous").cloned()),
        Some(serde_json::json!("context")),
        "resumed node must see its interrupted attempt's input, not its output"
    );
    // The completed run's output is still retained by name so
    // `{{output:Node}}` multi-hop references keep resolving after a resume.
    assert_eq!(
        node_outputs
            .get("Node")
            .and_then(|value| value.get("verdict").cloned()),
        Some(serde_json::json!("fail"))
    );
}

/// CB50 FR2: an `Interrupted` spec resumes at the node its last run was
/// on — never restarted from the entry node the way `loop_run` restarts
/// it. Two-node graph so entry ("node-1") and cursor ("node-2") are
/// provably different ids.
#[test]
fn resolve_spec_start_resumes_interrupted_spec_at_cursor() {
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Interrupted,
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
    let nodes = vec![
        GraphNode {
            id: "node-1".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Entry".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        },
        GraphNode {
            id: "node-2".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Second".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 2,
            created_at: chrono::Utc::now(),
        },
    ];
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        from_node: "node-1".to_string(),
        to_node: "node-2".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    }];
    // The interrupted attempt had already passed node-1 and was cut
    // short mid node-2 (the daemon-restart artifact: a run with no
    // terminal status).
    let runs = vec![
        GraphNodeRun {
            id: "run-1".to_string(),
            graph_id: "wf".to_string(),
            spec_id: spec.id.clone(),
            node_id: "node-1".to_string(),
            status: GraphRunStatus::Pass,
            input: None,
            output: Some(serde_json::json!({"ok": true})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        },
        GraphNodeRun {
            id: "run-2".to_string(),
            graph_id: "wf".to_string(),
            spec_id: spec.id.clone(),
            node_id: "node-2".to_string(),
            status: GraphRunStatus::Fail,
            input: Some(serde_json::json!({"from": "node-1"})),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        },
    ];

    // CB50 FR2: `loop_continue` (`is_resume: true`) must resume at the cursor.
    let (cursor, _node_outputs, _resume_previous_output, _iterations) =
        resolve_spec_start(&nodes, &edges, &spec, &runs, &[], true, None).unwrap();

    assert_eq!(
        cursor,
        SpecCursor::Node("node-2".to_string()),
        "an interrupted spec must resume where its last run stopped, not at the entry node"
    );
}

/// CB50 FR4: `loop_run` (`is_resume: false`) on an `Interrupted` spec
/// must restart from the entry node, not resume at the cursor — the two
/// verbs stay distinct by design.
#[test]
fn resolve_spec_start_restarts_interrupted_spec_on_loop_run() {
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Interrupted,
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
    let nodes = vec![
        GraphNode {
            id: "node-1".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Entry".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        },
        GraphNode {
            id: "node-2".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Second".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 2,
            created_at: chrono::Utc::now(),
        },
    ];
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        from_node: "node-1".to_string(),
        to_node: "node-2".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    }];
    let runs = vec![
        GraphNodeRun {
            id: "run-1".to_string(),
            graph_id: "wf".to_string(),
            spec_id: spec.id.clone(),
            node_id: "node-1".to_string(),
            status: GraphRunStatus::Pass,
            input: None,
            output: Some(serde_json::json!({"ok": true})),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        },
        GraphNodeRun {
            id: "run-2".to_string(),
            graph_id: "wf".to_string(),
            spec_id: spec.id.clone(),
            node_id: "node-2".to_string(),
            status: GraphRunStatus::Fail,
            input: Some(serde_json::json!({"from": "node-1"})),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        },
    ];

    let (cursor, _node_outputs, resume_previous_output, iterations) =
        resolve_spec_start(&nodes, &edges, &spec, &runs, &[], false, None).unwrap();

    assert_eq!(
        cursor,
        SpecCursor::Node("node-1".to_string()),
        "loop_run on an interrupted spec must restart from the entry node, not the cursor"
    );
    assert!(resume_previous_output.is_none());
    assert!(iterations.is_empty());
}

#[test]
fn resolve_spec_start_resets_iterations_for_fresh_spec() {
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: None,
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
    let details = crate::domain::graphs::GraphSpecDetails {
        spec: spec.clone(),
        nodes: vec![GraphNode {
            id: "node-1".to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Node".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: chrono::Utc::now(),
        }],
        edges: vec![],
    };
    // Historical runs from a previous attempt at this spec: 10 failed
    // iterations that exhausted the budget last time around.
    let runs: Vec<GraphNodeRun> = (0..10)
        .map(|i| GraphNodeRun {
            id: format!("run-{i}"),
            graph_id: "wf".to_string(),
            spec_id: spec.id.clone(),
            node_id: "node-1".to_string(),
            status: GraphRunStatus::Fail,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: i + 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .collect();

    let (cursor, node_outputs, _resume_previous_output, iterations) = resolve_spec_start(
        &details.nodes,
        &details.edges,
        &spec,
        &runs,
        &[],
        false,
        None,
    )
    .unwrap();

    assert_eq!(cursor, SpecCursor::Node("node-1".to_string()));
    assert!(node_outputs.is_empty());
    assert!(iterations.is_empty());
}

#[test]
fn find_entry_node_picks_lowest_position_in_retry_cycle() {
    // implement (pos 1) <-> review (pos 2): every node has an incoming
    // edge, so there is no source node. The entry must be the designated
    // start (lowest position), not an error.
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: None,
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
    let node = |id: &str, position: i64| GraphNode {
        id: id.to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position,
        created_at: chrono::Utc::now(),
    };
    let edge = |id: &str, from: &str, to: &str, condition| GraphEdge {
        id: id.to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    };
    let details = crate::domain::graphs::GraphSpecDetails {
        spec: spec.clone(),
        // Insert review before implement so the result cannot depend on
        // node ordering — only on position.
        nodes: vec![node("review", 2), node("implement", 1)],
        edges: vec![
            edge(
                "e1",
                "implement",
                "review",
                crate::domain::graphs::GraphEdgeCondition::Always,
            ),
            edge(
                "e2",
                "review",
                "implement",
                crate::domain::graphs::GraphEdgeCondition::Fail,
            ),
        ],
    };

    assert_eq!(
        find_entry_node(&details.nodes, &details.edges, &details.spec.name).unwrap(),
        "implement"
    );
}

#[test]
fn render_agent_prompt_includes_reporting_contract() {
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
        "{{spec_content}}",
        Some(&serde_json::json!({"feedback":"ok"})),
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();

    assert!(prompt.contains("graph_complete_node"));
    assert!(prompt.contains("graph_report_blocker"));
    assert!(!prompt.contains("run_id=\""));
    assert!(prompt.contains("CANOPY_NODE_RUN_ID"));
    assert!(prompt.contains("Do the thing"));
    assert!(prompt.contains("\"feedback\": \"ok\""));
}

/// CM47: `[SPEC NOTES]` is placed before `[REPORTING]` in the agent prompt
/// (req 5). A non-empty notes section must precede the reporting contract.
#[test]
fn render_agent_prompt_places_spec_notes_before_reporting() {
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
        "{{spec_content}}",
        Some(&serde_json::json!({"feedback":"ok"})),
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "# [SPEC NOTES]\n\n## Designer\nthe plan\nwrite your note.\n",
    )
    .unwrap();

    let notes_at = prompt
        .find("[SPEC NOTES]")
        .expect("SPEC NOTES section must be present");
    let reporting_at = prompt
        .find("[REPORTING]")
        .expect("REPORTING section must be present");
    assert!(notes_at < reporting_at, "SPEC NOTES must precede REPORTING");
    assert!(prompt.contains("## Designer\nthe plan"));
}

/// A spec picked up `Interrupted` gets an explicit continuation notice
/// in its cold-start prompt — the agent has no memory of the previous
/// attempt, so the prompt is the only thing that can tell it partial
/// work exists in the workdir and should be continued, not redone. A
/// `Pending` spec (any other status reaching a cold start) gets no such
/// notice — nothing was interrupted, there is nothing to continue.
#[test]
fn render_agent_prompt_adds_continuation_notice_only_when_spec_interrupted() {
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
    let mut spec = GraphSpec {
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

    let pending_prompt = render_agent_prompt(
        &lp,
        &spec,
        &node,
        "{{spec_content}}",
        None,
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();
    assert!(!pending_prompt.contains("[CONTINUATION]"));

    spec.status = GraphSpecStatus::Interrupted;
    let interrupted_prompt = render_agent_prompt(
        &lp,
        &spec,
        &node,
        "{{spec_content}}",
        None,
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();
    assert!(interrupted_prompt.contains("[CONTINUATION]"));
    assert!(interrupted_prompt.contains("interrupted"));
    assert!(interrupted_prompt.contains("git status"));
    assert!(interrupted_prompt.contains("git diff"));
    assert!(interrupted_prompt.contains(&lp.workdir));
}

#[test]
fn resolve_node_prompt_template_prefers_explicit_prompt_template_over_preset() {
    let dir = tempfile::tempdir().unwrap();
    // The prompts dir doesn't even exist — an explicit prompt_template
    // must win without ever touching disk.
    let prompts_dir = dir.path().join("prompts");

    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "prompt_template": "explicit template",
        "prompt_preset": "implementer"
    }));

    assert_eq!(
        resolve_node_prompt_template(&node, &prompts_dir),
        "explicit template"
    );
}

#[test]
fn resolve_node_prompt_template_reads_preset_file_over_hardcoded_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let prompts_dir = dir.path().join("prompts");
    std::fs::create_dir_all(&prompts_dir).unwrap();
    std::fs::write(
        prompts_dir.join("implementer.md"),
        "edited implementer preset",
    )
    .unwrap();

    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "prompt_preset": "implementer"
    }));

    assert_eq!(
        resolve_node_prompt_template(&node, &prompts_dir),
        "edited implementer preset"
    );
}

#[test]
fn resolve_node_prompt_template_falls_back_to_hardcoded_constant_when_preset_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    let prompts_dir = dir.path().join("prompts"); // never created

    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "prompt_preset": "reviewer"
    }));

    let expected = crate::domain::prompts::builtin_prompt_preset_specs()
        .into_iter()
        .find(|(name, _)| *name == "reviewer")
        .map(|(_, content)| content)
        .unwrap();

    assert_eq!(resolve_node_prompt_template(&node, &prompts_dir), expected);
}

#[test]
fn resolve_node_prompt_template_defaults_when_neither_prompt_nor_preset_set() {
    let dir = tempfile::tempdir().unwrap();
    let prompts_dir = dir.path().join("prompts");

    let node = agent_node_with_config(serde_json::json!({ "platform": "claude" }));

    assert_eq!(
        resolve_node_prompt_template(&node, &prompts_dir),
        "{{spec_content}}\n\n{{previous_feedback}}"
    );
}

#[test]
fn bound_previous_feedback_leaves_small_text_unchanged() {
    let text = "small feedback".to_string();
    assert_eq!(bound_previous_feedback(text.clone()), text);
}

#[test]
fn bound_previous_feedback_elides_marker_only_above_threshold() {
    let at_threshold = "a".repeat(PREVIOUS_FEEDBACK_ELISION_THRESHOLD);
    assert!(!bound_previous_feedback(at_threshold).contains("bytes elided"));

    let over_threshold = "a".repeat(PREVIOUS_FEEDBACK_ELISION_THRESHOLD + 1);
    let bounded = bound_previous_feedback(over_threshold);
    assert!(bounded.contains("bytes elided"));
    assert!(bounded.len() < PREVIOUS_FEEDBACK_ELISION_THRESHOLD + 200);
}

#[test]
fn render_agent_prompt_elides_huge_previous_feedback() {
    // A prior node (e.g. a `cargo test` check) can emit a full log many
    // times over the elision threshold — the real incident this fixes
    // was a 65KB test log blowing up argv. The full text must never be
    // interpolated whole; the marker must show it was cut.
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
    let huge_log = "x".repeat(500 * 1024);

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

    assert!(prompt.contains("bytes elided"));
    assert!(prompt.len() < 600 * 1024);
}
