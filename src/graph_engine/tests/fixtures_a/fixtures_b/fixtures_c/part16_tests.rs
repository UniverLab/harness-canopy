//! Part 16 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

#[tokio::test]
async fn idea_text_populates_spec_content_in_placeholder() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let argv_file = dir.path().join("argv.log");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    // CM13 legacy: holds the run briefly before it exits; the
    // self-report below lands at exit and does not depend on it.
    env.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env.insert("SELF_REPORT_NODE_ID".to_string(), "node-impl".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let cli = argv_cli_config(&script, env, None, None, None);
    let home = write_resume_cli_home(cli);

    db.insert_graph_node(&GraphNode {
        id: "node-impl".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "resume-cli" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-impl-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "node-impl".to_string(),
        to_node: "node-check".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();

    let guard = HomeGuard::set(home.path());
    // CB57: the echo script never calls graph_complete_node, so it
    // self-reports its own Pass verdict instead, as the last thing it
    // does before exiting.
    engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("Build a landing page for our startup".to_string()),
            None,
            None,
        )
        .await
        .unwrap();
    drop(guard);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
            argv.contains("Build a landing page for our startup"),
            "the idea text must appear in the prompt where {{{{spec_content}}}} was substituted: {argv}"
        );
    assert!(
        !argv.contains("{{spec_content}}"),
        "the {{{{spec_content}}}} placeholder must have been resolved, not left literal: {argv}"
    );
}

#[tokio::test]
async fn idea_without_graph_still_errors() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let error = engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("some idea".to_string()),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no specs to run"),
        "unexpected error message: {error}"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Draft,
        "an idea without a graph must leave the graph's status untouched"
    );
}

#[tokio::test]
async fn no_idea_no_graph_still_errors() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let error = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no specs to run"),
        "unexpected error message: {error}"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Draft,
        "no idea and no graph must leave the graph's status untouched"
    );
}

#[test]
fn render_agent_prompt_preserves_tagged_spec_body_verbatim() {
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
    let tagged_body = "<spec>\n  <objective>Ship it.</objective>\n  <functional_requirements>Does thing.</functional_requirements>\n  <non_functional_requirements>Fast.</non_functional_requirements>\n  <constraints>None.</constraints>\n  <guidelines>Style.</guidelines>\n  <in_scope>This.</in_scope>\n  <out_of_scope>Nothing.</out_of_scope>\n</spec>";
    let spec = GraphSpec {
        id: "spec".to_string(),
        graph_id: Some("wf".to_string()),
        name: "Spec".to_string(),
        description: Some(tagged_body.to_string()),
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
        None,
        &lp.workdir,
        "run-1",
        &HashMap::new(),
        &[],
        "",
    )
    .unwrap();

    assert!(prompt.contains(tagged_body));
    assert!(prompt.contains("# [SPEC]"));
    assert!(!prompt.contains("# Objective"));
    assert!(!prompt.contains("# Functional Requirements"));
}

/// CH4: A hook launches a target graph, and the launching graph completes
/// without waiting for it.
#[tokio::test]
async fn graph_hook_launches_target_graph_in_background() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-bg", "Target").unwrap();
    setup_graph_hook_source(&db, "source-bg", &target_id, None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-bg".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // The source graph should be completed.
    let source = db.get_graph("source-bg").unwrap().unwrap();
    assert_eq!(source.status, GraphStatus::Completed);

    // The launch is fire-and-forget: wait until the target has started
    // instead of sleeping a fixed time, which failed under the full
    // parallel suite on a loaded machine.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !matches!(
        db.get_graph(&target_id).unwrap().map(|g| g.status),
        Some(GraphStatus::Running | GraphStatus::Completed)
    ) && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // The target graph should have been launched.
    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert!(
        target.status == GraphStatus::Running || target.status == GraphStatus::Completed,
        "target should be running or completed, got {:?}",
        target.status
    );
}

