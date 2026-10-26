//! Part 14 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// B19: an agent that crashes once (fast, no self-report) and succeeds on
/// the in-place retry completes the spec without traversing any edge, and
/// the run history shows both attempts with infra markers.
#[tokio::test]
async fn infra_crash_then_success_retries_same_node_in_place() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // A fake CLI binary that ignores the rendered prompt entirely: it
    // fails fast on the first invocation and succeeds on the second
    // (marker file tracks invocations). Registered as its own platform
    // in a fixture canopy config, since node prompts are wrapped in a
    // [GRAPH CONTEXT] preamble that a plain `sh -c` cannot execute.
    let marker = dir.path().join("infra-marker");
    let script = dir.path().join("flaky-cli");
    std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ -f \"{m}\" ]; then printf ok; exit 0; else touch \"{m}\"; exit 1; fi\n",
                m = marker.to_string_lossy()
            ),
        )
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "flaky-cli".to_string(),
            binary: script.to_string_lossy().to_string(),
            headless_mode: "-c".to_string(),
            model_flag: None,
            supports_working_dir: false,
            working_dir_flag: None,
            env_vars: std::collections::HashMap::new(),
            interactive_args: None,
            fallback_interactive_args: None,
            resume_args: None,
            session_list_cmd: None,
            session_resume_cmd: None,
            accent_color: None,
            yolo_flag: None,
            prompt_via_stdin: false,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-flaky".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "flaky".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "flaky-cli",
            "prompt_template": "ignored",
            "infra_retry_limit": 2,
            "infra_crash_max_seconds": 60,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    // CM13: the second run (exit 0, stdout "ok") never self-reported,
    // so it is also infra. Retries exhaust and the spec fails.
    assert_eq!(
        spec.status,
        GraphSpecStatus::Failed,
        "CM13: all runs are unreported infra; retries exhaust and spec fails"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let flaky_runs: Vec<_> = runs.iter().filter(|r| r.node_id == "node-flaky").collect();
    assert_eq!(
        flaky_runs.len(),
        3,
        "CM13: initial + 2 retries = 3 run rows (all unreported infra)"
    );

    // Every run is infra — none pass because none self-report.
    // The first 2 carry infra_crash (retried by begin_infra_retry).
    // The last one settled after retry exhaustion — carries unreported.
    for run in &flaky_runs {
        assert_eq!(run.status, GraphRunStatus::Fail);
    }
    assert!(
        flaky_runs
            .iter()
            .filter(|r| {
                r.output
                    .as_ref()
                    .and_then(|o| o.get("infra_crash"))
                    .and_then(|v| v.as_bool())
                    == Some(true)
            })
            .count()
            >= 2,
        "retried runs must carry infra_crash marker"
    );
    assert!(
        flaky_runs.iter().any(|r| {
            r.output
                .as_ref()
                .and_then(|o| o.get("unreported"))
                .and_then(|v| v.as_bool())
                == Some(true)
        }),
        "at least one run must carry unreported marker"
    );
}

/// CM2 regression: an agent that infra-crashes once and then *recovers*
/// on the in-place retry must follow its `Pass` edge — the `Error` edge
/// (present here, as default pre-wiring would add it) must NOT fire just
/// because an earlier attempt crashed. Routing keys off the settled
/// attempt, not "did any attempt crash".
#[tokio::test]
async fn recovered_infra_retry_takes_pass_edge_not_error_edge() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // Fails fast on the first invocation, succeeds on the second.
    let marker = dir.path().join("recover-marker");
    let script = dir.path().join("flaky-cli");
    std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ -f \"{m}\" ]; then printf ok; exit 0; else touch \"{m}\"; exit 1; fi\n",
                m = marker.to_string_lossy()
            ),
        )
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "flaky-cli".to_string(),
            binary: script.to_string_lossy().to_string(),
            headless_mode: "-c".to_string(),
            model_flag: None,
            supports_working_dir: false,
            working_dir_flag: None,
            env_vars: std::collections::HashMap::new(),
            interactive_args: None,
            fallback_interactive_args: None,
            resume_args: None,
            session_list_cmd: None,
            session_resume_cmd: None,
            accent_color: None,
            yolo_flag: None,
            prompt_via_stdin: false,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-flaky".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "flaky".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "flaky-cli",
            "prompt_template": "ignored",
            "infra_retry_limit": 2,
            "infra_crash_max_seconds": 60,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-after".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "after".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf AFTER",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-infra".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "infra".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf INFRA",
            "success_condition": "exit_code_0"
        }),
        position: 3,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "edge-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-flaky".to_string(),
        to_node: "node-after".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-flaky".to_string(),
        to_node: "node-infra".to_string(),
        condition: GraphEdgeCondition::Error,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let flaky_runs: Vec<_> = runs.iter().filter(|r| r.node_id == "node-flaky").collect();
    let after_runs: Vec<_> = runs.iter().filter(|r| r.node_id == "node-after").collect();
    let infra_runs: Vec<_> = runs.iter().filter(|r| r.node_id == "node-infra").collect();

    assert_eq!(
        flaky_runs.len(),
        3,
        "CM13: initial + 2 retries = 3 run rows (all unreported infra)"
    );
    // CM13: all runs are infra, none pass (none self-report).
    for run in &flaky_runs {
        assert_eq!(run.status, GraphRunStatus::Fail);
    }
    assert_eq!(
        after_runs.len(),
        0,
        "CM13: pass edge must NOT fire when all runs are unreported infra"
    );
    assert_eq!(
        infra_runs.len(),
        1,
        "CM13: error edge fires when retries exhaust (all runs unreported)"
    );
}

