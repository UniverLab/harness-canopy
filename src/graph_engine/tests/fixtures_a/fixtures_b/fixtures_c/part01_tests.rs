//! Part 01 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

#[test]
fn shell_quote_handles_required_values() {
    assert_eq!(shell_quote("plain"), "'plain'");
    assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    assert_eq!(shell_quote("'"), "''\\'''");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    assert_eq!(shell_quote("a`b`c"), "'a`b`c'");
}

#[test]
fn command_hook_quotes_but_prompt_hook_stays_literal() {
    let completed_specs = Vec::new();
    let ctx = HookContext {
        graph_name: "Graph",
        workdir: "/tmp",
        completed_specs: &completed_specs,
        spec_name: Some("a'b"),
        spec_id: Some("spec-id"),
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };

    assert_eq!(
        render_hook_prompt(&GraphHookEvent::OnSpecCompleted, &ctx, "{{spec_name}}").unwrap(),
        "a'b"
    );
    assert_eq!(
        render_hook_command(&GraphHookEvent::OnSpecCompleted, &ctx, "{{spec_name}}").unwrap(),
        "'a'\\''b'"
    );
}

#[tokio::test]
async fn on_completed_hook_edit_mid_run_fires_the_new_hook() {
    use crate::domain::graphs::GraphCompletionHook;
    let (dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let v1_file = dir.path().join("hook_v1");
    let v2_file = dir.path().join("hook_v2");

    let hook_v1 = GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!("printf v1 > \"{}\"", v1_file.display())),
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
    hooks.insert(GraphHookEvent::OnCompleted, vec![hook_v1]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    // The run context would have captured this graph (holding hook_v1).
    let stale_lp = db.get_graph(&graph_id).unwrap().unwrap();

    let hook_v2 = GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!("printf v2 > \"{}\"", v2_file.display())),
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
    hooks.insert(GraphHookEvent::OnCompleted, vec![hook_v2.clone()]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let completed = vec![("Spec".to_string(), "ok".to_string())];
    let ctx = HookContext {
        graph_name: &stale_lp.name,
        workdir: &stale_lp.workdir,
        completed_specs: &completed,
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    engine
        .fire_hooks(&stale_lp, GraphHookEvent::OnCompleted, &ctx)
        .await;

    assert!(v2_file.exists(), "the edited hook must fire");
    assert!(!v1_file.exists(), "the captured hook must NOT fire");
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].hook_config.as_ref(), Some(&hook_v2));
}

#[tokio::test]
async fn on_spec_completed_hook_edit_between_specs_fires_new_version() {
    use crate::domain::graphs::GraphCompletionHook;
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let v1_file = dir.path().join("spec_v1");
    let v2_file = dir.path().join("spec_v2");

    let v1 = GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!("printf v1 > \"{}\"", v1_file.display())),
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
    hooks.insert(GraphHookEvent::OnSpecCompleted, vec![v1]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();
    let stale_lp = db.get_graph(&graph_id).unwrap().unwrap();

    engine.fire_on_spec_completed_hooks(&stale_lp, &spec).await;
    assert!(v1_file.exists());

    let v2 = GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some(format!("printf v2 > \"{}\"", v2_file.display())),
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
    hooks.insert(GraphHookEvent::OnSpecCompleted, vec![v2.clone()]);
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine.fire_on_spec_completed_hooks(&stale_lp, &spec).await;
    assert!(v2_file.exists(), "second spec must fire the edited hook");
    assert_eq!(std::fs::read_to_string(&v2_file).unwrap(), "v2");
    let runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[1].hook_config.as_ref(), Some(&v2));
}

#[tokio::test]
async fn graph_engine_captures_spec_start_head_for_git_workdir() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let expected_head = git_head(dir.path());

    db.insert_graph_node(&GraphNode {
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
    })
    .unwrap();

    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(
        spec.spec_start_head.as_deref(),
        Some(expected_head.as_str())
    );
}

#[tokio::test]
async fn graph_engine_substitutes_spec_start_head_in_check_command() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "test \"$(git rev-parse HEAD)\" = \"{{spec_start_head}}\"",
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
    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
}

#[tokio::test]
async fn graph_engine_substitutes_empty_spec_start_head_for_non_git_workdir() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "test -z \"{{spec_start_head}}\"",
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
    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(spec.spec_start_head, None);
}