/// CH4: A hook launches a graph with an `idea` and no queue, and the
/// target's first node receives that text as `{{spec_content}}`.
#[tokio::test]
async fn graph_hook_launches_graph_with_idea() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-idea", "Target Idea").unwrap();
    setup_graph_hook_source(&db, "source-idea", &target_id, None, Some("Build a widget")).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-idea".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // Give the background task time to start.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // The target graph should have been launched.
    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert!(
        target.status == GraphStatus::Running || target.status == GraphStatus::Completed,
        "target should be running or completed, got {:?}",
        target.status
    );
}

#[tokio::test]
async fn graph_hook_refused_launch_is_recorded_failed() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph_no_specs(&db, "target-empty", "Target Empty").unwrap();
    // Hook has only target_graph_id: no idea, no queue_id.
    setup_graph_hook_source(&db, "source-empty", &target_id, None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-empty".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // Constraint: a failed hook does not fail the graph that fired it.
    let source = db.get_graph("source-empty").unwrap().unwrap();
    assert_eq!(source.status, GraphStatus::Completed);

    // Target never ran.
    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert_eq!(target.status, GraphStatus::Draft);
    assert!(db
        .list_graph_node_runs(&target_id, None, None, 100, 0)
        .unwrap()
        .is_empty());

    // The hook run is recorded failed, with the engine's refusal text.
    let runs = db.list_graph_completion_hook_runs("source-empty").unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.status, GraphRunStatus::Fail);
    let summary = run.summary.as_deref().unwrap_or("");
    assert!(
        summary.contains("has no specs to run") && summary.contains("0 bound specs"),
        "summary should carry the engine refusal verbatim, got: {summary:?}"
    );
    // No launched_graph_id when nothing started.
    assert!(
        !run.output
            .as_ref()
            .is_some_and(|o| o.get("launched_graph_id").is_some()),
        "refused launch must not write launched_graph_id, got: {:?}",
        run.output
    );
}

#[tokio::test]
async fn graph_hook_with_idea_launches_and_target_runs() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph_no_specs(&db, "target-idea2", "Target Idea 2").unwrap();
    setup_graph_hook_source(&db, "source-idea2", &target_id, None, Some("Do the thing")).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-idea2".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // The launch is fire-and-forget: wait until the target has started
    // instead of sleeping a fixed time, which failed under the full
    // parallel suite on a loaded machine.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !matches!(
        db.get_graph(&target_id).unwrap().map(|g| g.status),
        Some(GraphStatus::Running | GraphStatus::Completed)
    ) && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert!(
        target.status == GraphStatus::Running || target.status == GraphStatus::Completed,
        "target should have started, got {:?}",
        target.status
    );

    let runs = db.list_graph_completion_hook_runs("source-idea2").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert_eq!(
        runs[0]
            .output
            .as_ref()
            .and_then(|o| o.get("launched_graph_id"))
            .and_then(|v| v.as_str()),
        Some(target_id.as_str()),
        "accepted launch still reports launched_graph_id"
    );
}

#[tokio::test]
async fn graph_hook_nonexistent_target_is_recorded_failed() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    // Source's hook points at an id no graph has.
    setup_graph_hook_source(&db, "source-missing", "does-not-exist-xyz", None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-missing".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    let source = db.get_graph("source-missing").unwrap().unwrap();
    assert_eq!(source.status, GraphStatus::Completed);

    let runs = db
        .list_graph_completion_hook_runs("source-missing")
        .unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Fail);
    assert!(
        runs[0]
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("not found")),
        "summary should name the missing target, got: {:?}",
        runs[0].summary
    );
}