#[test]
fn parse_router_config_reads_routes_and_fallback() {
    let node = router_node(
        "r1",
        "spec-1",
        "test-cli",
        &[("billing", "b"), ("technical", "t")],
        "technical",
        1,
    );
    let (routes, fallback) = parse_router_config(&node).unwrap();
    assert_eq!(routes.len(), 2);
    assert_eq!(routes[0].label, "billing");
    assert_eq!(routes[0].description, "b");
    assert_eq!(fallback, "technical");
}

#[test]
fn parse_router_config_rejects_missing_routes_array() {
    let mut node = router_node(
        "r1",
        "spec-1",
        "test-cli",
        &[("billing", "b")],
        "billing",
        1,
    );
    node.config.as_object_mut().unwrap().remove("routes");
    let err = parse_router_config(&node).unwrap_err();
    assert!(err.to_string().contains("routes"));
}

#[test]
fn parse_router_config_rejects_missing_fallback() {
    let mut node = router_node(
        "r1",
        "spec-1",
        "test-cli",
        &[("billing", "b")],
        "billing",
        1,
    );
    node.config.as_object_mut().unwrap().remove("fallback");
    let err = parse_router_config(&node).unwrap_err();
    assert!(err.to_string().contains("fallback"));
}

#[test]
fn match_router_token_matches_exact_trimmed_label() {
    let routes = sample_routes();
    assert_eq!(match_router_token("billing\n", &routes), Some("billing"));
    assert_eq!(
        match_router_token("  technical  ", &routes),
        Some("technical")
    );
}

#[test]
fn match_router_token_rejects_bare_word_inside_narration() {
    let routes = sample_routes();
    assert_eq!(
        match_router_token("I think billing is the right route here.", &routes),
        None,
        "a route label appearing inside narration must never match"
    );
}

#[test]
fn match_router_token_returns_none_for_unknown_answer() {
    let routes = sample_routes();
    assert_eq!(match_router_token("nonsense", &routes), None);
}

#[test]
fn select_router_step_resolves_matching_route_edge() {
    let edges = vec![
        route_edge("e1", "s", "r", "n-billing", "billing"),
        route_edge("e2", "s", "r", "n-technical", "technical"),
    ];
    let sel = select_router_step(&edges, "r", "technical")
        .unwrap()
        .unwrap();
    assert_eq!(sel.cursor, SpecCursor::Node("n-technical".to_string()));
}

#[test]
fn select_router_step_returns_none_when_route_unwired() {
    let edges = vec![route_edge("e1", "s", "r", "n-billing", "billing")];
    assert!(select_router_step(&edges, "r", "technical")
        .unwrap()
        .is_none());
}

#[test]
fn select_router_step_ambiguous_distinct_targets_errors() {
    let edges = vec![
        route_edge("e1", "s", "r", "n-a", "billing"),
        route_edge("e2", "s", "r", "n-b", "billing"),
    ];
    let err = select_router_step(&edges, "r", "billing").unwrap_err();
    assert!(err.to_string().contains("ambiguous"));
}