// ── CM38: check-command marker quoting, refusal, and env vars ──────

/// CM38 FR1: `{{spec_name}}` expands through the CB48 shell quoting, so
/// a name containing a space compares equal and cannot break the command.
#[tokio::test]
async fn check_command_quotes_spec_name_with_spaces() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let mut spec = standalone_spec("spec-spaces", 1);
    spec.name = "a b".to_string();
    spec.graph_id = Some(graph_id.clone());
    db.insert_graph_spec(&spec).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "test {{spec_name}} = 'a b'",
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
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
}

/// CM38 FR1: a spec named `x'; touch <p>; '` cannot inject into the
/// command — the quoted expansion round-trips the name byte for byte and
/// the probed file is never created.
#[tokio::test]
async fn check_command_injection_shaped_spec_name_is_neutralized() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let probed = dir.path().join("p");
    let mut spec = standalone_spec("spec-inject", 1);
    spec.name = format!("x'; touch {}; '", probed.display());
    spec.graph_id = Some(graph_id.clone());
    db.insert_graph_spec(&spec).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf %s {{spec_name}} > name.txt",
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
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
    assert!(!probed.exists(), "the injection must not have executed");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("name.txt")).unwrap(),
        spec.name,
        "the quoted expansion must round-trip the name exactly"
    );
}

/// CM38 FR3: an unknown marker fails the node before anything reaches
/// the shell — the run row records the exact message and no side effect
/// happens.
#[tokio::test]
async fn check_command_unknown_marker_fails_node_without_running() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let mut spec = standalone_spec("spec-marker", 1);
    spec.graph_id = Some(graph_id.clone());
    db.insert_graph_spec(&spec).unwrap();

    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "echo {{nope}} && printf RAN > ran.txt",
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

    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].status, GraphRunStatus::Fail);
    let output = runs[0].output.as_ref().expect("run row must carry output");
    assert_eq!(
        output.get("error").and_then(serde_json::Value::as_str),
        Some("unknown marker {{nope}} in check command")
    );
    assert!(
        !dir.path().join("ran.txt").exists(),
        "nothing may reach the shell"
    );
    assert_ne!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
}

/// CM38 FR6, bound-spec run: every CANOPY_* variable is set, the spec
/// file holds the spec's description, and the file is gone once the node
/// has finished.
#[tokio::test]
async fn check_command_env_vars_and_spec_file_on_bound_spec_run() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let mut spec = standalone_spec("bound-spec", 1);
    spec.name = "Bound Spec".to_string();
    spec.graph_id = Some(graph_id.clone());
    db.insert_graph_spec(&spec).unwrap();

    let command = concat!(
        r#"test -s "$CANOPY_SPEC_FILE""#,
        r#" && grep -q Functional "$CANOPY_SPEC_FILE""#,
        r#" && test "$CANOPY_SPEC_NAME" = "Bound Spec""#,
        r#" && test "$CANOPY_SPEC_ID" = bound-spec"#,
        r#" && test "$CANOPY_GRAPH_NAME" = Graph"#,
        r#" && test "$CANOPY_GRAPH_ID" = wf-test"#,
        r#" && test -z "$CANOPY_SPEC_START_HEAD""#,
        r#" && printf %s "$CANOPY_SPEC_FILE" > spec_file_path.txt"#,
    );
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": command,
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
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );

    let recorded = std::fs::read_to_string(dir.path().join("spec_file_path.txt")).unwrap();
    assert!(
        !std::path::Path::new(&recorded).exists(),
        "the spec file must be removed after the node finishes (was at {recorded})"
    );
}

/// CM38 FR6, queue run: the same contract holds when the spec arrives
/// through a run queue instead of being pre-bound.
#[tokio::test]
async fn check_command_env_vars_and_spec_file_on_queue_run() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let spec = standalone_spec("queue-spec", 1);
    db.insert_graph_spec(&spec).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&spec.id]);

    let command = concat!(
        r#"test -s "$CANOPY_SPEC_FILE""#,
        r#" && grep -q Functional "$CANOPY_SPEC_FILE""#,
        r#" && test "$CANOPY_SPEC_NAME" = queue-spec"#,
        r#" && test "$CANOPY_SPEC_ID" = queue-spec"#,
        r#" && printf %s "$CANOPY_SPEC_FILE" > spec_file_path.txt"#,
    );
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": command,
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
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );

    let recorded = std::fs::read_to_string(dir.path().join("spec_file_path.txt")).unwrap();
    assert!(!std::path::Path::new(&recorded).exists());
}

