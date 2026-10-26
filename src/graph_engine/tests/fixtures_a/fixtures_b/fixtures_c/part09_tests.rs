//! Part 09 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// (CB22) A graph with a bound spec whose name and description are both
/// empty is not executable: `run_graph` rejects it before claiming the
/// graph, so no agent node ever executes. This is the measured 2026-09-01
/// incident shape (empty name, `None` description, bound to the graph).
#[tokio::test]
async fn blank_bound_spec_is_rejected_before_any_agent_executes() {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let argv_file = dir.path().join("argv.log");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    let cli = argv_cli_config(&script, env, None, None, None);
    let home = write_resume_cli_home(cli);

    db.insert_graph_spec(&GraphSpec {
        id: "blank-spec".to_string(),
        graph_id: Some(graph_id.clone()),
        name: String::new(),
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
    })
    .unwrap();
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

    let guard = HomeGuard::set(home.path());
    let error = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap_err();
    drop(guard);

    let message = error.to_string();
    assert!(
        message.contains("blank-spec"),
        "the refusal must name the offending spec: {message}"
    );
    assert!(
        message.contains("both name and description are empty"),
        "the refusal must say what is missing: {message}"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Draft,
        "a refused launch must leave the graph's status untouched"
    );
    assert!(
        db.list_graph_runs_for_graph(&graph_id).unwrap().is_empty(),
        "a refused launch must record no run"
    );
    assert!(
        std::fs::read_to_string(&argv_file)
            .unwrap_or_default()
            .is_empty(),
        "no agent node may execute for a blank spec"
    );
}

/// (CB22) A whitespace-only name/description is blank too: trimming
/// applies before the content check.
#[tokio::test]
async fn whitespace_only_bound_spec_is_rejected() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "ws-spec".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "   ".to_string(),
        description: Some("  \n\t ".to_string()),
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
    })
    .unwrap();

    let error = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("ws-spec"),
        "the refusal must name the offending spec: {error}"
    );
}

/// A description-only bound spec has usable content and remains
/// executable even though the interactive spec-creation flow normally
/// requires a name.
#[tokio::test]
async fn description_only_bound_spec_is_executable() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "description-only".to_string(),
        graph_id: Some(graph_id.clone()),
        name: String::new(),
        description: Some("Run the verification checks.".to_string()),
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
        completed_via_at: None,
        completed_via_reason: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "description-only-check".to_string(),
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

    let spec = db.get_graph_spec("description-only").unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Completed);
}

/// (CB22) Queue isolation: a valid queue run succeeds even when the graph
/// has an unrelated blank bound spec — queue launches validate only the
/// queue's own members.
#[tokio::test]
async fn queue_run_ignores_unrelated_blank_bound_spec() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "blank-bound".to_string(),
        graph_id: Some(graph_id.clone()),
        name: String::new(),
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
    })
    .unwrap();

    let member = standalone_spec("queue-member", 1);
    db.insert_graph_spec(&member).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&member.id]);

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
}

/// (CB22) A blank queue member is rejected at launch: the queue path
/// validates the members it actually selects.
#[tokio::test]
async fn blank_queue_member_is_rejected() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "blank-member".to_string(),
        graph_id: None,
        name: String::new(),
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
    })
    .unwrap();
    insert_queue_with_members(&db, "queue-1", &["blank-member"]);

    let error = engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("blank-member"),
        "the refusal must name the offending queue member: {error}"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Draft,
        "a refused queue launch must leave the graph's status untouched"
    );
}

/// (CB22) Explicit non-empty `idea` still drives a zero-bound-spec graph:
/// the engine binds an internal bookkeeping row carrying the idea as
/// `description`, runs the top-level graph once, and a second idea
/// launch purges the prior terminal bookkeeping row so the new idea
/// actually executes (not a silent zero-exec completion).
#[tokio::test]
async fn explicit_idea_run_executes_graph_and_rerun_picks_up_new_idea() {
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
    // CM13: linger so the script's own self-report (below) has time to
    // land before the process exits.
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
    // CB57: the script self-reports its Pass verdict so the idea
    // dispatches complete, across both dispatches below.
    engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("build a landing page".to_string()),
            None,
            None,
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(specs.len(), 1, "idea run leaves one bookkeeping row");
    assert!(
        specs[0].name.is_empty(),
        "bookkeeping row keeps a blank name"
    );
    assert_eq!(
        specs[0].description.as_deref(),
        Some("build a landing page")
    );
    let first_placeholder_id = specs[0].id.clone();
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains("build a landing page"),
        "the agent prompt must carry the idea as spec_content: {argv}"
    );
    assert_eq!(
        argv.matches("===").count(),
        1,
        "the agent node must run exactly once on the first idea dispatch"
    );

    // A second idea launch: `claim_graph_for_run` only refuses while
    // `Running`, so the completed status from the first run doesn't
    // block this. The prior terminal bookkeeping row must be purged
    // and replaced so the new idea actually executes.
    engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("ship the docs site".to_string()),
            None,
            None,
        )
        .await
        .unwrap();
    drop(guard);

    let specs_after = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(specs_after.len(), 1);
    assert_ne!(
        specs_after[0].id, first_placeholder_id,
        "the second idea dispatch must not reuse the first bookkeeping row"
    );
    assert_eq!(
        specs_after[0].description.as_deref(),
        Some("ship the docs site")
    );
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains("ship the docs site"),
        "the second idea must reach the agent: {argv}"
    );
    assert_eq!(
        argv.matches("===").count(),
        2,
        "the agent node must run again on the second idea dispatch: {argv}"
    );
}