#[tokio::test]
async fn execute_router_node_spawn_failure_is_a_node_failure() {
    let (_dir, db) = test_db();
    let fake_home = setup_multi_cli_home(&[("broken-cli", "/nonexistent/nowhere/binary-xyz")]);
    let node = router_node(
        "r1",
        "spec-1",
        "broken-cli",
        &[("billing", "Billing"), ("technical", "Technical")],
        "technical",
        1,
    );
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution = execute_router_node(
        &db,
        &lp,
        &spec,
        &node,
        None,
        "run-router-spawn-fail",
        "/tmp",
    )
    .await
    .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(
        execution.output.get("route").is_none(),
        "a spawn failure must not carry a chosen route"
    );
}

#[tokio::test]
async fn execute_router_node_matches_declared_route_from_stdout() {
    let (dir, db) = test_db();
    let script = write_member_script(dir.path(), "router-billing.sh", "printf 'billing\\n'");
    let fake_home = setup_multi_cli_home(&[("router-billing-cli", &script)]);
    let node = router_node(
        "r1",
        "spec-1",
        "router-billing-cli",
        &[("billing", "Billing"), ("technical", "Technical")],
        "technical",
        1,
    );
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution = execute_router_node(&db, &lp, &spec, &node, None, "run-router-billing", "/tmp")
        .await
        .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output.get("route").and_then(Value::as_str),
        Some("billing")
    );
    assert_eq!(
        execution
            .output
            .get("used_fallback")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        execution.output.get("raw_answer").and_then(Value::as_str),
        Some("billing")
    );
}

#[tokio::test]
async fn execute_router_node_falls_back_and_records_raw_answer_when_unparseable() {
    let (dir, db) = test_db();
    let script = write_member_script(
        dir.path(),
        "router-narration.sh",
        "printf 'I think billing fits best.\\n'",
    );
    let fake_home = setup_multi_cli_home(&[("router-narration-cli", &script)]);
    let node = router_node(
        "r1",
        "spec-1",
        "router-narration-cli",
        &[("billing", "Billing"), ("technical", "Technical")],
        "technical",
        1,
    );
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution =
        execute_router_node(&db, &lp, &spec, &node, None, "run-router-narration", "/tmp")
            .await
            .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output.get("route").and_then(Value::as_str),
        Some("technical"),
        "an unparseable answer must take the declared fallback"
    );
    assert_eq!(
        execution
            .output
            .get("used_fallback")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        execution.output.get("raw_answer").and_then(Value::as_str),
        Some("I think billing fits best.")
    );
}

/// Acceptance: a three-route router graph takes a different path per
/// input, and each decision — chosen route, raw answer — is persisted on
/// the router node's own run row: the same JSON blob `execute_router_node`
/// logs via `tracing::info!` (B43's node-run lifecycle logging) is what
/// `update_graph_run_result` writes, so the run row is the queryable
/// record of what the daemon log carries.
#[tokio::test]
async fn three_route_router_graph_takes_a_different_path_per_input() {
    for (answer, expected_marker) in [
        ("billing", "billing.marker"),
        ("technical", "technical.marker"),
        ("other", "other.marker"),
    ] {
        let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
        let script = write_member_script(dir.path(), "router.sh", &format!("printf '{answer}\\n'"));
        let fake_home = setup_multi_cli_home(&[("router-cli", &script)]);

        db.insert_graph_node(&router_node(
            "router",
            &spec_id,
            "router-cli",
            &[
                ("billing", "Billing questions"),
                ("technical", "Technical issues"),
                ("other", "Everything else"),
            ],
            "other",
            1,
        ))
        .unwrap();

        for (position, (route_label, marker_name, node_id)) in [
            ("billing", "billing.marker", "path-billing"),
            ("technical", "technical.marker", "path-technical"),
            ("other", "other.marker", "path-other"),
        ]
        .into_iter()
        .enumerate()
        {
            db.insert_graph_node(&GraphNode {
                id: node_id.to_string(),
                spec_id: Some(spec_id.clone()),
                graph_id: None,
                name: node_id.to_string(),
                kind: GraphNodeKind::Check,
                config: serde_json::json!({
                    "command": format!("touch \"{}\"", dir.path().join(marker_name).display()),
                    "success_condition": "exit_code_0"
                }),
                position: 2 + position as i64,
                created_at: chrono::Utc::now(),
            })
            .unwrap();
            db.insert_graph_edge(&route_edge(
                &format!("edge-{route_label}"),
                &spec_id,
                "router",
                node_id,
                route_label,
            ))
            .unwrap();
        }

        let _home = HomeGuard::set(fake_home.path());
        let result = engine
            .run_graph(graph_id.clone(), None, None, None, None, None)
            .await;
        drop(_home);
        result.unwrap();

        let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
        assert_eq!(
            spec.status,
            GraphSpecStatus::Completed,
            "route '{answer}' must complete the spec"
        );

        for marker in ["billing.marker", "technical.marker", "other.marker"] {
            let exists = dir.path().join(marker).exists();
            if marker == expected_marker {
                assert!(exists, "expected marker '{marker}' for answer '{answer}'");
            } else {
                assert!(
                    !exists,
                    "unexpected marker '{marker}' for answer '{answer}'"
                );
            }
        }

        let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
        let router_run = runs.iter().find(|r| r.node_id == "router").unwrap();
        assert_eq!(router_run.status, GraphRunStatus::Pass);
        let output = router_run.output.as_ref().unwrap();
        assert_eq!(output.get("route").and_then(Value::as_str), Some(answer));
        assert_eq!(
            output.get("raw_answer").and_then(Value::as_str),
            Some(answer)
        );
        assert_eq!(
            output.get("used_fallback").and_then(Value::as_bool),
            Some(false)
        );
    }
}