/// CM38 FR6, idea run: `CANOPY_SPEC_NAME` is empty and
/// `CANOPY_SPEC_FILE` holds the idea text.
#[tokio::test]
async fn check_command_env_vars_on_idea_run_point_at_idea_text() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let command = concat!(
        r#"test -z "$CANOPY_SPEC_NAME""#,
        r#" && test -s "$CANOPY_SPEC_FILE""#,
        r#" && grep -q "CM38 IDEA" "$CANOPY_SPEC_FILE""#,
        r#" && printf APPROVED"#,
    );
    db.insert_graph_node(&GraphNode {
        id: "graph-check".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": command,
            "success_condition": "exit_code_0_and_output_contains:APPROVED"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("A CM38 IDEA: check commands must be able to read the idea text.".to_string()),
            None,
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Completed
    );
}

// ── CM29: spec-end dirty-tree capture and dirty-start refusal ────────

#[tokio::test]
async fn run_spec_records_spec_end_dirty_on_completed() {
    let (_fixture_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());
    let workdir = git_dir.path().to_string_lossy().to_string();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id, None, Some(workdir), None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.spec_end_dirty, Some(0));
}

#[tokio::test]
async fn run_spec_records_spec_end_dirty_and_paths_on_failed() {
    let (_fixture_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());
    let workdir = git_dir.path().to_string_lossy().to_string();
    std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();
    std::fs::write(git_dir.path().join("c.txt"), "c").unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "false",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    engine
        .run_graph(graph_id, None, Some(workdir), None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.spec_end_dirty, Some(3));
    let paths = spec.spec_end_dirty_paths.unwrap();
    assert_eq!(paths.len(), 3);
    assert!(paths.contains(&"a.txt".to_string()));
    assert!(paths.contains(&"b.txt".to_string()));
    assert!(paths.contains(&"c.txt".to_string()));

    let entries = db.list_recent_activity_log_entries(50).unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e.message.contains("worktree left dirty")),
        "expected an activity entry with the dirty-tree note: {entries:?}"
    );
}

#[tokio::test]
async fn run_spec_end_dirty_none_for_non_git_workdir() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0"
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
    assert_eq!(spec.spec_end_dirty, None);
}