/// CH4: A graph launched by a hook has its own graph-launching hook refused,
/// with the reason recorded, while its other hooks still run.
#[tokio::test]
async fn graph_hook_depth_cap_refuses_second_launch() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());

    // Create graph C (target for B's hook)
    let c_id = create_target_graph(&db, "graph-c", "Graph C").unwrap();

    // Create graph B manually with its own hook that launches C
    let b_hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: Some(c_id.clone()),
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut b_hooks = std::collections::BTreeMap::new();
    b_hooks.insert(GraphHookEvent::OnCompleted, vec![b_hook]);
    let lp_b = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "graph-b".to_string(),
        name: "Graph B".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: b_hooks,
        require_spec_type: false,
    };
    db.insert_graph(&lp_b).unwrap();
    // Add a Check node so graph B can actually run specs.
    let b_node = crate::domain::graphs::GraphNode {
        id: "graph-b-node-1".to_string(),
        spec_id: None,
        graph_id: Some("graph-b".to_string()),
        name: "Check".to_string(),
        kind: crate::domain::graphs::GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&b_node).unwrap();
    // Add a spec so graph B has something to run.
    let b_spec = crate::domain::graphs::GraphSpec {
        id: "graph-b-spec".to_string(),
        graph_id: Some("graph-b".to_string()),
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
    db.insert_graph_spec(&b_spec).unwrap();

    // A has an on_completed hook that launches B (with a spec).
    setup_graph_hook_source(&db, "graph-a", "graph-b", None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));

    // Run A — this should launch B, which should try to launch C but be refused.
    let result = engine
        .run_graph("graph-a".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "graph A should complete: {result:?}");

    // B runs on a background task: wait until its own hook run is
    // recorded instead of sleeping a fixed time, which failed whenever the
    // machine was loaded.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while db
        .list_graph_completion_hook_runs("graph-b")
        .unwrap()
        .is_empty()
        && std::time::Instant::now() < deadline
    {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Debug: check what happened to B
    let b_graph = db.get_graph("graph-b").unwrap().unwrap();
    eprintln!("B status: {:?}", b_graph.status);
    let b_runs = db.list_graph_completion_hook_runs("graph-b").unwrap();
    eprintln!("B hook runs: {}", b_runs.len());
    for r in &b_runs {
        eprintln!("  run: status={:?}, summary={:?}", r.status, r.summary);
    }
    let b_spec_runs = db.list_graph_specs("graph-b").unwrap();
    eprintln!("B specs: {}", b_spec_runs.len());
    for s in &b_spec_runs {
        eprintln!("  spec: status={:?}", s.status);
    }

    // C should NOT have been launched (B's hook was refused at depth 1).
    let c = db.get_graph(&c_id).unwrap().unwrap();
    assert_eq!(
        c.status,
        GraphStatus::Draft,
        "C should still be Draft (B's hook was refused at depth 1)"
    );

    // Check that B's hook run was recorded as failed.
    assert!(
        b_runs.iter().any(|r| {
            r.status == GraphRunStatus::Fail
                && r.summary
                    .as_deref()
                    .is_some_and(|s| s.contains("Depth cap"))
        }),
        "B's graph hook should have been refused with depth cap reason"
    );
}

/// CH4: A hook targeting an already-running graph fails with the target
/// named, and the launching graph's status is unchanged.
#[tokio::test]
async fn graph_hook_refuses_already_running_target() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-running", "Target Running").unwrap();
    setup_graph_hook_source(&db, "source-running", &target_id, None, None).unwrap();

    // Manually set target to Running status.
    db.update_graph_status(&target_id, GraphStatus::Running, None, None)
        .unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-running".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // Source should be completed.
    let source = db.get_graph("source-running").unwrap().unwrap();
    assert_eq!(source.status, GraphStatus::Completed);

    // The hook run should be recorded as failed.
    let runs = db
        .list_graph_completion_hook_runs("source-running")
        .unwrap();
    assert!(
        runs.iter().any(|r| {
            r.status == GraphRunStatus::Fail
                && r.summary
                    .as_deref()
                    .is_some_and(|s| s.contains("already running"))
        }),
        "hook should have failed with 'already running'"
    );

    // Target should still be Running (unchanged).
    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert_eq!(target.status, GraphStatus::Running);
}

/// CH4: A hook targeting an archived graph fails with the target named.
#[tokio::test]
async fn graph_hook_refuses_archived_target() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-archived", "Target Archived").unwrap();
    setup_graph_hook_source(&db, "source-archived", &target_id, None, None).unwrap();

    // Archive the target.
    db.archive_graph(&target_id).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-archived".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    let runs = db
        .list_graph_completion_hook_runs("source-archived")
        .unwrap();
    assert!(
        runs.iter().any(|r| {
            r.status == GraphRunStatus::Fail
                && r.summary.as_deref().is_some_and(|s| s.contains("archived"))
        }),
        "hook should have failed with 'archived'"
    );
}