/// CB6: a router must be transparent — the node after it must see what
/// the router read (`previous_output` before the router), not the
/// router's own `{kind, route}` verdict. This covers the natural
/// placement `check(fail) -> router -> check`, where the check after
/// the router needs the failure payload the reviewer produced.
#[tokio::test]
async fn router_preserves_previous_output_for_next_node() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "reviewer".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "reviewer".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "echo 'CHANGE_LIST: fix line 42, update test' && exit 1",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let router_script = write_member_script(dir.path(), "router.sh", "printf 'changes\\n'");
    let fake_home = setup_multi_cli_home(&[("router-cli", &router_script)]);

    db.insert_graph_node(&router_node(
        "router",
        &spec_id,
        "router-cli",
        &[
            ("changes", "Reviewer asked for changes"),
            ("quota", "Quota exhausted"),
        ],
        "changes",
        2,
    ))
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "implementer".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implementer".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0"
        }),
        position: 3,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_edge(&GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "reviewer".to_string(),
        to_node: "router".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();
    db.insert_graph_edge(&route_edge(
        "e2",
        &spec_id,
        "router",
        "implementer",
        "changes",
    ))
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let impl_run = runs.iter().find(|r| r.node_id == "implementer").unwrap();
    let input = impl_run
        .input
        .as_ref()
        .expect("implementer must have input");

    // Implementer must receive the reviewer's check output, not the router's.
    assert!(
        input.get("kind").is_none() || input.get("kind").and_then(Value::as_str) != Some("router"),
        "implementer input must not be the router's output, got: {input}"
    );
    assert!(
        input.get("route").is_none(),
        "implementer input must not contain router's route field, got: {input}"
    );
    assert!(
        input.get("stdout").is_some(),
        "implementer input must be the reviewer's check output (has stdout field), got: {input}"
    );
    assert_eq!(
        input.get("exit_code").and_then(Value::as_u64),
        Some(1),
        "implementer input must be the reviewer's failed check output, got: {input}"
    );
    assert!(
        input
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or("")
            .contains("CHANGE_LIST"),
        "implementer must see the reviewer's stdout, got: {input}"
    );
}