/// (CB22) A leftover *completed* blank bookkeeping/legacy row must not
/// block a launch that still has a real pending bound spec — only
/// non-terminal blank content is a launch error.
#[tokio::test]
async fn completed_blank_bookkeeping_does_not_block_real_bound_spec() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "legacy-blank".to_string(),
        graph_id: Some(graph_id.clone()),
        name: String::new(),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Completed,
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
    db.insert_graph_spec(&GraphSpec {
            id: "real-spec".to_string(),
            graph_id: Some(graph_id.clone()),
            name: "Real Work".to_string(),
            description: Some(
                "Functional Requirements:\n- A\n\nNon-Functional Requirements:\n- B\n\nObjective:\n- C\n\nConstraints:\n- D\n\nGuidelines:\n- E\n\nIn Scope:\n- F\n\nOut of Scope:\n- G".to_string(),
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
        })
        .unwrap();
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
    assert_eq!(lp.status, GraphStatus::Completed);
    // Legacy blank bookkeeping is purged once terminal at the start of
    // the next bound-spec dispatch.
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert!(
        specs.iter().all(|s| s.id != "legacy-blank"),
        "terminal blank bookkeeping must be purged: {specs:?}"
    );
}

/// (CB22) A graph whose only bound row is terminal blank-name bookkeeping
/// (no real named specs, no idea) must refuse *before* claim — leaving
/// status `Draft` — rather than claim, purge, and fail after the fact.
#[tokio::test]
async fn only_terminal_blank_bookkeeping_is_refused_before_claim() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "stale-bookkeeping".to_string(),
        graph_id: Some(graph_id.clone()),
        name: String::new(),
        description: Some("prior idea text".to_string()),
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Completed,
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
    db.insert_graph_node(&GraphNode {
        id: "graph-only".to_string(),
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

    let error = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no specs to run"),
        "only bookkeeping left must look empty: {error}"
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Draft,
        "refusal must happen before claim so status stays Draft"
    );
    assert!(
        db.list_graph_runs_for_graph(&graph_id).unwrap().is_empty(),
        "a pre-claim refusal must record no run"
    );
}

/// (CB22) A whitespace-only `idea` does not unlock a zero-bound-spec
/// launch even when a top-level graph exists.
#[tokio::test]
async fn whitespace_only_idea_is_rejected_even_with_graph() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "graph-only".to_string(),
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

    let error = engine
        .run_graph(
            graph_id.clone(),
            None,
            None,
            Some("   \n\t  ".to_string()),
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("no specs to run"),
        "whitespace idea must not count as content: {error}"
    );
    assert_eq!(
        db.get_graph(&graph_id).unwrap().unwrap().status,
        GraphStatus::Draft
    );
}

/// (Requirement 3) When the graph's last run was queue-driven and a fresh
/// `graph_run` arrives without `queue_id` and finds zero bound specs, the
/// error must name the last queue so a recovery agent can retry
/// correctly instead of silently discarding the queue context.
#[tokio::test]
async fn graph_engine_queue_less_relaunch_after_queue_run_names_last_queue() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    // Simulate the incident: a queue-driven run left interrupted (daemon
    // crash, quota failure) — `active_run_queue_id` stays persisted
    // (it's only ever cleared on a *genuine* completion) with pending
    // queue members still queued behind it.
    let pending = standalone_spec("queue-pending", 1);
    db.insert_graph_spec(&pending).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&pending.id]);
    db.set_graph_active_run_queue(&graph_id, Some("queue-1"))
        .unwrap();

    // The recovery agent's mistake: relaunch directly (the graph's own
    // bound specs are still empty — every spec lives in the queue)
    // without passing `queue_id` back.
    let error = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("queue-1"),
        "error must name the last queue so a recovery agent can retry correctly: {error}"
    );

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Draft,
        "the failed queue-less relaunch must not touch the graph's status"
    );
    assert_eq!(
        lp.active_run_queue_id.as_deref(),
        Some("queue-1"),
        "the queue context must not be silently discarded by the failed relaunch"
    );
}