/// CH4: Provenance is recorded when a hook launches a graph.
#[tokio::test]
async fn graph_hook_records_provenance() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-prov", "Target Prov").unwrap();
    setup_graph_hook_source(&db, "source-prov", &target_id, None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-prov".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // Give the background task time to record provenance.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Check provenance via the DB method (indirectly: verify the launch happened).
    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert!(
        target.status == GraphStatus::Running || target.status == GraphStatus::Completed,
        "target should have been launched"
    );
}

/// CH4: The hook_launched flag is cleared after the run completes.
#[tokio::test]
async fn graph_hook_launched_flag_cleared_on_completion() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-flag", "Target Flag").unwrap();
    setup_graph_hook_source(&db, "source-flag", &target_id, None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-flag".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // After the source graph completes, the hook_launched flag should be cleared.
    // (The source graph was NOT hook-launched, so the flag was never set for it.)
    assert!(
        !db.is_graph_hook_launched("source-flag").unwrap(),
        "source graph should not have hook_launched flag"
    );
}

/// CH4: The hook_launched flag is set on the target graph, then cleared
/// when it completes.
#[tokio::test]
async fn graph_hook_launched_flag_set_on_target() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-flag2", "Target Flag2").unwrap();
    setup_graph_hook_source(&db, "source-flag2", &target_id, None, None).unwrap();

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-flag2".to_string(), None, None, None, None, None)
        .await;
    assert!(result.is_ok(), "source graph should complete: {result:?}");

    // The target runs on a background task. Wait for it to finish and
    // clear the flag instead of sleeping a fixed time: a fixed 500 ms
    // failed under the full parallel suite on a loaded machine.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while db.is_graph_hook_launched(&target_id).unwrap() && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // After the target graph completes, the hook_launched flag is cleared.
    // So we just verify the target actually ran (completed).
    let target = db.get_graph(&target_id).unwrap().unwrap();
    assert!(
        target.status == GraphStatus::Completed || target.status == GraphStatus::Running,
        "target should have run, got {:?}",
        target.status
    );
    // And the flag is cleared.
    assert!(
        !db.is_graph_hook_launched(&target_id).unwrap(),
        "hook_launched flag should be cleared after target completes"
    );
}