/// Concurrency invariant: starting, pausing, resuming, and finishing one
/// graph must never change any observable state of another graph in a
/// different workdir sharing the same database. Graph A is seeded
/// mid-run and never driven again — its persisted rows are the baseline.
/// Graph B is driven through a full, real start → pause → resume →
/// finish cycle via the same `GraphEngine` a shared daemon would use, and
/// graph A's graph/spec/run rows must be byte-identical (via their
/// serialized JSON) before and after.
#[tokio::test]
async fn graph_b_full_lifecycle_never_touches_graph_a_in_a_different_workdir() {
    let db_dir = tempdir().unwrap();
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    let db = Arc::new(Database::new(&db_dir.path().join("shared.db")).unwrap());

    // Graph A: seeded as mid-run and left alone for the rest of the test.
    let graph_a = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-graph-a".to_string(),
        name: "Graph A".to_string(),
        description: None,
        workdir: dir_a.path().to_string_lossy().to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    };
    let spec_a = GraphSpec {
        id: "spec-graph-a".to_string(),
        graph_id: Some(graph_a.id.clone()),
        name: "Spec A".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Running,
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        spec_start_head: Some("deadbeef".to_string()),
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
    let node_a = GraphNode {
        id: "node-graph-a".to_string(),
        spec_id: Some(spec_a.id.clone()),
        graph_id: None,
        name: "Node A".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    let run_a = GraphNodeRun {
        id: "run-graph-a".to_string(),
        graph_id: graph_a.id.clone(),
        spec_id: spec_a.id.clone(),
        node_id: node_a.id.clone(),
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
    };
    db.insert_graph(&graph_a).unwrap();
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_node(&node_a).unwrap();
    db.insert_graph_run(&run_a).unwrap();

    let snapshot = |db: &Database| {
        (
            serde_json::to_value(db.get_graph(&graph_a.id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(db.get_graph_spec(&spec_a.id).unwrap().unwrap()).unwrap(),
            serde_json::to_value(db.get_graph_run(&run_a.id).unwrap().unwrap()).unwrap(),
        )
    };
    let snapshot_before = snapshot(&db);

    // Graph B: a real graph in a different workdir, driven through its
    // full lifecycle by the engine.
    let graph_b = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-graph-b".to_string(),
        name: "Graph B".to_string(),
        description: None,
        workdir: dir_b.path().to_string_lossy().to_string(),
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
    let spec_b = GraphSpec {
        id: "spec-graph-b".to_string(),
        graph_id: Some(graph_b.id.clone()),
        name: "Spec B".to_string(),
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
    db.insert_graph(&graph_b).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-graph-b".to_string(),
        spec_id: Some(spec_b.id.clone()),
        graph_id: None,
        name: "Node B".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 0.3 && true",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let engine = Arc::new(GraphEngine::new(
        Arc::clone(&db),
        Arc::new(DefaultNotificationService),
    ));

    // Start graph B in the background.
    let dispatch_engine = Arc::clone(&engine);
    let graph_b_id = graph_b.id.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_engine
            .run_graph(graph_b_id, None, None, None, None, None)
            .await
    });

    // Poll (never a fixed sleep) until graph B's node run is actually
    // recorded `Running` before pausing it, to avoid a flaky race
    // against process spawn.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if !db
            .list_running_graph_runs(&graph_b.id)
            .unwrap_or_default()
            .is_empty()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph B's node run never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    // A snapshot mid-run of graph B confirms graph A is unaffected by
    // graph B's mere presence as `Running`, not just by its start/end.
    assert_eq!(
        snapshot(&db),
        snapshot_before,
        "graph A must be untouched while graph B is starting"
    );

    // Pause graph B.
    assert!(engine.request_pause(&graph_b.id, false).unwrap());
    dispatch.await.unwrap().unwrap();
    assert_eq!(
        db.get_graph(&graph_b.id).unwrap().unwrap().status,
        GraphStatus::Paused,
        "graph B must actually have paused for this test to be meaningful"
    );
    assert_eq!(
        snapshot(&db),
        snapshot_before,
        "graph A must be untouched by graph B pausing"
    );

    // Resume graph B — relaunching a paused graph directly is a
    // supported, documented entry point of `run_graph`.
    engine
        .run_graph(graph_b.id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let graph_b_after = db.get_graph(&graph_b.id).unwrap().unwrap();
    assert_eq!(
        graph_b_after.status,
        GraphStatus::Completed,
        "graph B must have finished its lifecycle for this test to be meaningful"
    );

    assert_eq!(
        snapshot(&db),
        snapshot_before,
        "graph A's persisted state must be byte-identical after graph B's full \
             start/pause/resume/finish lifecycle"
    );
}

/// A graph's node run must never be signalled by a process that did not
/// launch it (no pid, no boot id recorded here — the shape a crashed
/// prior boot leaves behind). Pausing graph B — whose own running node
/// carries a real pid — must leave graph A's running node run row
/// completely untouched, even though both rows describe a `Running`
/// node run at the same instant. Asserted on the persisted run row
/// (not the daemon log), per the invariant that the 2026-08-03 incident
/// was invisible in the daemon's own log.
#[tokio::test]
async fn pausing_graph_b_never_signals_or_mutates_graph_a_run() {
    let db_dir = tempdir().unwrap();
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    let db = Arc::new(Database::new(&db_dir.path().join("shared.db")).unwrap());

    let graph_a = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-signal-a".to_string(),
        name: "Graph A".to_string(),
        description: None,
        workdir: dir_a.path().to_string_lossy().to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    };
    let spec_a = GraphSpec {
        id: "spec-signal-a".to_string(),
        graph_id: Some(graph_a.id.clone()),
        name: "Spec A".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Running,
        started_at: Some(chrono::Utc::now()),
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
    let node_a = GraphNode {
        id: "node-signal-a".to_string(),
        spec_id: Some(spec_a.id.clone()),
        graph_id: None,
        name: "Node A".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    // A "running" node run with no pid of its own — never a process this
    // test spawns, so any attempt to signal it would be a no-op at best
    // and a wrong-process kill at worst; the assertion below is on the
    // row, not on process survival.
    let run_a = GraphNodeRun {
        id: "run-signal-a".to_string(),
        graph_id: graph_a.id.clone(),
        spec_id: spec_a.id.clone(),
        node_id: node_a.id.clone(),
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
    };
    db.insert_graph(&graph_a).unwrap();
    db.insert_graph_spec(&spec_a).unwrap();
    db.insert_graph_node(&node_a).unwrap();
    db.insert_graph_run(&run_a).unwrap();
    let run_a_before = serde_json::to_value(db.get_graph_run(&run_a.id).unwrap().unwrap()).unwrap();

    let graph_b = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf-signal-b".to_string(),
        name: "Graph B".to_string(),
        description: None,
        workdir: dir_b.path().to_string_lossy().to_string(),
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
    let spec_b = GraphSpec {
        id: "spec-signal-b".to_string(),
        graph_id: Some(graph_b.id.clone()),
        name: "Spec B".to_string(),
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
    db.insert_graph(&graph_b).unwrap();
    db.insert_graph_spec(&spec_b).unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-signal-b".to_string(),
        spec_id: Some(spec_b.id.clone()),
        graph_id: None,
        name: "Node B".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 0.3 && true",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let engine = Arc::new(GraphEngine::new(
        Arc::clone(&db),
        Arc::new(DefaultNotificationService),
    ));
    let dispatch_engine = Arc::clone(&engine);
    let graph_b_id = graph_b.id.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_engine
            .run_graph(graph_b_id, None, None, None, None, None)
            .await
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if !db
            .list_running_graph_runs(&graph_b.id)
            .unwrap_or_default()
            .is_empty()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "graph B's node run never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    assert!(engine.request_pause(&graph_b.id, false).unwrap());
    dispatch.await.unwrap().unwrap();

    let b_runs = db.list_graph_runs_for_spec(&spec_b.id).unwrap();
    assert_eq!(
        b_runs.len(),
        1,
        "graph B's node must have completed naturally and been finalized"
    );
    // With the new pause behavior, the node runs to completion — the
    // command `sleep 0.3 && true` exits 0, so the run is Pass, not Fail.
    assert_eq!(b_runs[0].status, GraphRunStatus::Pass);

    let run_a_after = serde_json::to_value(db.get_graph_run(&run_a.id).unwrap().unwrap()).unwrap();
    assert_eq!(
        run_a_after, run_a_before,
        "graph A's node run must never be signalled or mutated by graph B's pause"
    );
}

/// Req 1 + 2: a default pause waits for the running node, is visible as a
/// distinct `pausing` status while it waits, and the node's run is recorded
/// with its own outcome — the graph only reaches `paused` once it finished.
#[tokio::test]
async fn pause_waits_for_node_and_reports_pausing_until_it_finishes() {
    let (_dir, db, engine, graph_id, spec_id) = cb31_fixture("sleep 2 && true");
    let disp = {
        let (e, id) = (Arc::clone(&engine), graph_id.clone());
        tokio::spawn(async move { e.run_graph(id, None, None, None, None, None).await })
    };

    cb31_wait_for_running_run(&db, &graph_id).await;
    assert!(engine.request_pause(&graph_id, false).unwrap());

    assert_eq!(
            db.get_graph(&graph_id).unwrap().unwrap().status,
            GraphStatus::Pausing,
            "an accepted wait-for-completion pause is visible as `pausing`, distinct from running/paused"
        );
    assert!(
        !db.list_running_graph_runs(&graph_id).unwrap().is_empty(),
        "the running node must not be terminated by a default pause"
    );

    disp.await.unwrap().unwrap();

    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Paused,
        "the graph reaches `paused` only after the node finished"
    );
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(
        runs[0].status,
        GraphRunStatus::Pass,
        "the node ran to its own completion and is recorded Pass — not Fail, not Interrupted"
    );
}