/// (Requirement 4b) A queue run where every member is already completed
/// must be treated as the same empty-set error, not a fresh completed
/// run — a queue is shared/reusable, so "nothing pending" is far more
/// likely a stale/incorrect queue_id than a genuine finish.
#[tokio::test]
async fn graph_engine_queue_run_with_all_members_completed_is_a_launch_error() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();

    let mut done = standalone_spec("queue-done", 1);
    done.status = GraphSpecStatus::Completed;
    db.insert_graph_spec(&done).unwrap();
    insert_queue_with_members(&db, "queue-1", &[&done.id]);

    let error = engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
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
        "an empty queue launch must leave the graph's status untouched"
    );
}

/// (Requirement 4c) A normal queue run — real pending members — still
/// completes and fires the `on_completed` hook exactly once; the B17
/// guard must not interfere with a genuine completion.
#[tokio::test]
async fn graph_engine_normal_queue_run_still_completes_and_fires_hook_once() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let marker = dir.path().join("hook_fired.marker");

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

    let marker_path = marker.to_string_lossy().to_string();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("touch \"{}\"", marker_path)),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    db.update_graph_completion_hook(&graph_id, Some(&hook))
        .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    assert!(marker.exists(), "on_completed hook must have run");

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1, "hook must fire exactly once");
}

/// A graph whose bound specs are non-empty but were *all* already
/// completed/skipped before this dispatch (e.g. the graph's last spec was
/// explicitly skipped via `graph_continue`) legitimately completes — the
/// B17 guard only fires on *zero bound specs*, not "zero pending" — but
/// must not fire the hook, since this dispatch executed nothing.
#[tokio::test]
async fn graph_engine_all_bound_specs_already_done_completes_without_firing_hook() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("hook_should_not_exist.marker");

    db.update_graph_spec_status(
        &spec_id,
        GraphSpecStatus::Skipped,
        None,
        Some(chrono::Utc::now()),
    )
    .unwrap();

    let marker_path = marker.to_string_lossy().to_string();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("touch \"{}\"", marker_path)),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    db.update_graph_completion_hook(&graph_id, Some(&hook))
        .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "a graph whose only bound spec is already skipped is genuinely done"
    );
    assert!(
        !marker.exists(),
        "on_completed must not fire for a dispatch that executed zero specs"
    );
    assert!(db
        .list_graph_completion_hook_runs(&graph_id)
        .unwrap()
        .is_empty());
}

/// Placeholder interpolation: `{{graph_name}}`, `{{workdir}}`,
/// `{{completed_specs}}` must all be substituted.
#[tokio::test]
async fn render_completion_hook_prompt_substitutes_all_placeholders() {
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf".to_string(),
        name: "MyGraph".to_string(),
        description: None,
        workdir: "/tmp/proj".to_string(),
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
    let completed_specs = vec![
        ("Spec-A".to_string(), "summary A".to_string()),
        ("Spec-B".to_string(), "summary B".to_string()),
    ];

    let result = render_completion_hook_prompt(
        &lp,
        &lp.workdir,
        &completed_specs,
        "Graph={{graph_name}} Workdir={{workdir}} Specs={{completed_specs}}",
    )
    .unwrap();

    assert_eq!(
        result,
        "Graph=MyGraph Workdir=/tmp/proj Specs=- Spec-A: summary A\n- Spec-B: summary B"
    );
}

/// Empty completed_specs list renders `(none)`.
#[tokio::test]
async fn render_completion_hook_prompt_empty_specs_shows_none() {
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf".to_string(),
        name: "Graph".to_string(),
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

    let result =
        render_completion_hook_prompt(&lp, &lp.workdir, &[], "{{completed_specs}}").unwrap();

    assert_eq!(result, "(none)");
}

// --- CH1: renderer validation tests for event-specific hooks ---

#[test]
fn render_hook_prompt_on_completed_binds_graph_name_workdir_completed_specs() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
        completed_specs: &[("SpecA".into(), "done".into())],
        spec_name: None,
        spec_id: None,
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result = render_hook_prompt(
        &GraphHookEvent::OnCompleted,
        &ctx,
        "{{graph_name}} {{workdir}} {{completed_specs}}",
    )
    .unwrap();
    assert_eq!(result, "TestGraph /tmp/test - SpecA: done");
}

#[test]
fn render_hook_prompt_on_failed_binds_blocker_and_node() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("quota exceeded"),
        node_name: Some("agent-1"),
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result =
        render_hook_prompt(&GraphHookEvent::OnFailed, &ctx, "{{blocker}} on {{node}}").unwrap();
    assert_eq!(result, "quota exceeded on agent-1");
}