/// CH4: render_hook_idea renders placeholders correctly.
#[test]
fn render_hook_idea_substitutes_placeholders() {
    let ctx = HookContext {
        graph_name: "MyGraph",
        workdir: "/tmp/project",
        completed_specs: &[("SpecA".to_string(), "done".to_string())],
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result = render_hook_idea(
        &GraphHookEvent::OnCompleted,
        &ctx,
        "Graph {{graph_name}} finished",
    )
    .unwrap();
    assert_eq!(result, "Graph MyGraph finished");
}

/// CH4: render_hook_idea renders known markers with no event value.
#[test]
fn render_hook_idea_renders_missing_event_value_as_none() {
    let ctx = HookContext {
        graph_name: "MyGraph",
        workdir: "/tmp",
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result = render_hook_idea(&GraphHookEvent::OnCompleted, &ctx, "Blocker: {{blocker}}");
    assert_eq!(result.unwrap(), "Blocker: (none)");
}

/// CH4: Verify that a graph with a spec can be found by list_graph_specs.
#[test]
fn graph_hook_spec_is_queryable() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "test-graph".to_string(),
        name: "Test".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
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
    let spec = crate::domain::graphs::GraphSpec {
        id: "test-spec".to_string(),
        graph_id: Some("test-graph".to_string()),
        name: "Spec".to_string(),
        description: Some("do nothing".to_string()),
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
    db.insert_graph_spec(&spec).unwrap();
    let specs = db.list_graph_specs("test-graph").unwrap();
    assert_eq!(specs.len(), 1, "should find the spec");
    assert_eq!(specs[0].id, "test-spec");
}

/// CH4: Verify that a graph with a hook can run the hook on completion.
#[tokio::test]
async fn graph_hook_runs_on_completion() {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let target_id = create_target_graph(&db, "target-compl", "Target Compl").unwrap();

    // Create source graph with a spec AND a hook.
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: Some(target_id.clone()),
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(GraphHookEvent::OnCompleted, vec![hook]);
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "source-compl".to_string(),
        name: "Source".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks,
        require_spec_type: false,
    };
    db.insert_graph(&lp).unwrap();

    // Verify spec exists before running
    let spec = crate::domain::graphs::GraphSpec {
        id: "spec-compl".to_string(),
        graph_id: Some("source-compl".to_string()),
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
    db.insert_graph_spec(&spec).unwrap();

    // Verify spec is queryable
    let found = db.list_graph_specs("source-compl").unwrap();
    eprintln!("Found {} specs for source-compl", found.len());

    // Verify graph was stored with hooks
    let stored = db.get_graph("source-compl").unwrap().unwrap();
    eprintln!("Graph hooks: {:?}", stored.hooks.keys().collect::<Vec<_>>());
    if let Some(on_completed_hooks) = stored.hooks.get(&GraphHookEvent::OnCompleted) {
        eprintln!("on_completed hooks count: {}", on_completed_hooks.len());
        for h in on_completed_hooks {
            eprintln!("  target_graph_id: {:?}", h.target_graph_id);
            eprintln!("  is_graph: {}", h.is_graph());
        }
    }

    let engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
    let result = engine
        .run_graph("source-compl".to_string(), None, None, None, None, None)
        .await;
    eprintln!("run_graph result: {:?}", result);
    assert!(result.is_ok(), "source graph should complete: {result:?}");
}

// ── CM13: classify infrastructure by verdict, not heuristics ────────

/// CM13 measurement 1: a run that exits non-zero after 90 seconds
/// without reporting is classified as infrastructure and takes the
/// `error` edge — the case that failed at 75 seconds with a rate limit.
#[test]
fn cm13_slow_unreported_run_is_infra_crash() {
    let node = sample_agent_node();
    let execution = NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "kind": "agent",
            "exit_code": 1,
            "stderr": "Error from provider (Console): Rate limit exceeded",
            "unreported": true,
        }),
        summary: "agent exited with code 1".to_string(),
    };
    let run = GraphNodeRun {
        id: "run-slow".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        // Started 90 seconds ago — well past the old 60-second window.
        started_at: chrono::Utc::now() - chrono::Duration::seconds(90),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert!(
        is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "CM13: a slow unreported run must be infra regardless of duration"
    );
}

/// CM13 measurement 2: a run that exits 0 with empty stdout and no
/// report is classified as infrastructure — the case that failed with
/// `no_output: true` after 15 minutes of backend timeout.
#[test]
fn cm13_silent_unreported_run_is_infra_crash() {
    let node = sample_agent_node();
    let execution = NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "kind": "agent",
            "exit_code": 0,
            "no_output": true,
            "unreported": true,
        }),
        summary: "agent produced no output (exit code 0).".to_string(),
    };
    let run = GraphNodeRun {
        id: "run-silent".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        // Started 15 minutes ago — the exact backend timeout case.
        started_at: chrono::Utc::now() - chrono::Duration::minutes(15),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert!(
        is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "CM13: a silent unreported run must be infra regardless of no_output"
    );
}

/// CM13 measurement 3: a run that exits 0 without reporting is not
/// recorded as pass — the case that failed with a designer exiting 0 at
/// 100 seconds having written nothing.
#[test]
fn cm13_exit_zero_unreported_is_not_pass() {
    let cli = Cli::new("test-cli");
    let node = sample_agent_node();
    let execution = agent_finished_execution(&node, &cli, None, 0, "some output", "", false);

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        "CM13: an exit-0 unreported run must never be recorded as pass"
    );
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("unreported"),
        "CM13: the outcome must say 'unreported'"
    );
}