/// Req 3 + 4 + guideline 7: `interrupt: true` stops the node now and
/// records it as operator-interrupted, distinguishable in run history from
/// a node that failed on its own — and never carrying the engine's
/// out-of-band `terminated` marker.
#[tokio::test]
async fn explicit_interrupt_records_operator_interrupted_not_failure() {
    let (_dir, db, engine, graph_id, spec_id) = cb31_fixture("sleep 30 && true");
    let disp = {
        let (e, id) = (Arc::clone(&engine), graph_id.clone());
        tokio::spawn(async move { e.run_graph(id, None, None, None, None, None).await })
    };

    cb31_wait_for_running_run(&db, &graph_id).await;
    assert!(engine.request_pause(&graph_id, true).unwrap());

    disp.await.unwrap().unwrap();

    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Paused
    );
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Interrupted);
    assert_ne!(
        runs[0].status,
        GraphRunStatus::Fail,
        "an operator's decision to stop the node must not read as the node failing"
    );
    let output = runs[0].output.clone().unwrap();
    assert_eq!(
        output.get("interrupted").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert!(
        output.get("terminated").is_none(),
        "an operator interrupt is not the engine's out-of-band termination"
    );
}

/// Req 5 + guideline 4 (waiting pause): a pause-and-continue cycle does not
/// advance the node's iteration counter — the node re-runs on continue at
/// the same iteration number it paused on.
#[tokio::test]
async fn waiting_pause_and_continue_consumes_no_iteration() {
    let (_dir, db, engine, graph_id, spec_id) = cb31_fixture("sleep 2 && true");
    let disp = {
        let (e, id) = (Arc::clone(&engine), graph_id.clone());
        tokio::spawn(async move { e.run_graph(id, None, None, None, None, None).await })
    };

    cb31_wait_for_running_run(&db, &graph_id).await;
    assert!(engine.request_pause(&graph_id, false).unwrap());
    disp.await.unwrap().unwrap();

    let before = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].iteration, 1);

    // Continue (the resume path behind graph_continue).
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

    let after = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(after.len(), 2, "the node re-ran once on continue");
    assert!(
        after.iter().all(|r| r.iteration == 1),
        "pause-and-continue must not consume an iteration; saw {:?}",
        after.iter().map(|r| r.iteration).collect::<Vec<_>>()
    );
}