#[tokio::test]
async fn dirty_start_check_refusal_names_blocking_predecessor_when_next_resumes_own_dirt() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    // G1: a graph on the same git workdir, carrying spec A.
    db.insert_graph(&Graph {
        id: "g1".to_string(),
        name: "Graph1".to_string(),
        description: None,
        workdir: workdir.clone(),
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
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
    })
    .unwrap();

    let now = chrono::Utc::now().timestamp();
    let t1 = chrono::DateTime::from_timestamp(now - 3600, 0).unwrap();
    let t2 = chrono::DateTime::from_timestamp(now - 300, 0).unwrap();

    // Spec A: Failed in G1, completed_at T1, end_dirty 3, run row in G1.
    let mut spec_a =
        dirty_predecessor_spec_named("spec-a", "Alpha", &workdir, GraphSpecStatus::Failed);
    spec_a.graph_id = Some("g1".to_string());
    spec_a.workdir = None;
    db.insert_graph_spec(&spec_a).unwrap();
    db.update_graph_spec_status("spec-a", GraphSpecStatus::Failed, Some(t1), Some(t1))
        .unwrap();
    db.set_graph_spec_end_state(
        "spec-a",
        Some(3),
        &["a".to_string(), "b".to_string(), "c".to_string()],
    )
    .unwrap();
    insert_run_row(&db, "g1", "spec-a", "node-a", 1, t1.timestamp()).unwrap();

    // Spec B: Failed in G2 (fixture graph), completed_at T2, end_dirty 2,
    // run row in G2 — it is the next spec and resumes its own dirt.
    let mut spec_b =
        dirty_predecessor_spec_named("spec-b", "Beta", &workdir, GraphSpecStatus::Failed);
    spec_b.graph_id = Some(graph_id.clone());
    spec_b.workdir = None;
    spec_b.position = 2;
    db.insert_graph_spec(&spec_b).unwrap();
    db.update_graph_spec_status("spec-b", GraphSpecStatus::Failed, Some(t2), Some(t2))
        .unwrap();
    db.set_graph_spec_end_state("spec-b", Some(2), &["b".to_string()])
        .unwrap();
    insert_run_row(&db, &graph_id, "spec-b", "node-b", 2, t2.timestamp()).unwrap();

    // Workdir dirty (1 file).
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");

    assert!(notice.refuse);
    // FR1: the blocking predecessor is named.
    assert!(
        notice.message.contains("but spec 'Alpha'"),
        "message must name the blocking predecessor, got: {}",
        notice.message
    );
    assert!(
        notice.message.contains("g1"),
        "message must name the predecessor's graph, got: {}",
        notice.message
    );
    assert!(
        notice.message.contains("failed"),
        "message must name the predecessor's end status, got: {}",
        notice.message
    );
    assert!(
        notice.message.contains(&t1.to_rfc3339()),
        "message must name the predecessor's end time, got: {}",
        notice.message
    );
    // FR2: the next spec is also resuming its own dirt — both named.
    assert!(
        notice.message.contains("Beta"),
        "message must name the resuming next spec, got: {}",
        notice.message
    );
    assert!(
        notice.message.contains("is resuming its own dirty work"),
        "message must state the resume, got: {}",
        notice.message
    );
    assert!(
        notice.message.contains("not launched"),
        "refusal must say not launched: {}",
        notice.message
    );
    assert!(
        notice.message.contains("Set allow_dirty_start"),
        "refusal must carry the flag hint: {}",
        notice.message
    );
}

#[tokio::test]
async fn dirty_start_check_resume_of_only_own_dirt_still_proceeds() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();

    // Only spec B is dirty; no predecessor A exists.
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b",
        "Beta",
        &workdir,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-b", Some(2), &["b".to_string()])
        .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap()
        .expect("resuming its own dirt must produce a notice");
    assert!(!notice.refuse);
    assert!(
        notice.message.contains("resuming its own work"),
        "message must state the resume reason: {}",
        notice.message
    );
    assert!(
        notice.message.contains("Beta"),
        "the resuming spec must be named: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("Set allow_dirty_start"),
        "must not tell the user to set a flag: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("allow_dirty_start is set"),
        "the flag reason must not appear: {}",
        notice.message
    );
}

#[tokio::test]
async fn dirty_start_check_refuses_when_predecessor_failed_dirty() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    // The dirty-leaving spec is not this graph's — no run of `graph_id`
    // recorded the dirt — so there is no attribution; the refusal still
    // names the blocking predecessor from `find_dirty_predecessor_spec`.
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();
    std::fs::write(dir.path().join("y.txt"), "y").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(notice.refuse);
    assert!(
        notice.message.contains("Predecessor"),
        "the blocking predecessor must be named: {}",
        notice.message
    );
    assert!(
        notice.message.contains("standalone"),
        "the predecessor's graph id must be named: {}",
        notice.message
    );
    assert!(
        notice.message.contains("failed"),
        "the predecessor's end status must be named: {}",
        notice.message
    );
    assert!(notice.message.contains("not launched"));
    assert!(notice.message.contains("Set allow_dirty_start"));
    assert!(notice.message.contains('2'));
}

#[tokio::test]
async fn dirty_start_check_names_the_blocking_predecessor_not_the_attribution() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-a-old",
        "OldPredecessor",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b-new",
        "RecentDirtier",
        &workdir,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-a-old", Some(5), &["a".to_string()])
        .unwrap();
    db.set_graph_spec_end_state("spec-b-new", Some(2), &["b".to_string()])
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    insert_run_row(&db, &graph_id, "spec-a-old", "node-a", 1, now - 3600).unwrap();
    insert_run_row(&db, &graph_id, "spec-b-new", "node-b", 2, now - 60).unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();
    std::fs::write(dir.path().join("y.txt"), "y").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b-new"), false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(notice.refuse);
    assert!(
        notice.message.contains("but spec 'OldPredecessor'"),
        "the blocking predecessor must be named as the blocker: {}",
        notice.message
    );
    assert!(
        notice.message.contains("OldPredecessor"),
        "the blocking predecessor must be named: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("RecentDirtier left it dirty"),
        "the attribution sentence must be gone: {}",
        notice.message
    );
    assert!(
        notice.message.contains("standalone"),
        "the predecessor's graph id must be named: {}",
        notice.message
    );
    assert!(
        notice.message.contains("failed"),
        "the predecessor's end status must be named: {}",
        notice.message
    );
}