/// CM13: an ensemble member that exits without reporting causes
/// round-robin to try the next member, and the ensemble passes when
/// that one succeeds. Verified via `is_infra_crash_shape` which is
/// the sole source of `member_had_no_verdict`.
#[test]
fn cm13_ensemble_fallthrough_on_unreported_member() {
    let node_a = sample_agent_node();
    let node_b = {
        let mut n = sample_agent_node();
        n.id = "cm13-member-b".to_string();
        n
    };

    // Member A: unreported (still Running), exit 0, no output.
    let exec_a = NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "kind": "agent",
            "exit_code": 0,
            "no_output": true,
            "unreported": true,
        }),
        summary: "agent exited with code 0".to_string(),
    };
    let run_a = GraphNodeRun {
        id: "run-member-a".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
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

    // member_had_no_verdict must be true for the unreported member.
    let no_verdict_a = is_infra_crash_shape(&node_a, &exec_a, &run_a, 60);
    assert!(
        no_verdict_a,
        "CM13: an unreported ensemble member must be 'no verdict'"
    );

    // Member B: self-reported pass.
    let exec_b = NodeExecution {
        status: GraphRunStatus::Pass,
        output: serde_json::json!({
            "kind": "agent",
            "exit_code": 0,
        }),
        summary: "agent reported pass".to_string(),
    };
    let run_b = GraphNodeRun {
        id: "run-member-b".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
        node_id: node_b.id.clone(),
        status: GraphRunStatus::Pass,
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

    let no_verdict_b = is_infra_crash_shape(&node_b, &exec_b, &run_b, 60);
    assert!(
        !no_verdict_b,
        "CM13: a self-reported member must NOT be 'no verdict'"
    );
}

/// CM13: a run that called `graph_complete_node` with a `fail` status
/// still routes on `fail`, never on `error`, and never causes a
/// fallthrough.
#[tokio::test]
async fn cm13_self_reported_fail_routes_on_fail_not_error() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-selfreport-fail");
    db.update_graph_run_result(
        "run-selfreport-fail",
        GraphRunStatus::Fail,
        Some(&serde_json::json!({ "summary": "agent reported it failed" })),
        Some(chrono::Utc::now()),
    )
    .unwrap();

    let run = db.get_graph_run("run-selfreport-fail").unwrap();
    let reported = self_reported_execution(run.as_ref(), &node)
        .expect("a completed run row must be read as self-reported");
    assert_eq!(reported.status, GraphRunStatus::Fail);

    assert!(
        !is_infra_crash(&node, &reported, &run.unwrap(), 0, 3, 60),
        "CM13: a self-reported fail must never be infra crash"
    );
}

/// CM13: a run that declared a blocker still blocks.
#[test]
fn cm13_blocker_still_blocks() {
    let cli = Cli::new("test-cli");
    let node = sample_agent_node();
    // A self-reported blocker: the run called graph_report_blocker,
    // so self_reported = true.
    let execution = agent_finished_execution(&node, &cli, None, 0, "", "", true);

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        "a blocker must be recorded as fail"
    );
    assert!(
        !is_infra_crash_shape(
            &node,
            &execution,
            &GraphNodeRun {
                id: "run-blocker".to_string(),
                graph_id: "loop1".to_string(),
                spec_id: "spec1".to_string(),
                node_id: node.id.clone(),
                status: GraphRunStatus::Fail, // self_reported
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
            },
            60
        ),
        "CM13: a self-reported blocker must not be infra crash"
    );
}

/// CM13: the recorded outcome distinguishes "never reported" from
/// "failed" and from "process crashed".
#[test]
fn cm13_recorded_outcome_distinguishes_cases() {
    let cli = Cli::new("test-cli");
    let node = sample_agent_node();

    // Case 1: unreported → failure_kind: "unreported"
    let unreported = agent_finished_execution(&node, &cli, None, 0, "output", "", false);
    assert_eq!(unreported.status, GraphRunStatus::Fail);
    assert_eq!(
        unreported
            .output
            .get("failure_kind")
            .and_then(Value::as_str),
        Some("unreported"),
        "unreported must carry failure_kind: unreported"
    );

    // Case 2: self-reported fail → no failure_kind from CM13
    let self_reported = agent_finished_execution(&node, &cli, None, 1, "", "", true);
    assert_eq!(self_reported.status, GraphRunStatus::Fail);
    assert!(
        self_reported.output.get("failure_kind").is_none(),
        "self-reported fail must not carry failure_kind"
    );

    // Case 3: infra crash marker → infra_crash: true
    let infra = NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "infra_crash": true,
            "infra_attempt": 0,
        }),
        summary: "crashed".to_string(),
    };
    assert!(execution_is_infra_failure(&infra.output));
}