/// Req 5 + guideline 4 (explicit interrupt): an interrupt-and-continue
/// cycle likewise leaves the iteration counter untouched.
#[tokio::test]
async fn interrupt_and_continue_consumes_no_iteration() {
    // First run blocks on `sleep` (no marker yet) so it can be interrupted;
    // once the marker exists the continue re-run returns immediately.
    let (dir, db, engine, graph_id, spec_id) = {
        let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
        let marker = dir.path().join("go.marker");
        db.insert_graph_node(&GraphNode {
            id: "cb31-node".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "work".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": format!("test -f {} || sleep 30", marker.display()),
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        (dir, Arc::clone(&db), Arc::new(engine), graph_id, spec_id)
    };
    let disp = {
        let (e, id) = (Arc::clone(&engine), graph_id.clone());
        tokio::spawn(async move { e.run_graph(id, None, None, None, None, None).await })
    };

    cb31_wait_for_running_run(&db, &graph_id).await;
    assert!(engine.request_pause(&graph_id, true).unwrap());
    disp.await.unwrap().unwrap();

    std::fs::write(dir.path().join("go.marker"), b"").unwrap();

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

    let after = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        after.iter().all(|r| r.iteration == 1),
        "interrupt-and-continue must not consume an iteration; saw {:?}",
        after.iter().map(|r| r.iteration).collect::<Vec<_>>()
    );
}