#[tokio::test]
async fn dirty_start_check_refusal_says_not_launched() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-a-old",
        "OldPredecessor",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b-new",
        "RecentDirtier",
        &workdir,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-a-old", Some(5), &["a".to_string()])
        .unwrap();
    db.set_graph_spec_end_state("spec-b-new", Some(2), &["b".to_string()])
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    insert_run_row(&db, &graph_id, "spec-a-old", "node-a", 1, now - 3600).unwrap();
    insert_run_row(&db, &graph_id, "spec-b-new", "node-b", 2, now - 60).unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();
    std::fs::write(dir.path().join("y.txt"), "y").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b-new"), false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(notice.refuse);
    assert!(
        notice.message.contains("not launched"),
        "refusal must say not launched: {}",
        notice.message
    );
    assert!(
        notice.message.contains("Set allow_dirty_start"),
        "refusal must carry the flag hint: {}",
        notice.message
    );
    assert!(
        notice.message.contains("OldPredecessor"),
        "the blocking predecessor must be named: {}",
        notice.message
    );
    assert!(
        notice.message.contains("but spec 'OldPredecessor'"),
        "OldPredecessor must be the blocker: {}",
        notice.message
    );
}

#[tokio::test]
async fn dirty_start_check_allow_dirty_start_states_reason_without_flag_hint() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-a-old",
        "OldPredecessor",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b-new",
        "RecentDirtier",
        &workdir,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-a-old", Some(5), &["a".to_string()])
        .unwrap();
    db.set_graph_spec_end_state("spec-b-new", Some(2), &["b".to_string()])
        .unwrap();
    let now = chrono::Utc::now().timestamp();
    insert_run_row(&db, &graph_id, "spec-a-old", "node-a", 1, now - 3600).unwrap();
    insert_run_row(&db, &graph_id, "spec-b-new", "node-b", 2, now - 60).unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();
    std::fs::write(dir.path().join("y.txt"), "y").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b-new"), true)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(!notice.refuse);
    assert!(
        notice.message.contains("allow_dirty_start is set"),
        "proceeded launch must state the flag reason: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("Set allow_dirty_start"),
        "must not tell the user to set an already-set flag: {}",
        notice.message
    );
    assert!(
        notice.message.contains("RecentDirtier"),
        "the spec whose run last left the dirt must be named: {}",
        notice.message
    );
}

/// CB69/FR2: when the launch proceeds because `allow_dirty_start` is
/// set, the message gives that reason and never tells the reader to
/// set the flag.
#[tokio::test]
async fn dirty_start_check_warns_instead_of_refusing_with_allow_dirty_start() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();
    std::fs::write(dir.path().join("y.txt"), "y").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, true)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(!notice.refuse);
    assert!(notice.message.contains("allow_dirty_start is set"));
    assert!(!notice.message.contains("Set allow_dirty_start"));
}

#[tokio::test]
async fn dirty_start_check_ignores_clean_workdir() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, false)
        .await
        .unwrap();
    assert!(notice.is_none());
}

#[tokio::test]
async fn dirty_start_check_ignores_non_git_workdir() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, false)
        .await
        .unwrap();
    assert!(notice.is_none());
}

/// CB69: the next spec is excluded from the predecessor search so it is
/// never mistaken for its own bad predecessor (CM29 guideline). With no
/// run row and no recorded dirt, there is neither a predecessor nor an
/// attribution, so there is no notice at all — exactly the old behavior.
#[tokio::test]
async fn dirty_start_check_excludes_the_next_spec_itself() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec(
        "spec-b",
        &workdir,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b"), false)
        .await
        .unwrap();
    assert!(notice.is_none());
}