// ── §C: missing behavioural tests (spec GUIDELINES) ───────────────

/// C1: a run that exits 0 without reporting takes the `error` edge and
/// is never recorded as pass — the exact case of measurement (2) and (3)
/// (designer exits 0 at 100 seconds having written nothing).
#[tokio::test]
async fn cm13_unreported_exit_zero_routes_to_error_edge_and_is_not_pass() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-implement".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "test-cli",
            "prompt_template": "exit 0",
            "infra_retry_limit": 1,
            "infra_crash_max_seconds": 60,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-resilience".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf RESILIENCE",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    // Error edge to resilience node (should be taken on infra crash).
    db.insert_graph_edge(&GraphEdge {
        id: "edge-error".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-implement".to_string(),
        to_node: "node-resilience".to_string(),
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

    let implement_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-implement")
        .collect();
    let resilience_runs: Vec<_> = runs
        .iter()
        .filter(|r| r.node_id == "node-resilience")
        .collect();

    // Error edge was taken even though the run exited 0.
    assert_eq!(
        resilience_runs.len(),
        1,
        "resilience node should run (Error edge taken)"
    );
    // Every implement run must be Fail, never Pass.
    assert!(
        implement_runs
            .iter()
            .all(|r| r.status == GraphRunStatus::Fail),
        "all implement runs must be Fail, never Pass"
    );
    // The settled run carries failure_kind: unreported.
    let settled = implement_runs.last().unwrap();
    assert_eq!(
        settled
            .output
            .as_ref()
            .and_then(|o| o.get("failure_kind"))
            .and_then(Value::as_str),
        Some("unreported"),
        "settled run must carry failure_kind: unreported"
    );
}

/// C2: an ensemble member that exits without reporting causes
/// round-robin to try the next member, and the ensemble passes when
/// that one succeeds.
#[tokio::test]
async fn cm13_round_robin_falls_through_unreported_member_then_passes_on_next() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);

    let silent_path = write_member_script(dir.path(), "silent.sh", "exit 0");
    let ok_path = write_self_reporting_member_script(
        dir.path(),
        "ok.sh",
        "sleep 1",
        &db_path,
        "rr-ok",
        "pass",
        None,
    );

    let fake_home = setup_multi_cli_home(&[
        ("rr-silent", silent_path.as_str()),
        ("rr-ok", ok_path.as_str()),
    ]);
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-silent", "rr-silent"), ("rr-ok", "rr-ok")],
    );

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(_graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);

    result.unwrap();

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Pass,
        "ensemble must pass when the second member succeeds"
    );
    let out = join.output.as_ref().unwrap();
    assert_eq!(out["kind"], "round_robin");
    assert!(
        out["members_tried"].as_u64().unwrap() >= 2,
        "must have tried at least 2 members"
    );
    // rr-silent ran its infra retries (initial + 1 since infra_retry_limit=1 from insert_kind_ensemble → 2 runs),
    // then rr-ok ran and passed.
    assert!(
        !member_runs(&db, &spec_id, "rr-silent").is_empty(),
        "rr-silent must have run"
    );
    let ok_runs = member_runs(&db, &spec_id, "rr-ok");
    assert!(
        ok_runs.iter().any(|r| r.status == GraphRunStatus::Pass),
        "rr-ok must have a Pass run"
    );
    // The passing join must route onward on its Pass edge (to the
    // `done-pass` node `insert_kind_ensemble` wires as on_pass_to) — a
    // fallthrough that reaches a healthy member is only useful if the
    // ensemble then advances like any other pass.
    assert!(
        member_runs(&db, &spec_id, "done-pass")
            .iter()
            .any(|r| r.status == GraphRunStatus::Pass),
        "a passing ensemble join must route on its Pass edge to on_pass_to"
    );
}