#[test]
fn render_hook_prompt_on_blocked_binds_blocker() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("needs human review"),
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result =
        render_hook_prompt(&GraphHookEvent::OnBlocked, &ctx, "Blocked: {{blocker}}").unwrap();
    assert_eq!(result, "Blocked: needs human review");
}

#[test]
fn render_hook_prompt_missing_optional_values_use_none() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
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
    let result =
        render_hook_prompt(&GraphHookEvent::OnFailed, &ctx, "{{blocker}}/{{node}}").unwrap();
    assert_eq!(result, "(none)/(none)");
    assert!(!result.contains("{{"));
}

#[test]
fn render_hook_command_missing_node_uses_none() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
        completed_specs: &[],
        spec_name: None,
        spec_id: None,
        blocker: Some("graph failed"),
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result =
        render_hook_command(&GraphHookEvent::OnFailed, &ctx, "echo {{node}} {{blocker}}").unwrap();
    assert_eq!(result, "echo '(none)' 'graph failed'");
}

#[test]
fn render_hook_idea_missing_event_values_use_none() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
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
    let result = render_hook_idea(
        &GraphHookEvent::OnCompleted,
        &ctx,
        "{{spec_name}}/{{spec_id}}/{{blocker}}/{{node}}",
    )
    .unwrap();
    assert_eq!(result, "(none)/(none)/(none)/(none)");
    assert!(!result.contains("{{"));
}

#[test]
fn render_hook_prompt_on_spec_completed_binds_spec_name_and_id() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
        completed_specs: &[],
        spec_name: Some("Auth Spec"),
        spec_id: Some("spec-abc"),
        blocker: None,
        node_name: None,
        spec_code: None,
        spec_commit_message: None,
        spec_scope: None,
        spec_type: None,
    };
    let result = render_hook_prompt(
        &GraphHookEvent::OnSpecCompleted,
        &ctx,
        "Spec {{spec_name}} ({{spec_id}}) done",
    )
    .unwrap();
    assert_eq!(result, "Spec Auth Spec (spec-abc) done");
}

#[test]
fn render_hook_prompt_rejects_unbindable_marker() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
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
    // {{unknown}} is not supported by graph hooks.
    let result = render_hook_prompt(
        &GraphHookEvent::OnCompleted,
        &ctx,
        "{{graph_name}} {{unknown}}",
    );
    assert!(result.is_err());
}

#[test]
fn render_hook_prompt_cross_event_marker_is_bound_to_none() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
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
    let result = render_hook_prompt(
        &GraphHookEvent::OnFailed,
        &ctx,
        "{{graph_name}} {{completed_specs}}",
    );
    assert_eq!(result.unwrap(), "TestGraph (none)");
}

#[test]
fn render_hook_prompt_rejects_unknown_marker() {
    let ctx = HookContext {
        graph_name: "TestGraph",
        workdir: "/tmp/test",
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
    let result = render_hook_prompt(&GraphHookEvent::OnFailed, &ctx, "{{nodee}}");
    assert!(result.is_err());
}

/// A hook failure does not change the graph's already-final status — the
/// graph is `Completed` even though the hook exited non-zero.
#[tokio::test]
async fn graph_engine_hook_failure_does_not_alter_graph_status() {
    let fake_home = setup_test_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

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

    // Hook that always fails (exit 1).
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some("exit 1".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(1),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    db.update_graph_completion_hook(&graph_id, Some(&hook))
        .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Completed,
        "graph must stay Completed even when its on_completed hook fails"
    );

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Fail);
}

/// CH1: `on_spec_completed` fires once per bound spec completing, with
/// `{{spec_name}}` bound to that spec.
#[tokio::test]
async fn graph_engine_on_spec_completed_fires_once_with_spec_name() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
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

    // Marker path embeds {{spec_name}} so a passing run proves binding.
    let marker = dir.path().join("spec-Spec.marker");
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("touch \"{}\"", marker.display())),
        command: None,
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
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    assert!(marker.exists(), "on_spec_completed hook must have run");
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(
        hook_runs[0].event,
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted
    );
    assert_eq!(hook_runs[0].hook_index, 0);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
}

/// CH1: `on_spec_completed` fires once per queue member completing, with
/// `{{spec_name}}` bound to that spec.
#[tokio::test]
async fn graph_engine_on_spec_completed_fires_for_queue_member() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
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

    let marker = dir.path().join("queue-spec.marker");
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("touch \"{}\"", marker.display())),
        command: None,
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
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted,
        vec![hook],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(
            graph_id.clone(),
            Some("queue-1".to_string()),
            None,
            None,
            None,
            None,
        )
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    assert!(
        marker.exists(),
        "on_spec_completed hook must have run for queue member"
    );
    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(
        hook_runs[0].event,
        crate::domain::graphs::GraphHookEvent::OnSpecCompleted
    );
    assert_eq!(hook_runs[0].hook_index, 0);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
}