#[tokio::test]
async fn dirty_start_check_resume_of_own_dirt_states_resume_reason() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec_named(
        "spec-b-new",
        "RecentDirtier",
        &workdir,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.set_graph_spec_end_state("spec-b-new", Some(2), &["b".to_string()])
        .unwrap();
    insert_run_row(
        &db,
        &graph_id,
        "spec-b-new",
        "node-b",
        1,
        chrono::Utc::now().timestamp() - 60,
    )
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, Some("spec-b-new"), false)
        .await
        .unwrap()
        .expect("resuming its own dirt must produce a notice");
    assert!(!notice.refuse);
    assert!(
        notice.message.contains("resuming its own work"),
        "message must state the resume reason: {}",
        notice.message
    );
    assert!(
        notice.message.contains("RecentDirtier"),
        "the resuming spec must be named: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("Set allow_dirty_start"),
        "must not tell the user to set a flag: {}",
        notice.message
    );
    assert!(
        !notice.message.contains("allow_dirty_start is set"),
        "the flag reason must not appear: {}",
        notice.message
    );
}

#[tokio::test]
async fn dirty_start_check_attribution_empty_when_no_run_recorded_the_dirt() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, false)
        .await
        .unwrap()
        .expect("expected a dirty-start notice");
    assert!(notice.refuse);
    assert!(
        notice.message.contains("Predecessor"),
        "the blocking predecessor must be named: {}",
        notice.message
    );
    assert!(
        notice.message.contains("standalone"),
        "the predecessor's graph id must be named: {}",
        notice.message
    );
    assert!(
        notice.message.contains("failed"),
        "the predecessor's end status must be named: {}",
        notice.message
    );
}

#[tokio::test]
async fn dirty_start_check_ignores_a_completed_predecessor() {
    let (_dir, db, engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let dir = tempdir().unwrap();
    init_git_repo(dir.path());
    let workdir = dir.path().to_string_lossy().to_string();
    // Completed and never recorded end dirt → owns nothing.
    db.insert_graph_spec(&dirty_predecessor_spec(
        "pred-a",
        &workdir,
        GraphSpecStatus::Completed,
    ))
    .unwrap();
    std::fs::write(dir.path().join("x.txt"), "x").unwrap();

    let notice = engine
        .dirty_start_check(&graph_id, &workdir, None, false)
        .await
        .unwrap();
    assert!(notice.is_none());
}

#[tokio::test]
async fn node_without_commit_rights_that_commits_fails_and_routes_via_fail_edge() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    init_git_repo(dir.path());
    let head_before = git_head(dir.path());

    // `worker` exits 0 and reports success — but commits. `committer`
    // (never reached) is what makes this graph enforce commit rights.
    db.insert_graph_node(&rights_node(&spec_id, "worker", COMMIT_CMD, None, 1))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "committer", "true", Some(true), 2))
        .unwrap();
    db.insert_graph_node(&rights_node(&spec_id, "triage", "true", None, 3))
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "committer".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "e-fail".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "worker".to_string(),
        to_node: "triage".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let worker = runs.iter().find(|r| r.node_id == "worker").unwrap();
    assert_eq!(
        worker.status,
        GraphRunStatus::Fail,
        "committing without commit rights must be a deterministic fail"
    );
    let violation = worker
        .output
        .as_ref()
        .and_then(|o| o.get("commit_rights_violation"))
        .expect("the violation must be recorded on the run's output");
    assert_eq!(
        violation.get("head_before").and_then(|v| v.as_str()),
        Some(head_before.as_str())
    );
    assert_eq!(
        violation.get("head_after").and_then(|v| v.as_str()),
        Some(git_head(dir.path()).as_str())
    );
    assert!(
        violation
            .get("message")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.contains("no commit rights")),
        "the reason must be stated in plain words"
    );

    // Routed through the fail edge like any other failure — never
    // silently accepted, and never onward to the committer.
    assert!(
        runs.iter().any(|r| r.node_id == "triage"),
        "the fail edge must have been taken"
    );
    assert!(
        !runs.iter().any(|r| r.node_id == "committer"),
        "the pass edge must not have been taken"
    );

    // The unauthorized commit is reported, never rewritten away.
    assert_ne!(git_head(dir.path()), head_before);
}
