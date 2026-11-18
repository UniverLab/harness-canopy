//! Part 26 of graph-engine `tests`: spec-less cron graphs with a
//! `trigger_prompt` (FR3/FR4/FR5) — a tick runs the nodes with the prompt
//! as its idea, an overlap skips, and a check that finds nothing to do
//! completes idle instead of failing.
use super::*;

const TICK_PROMPT: &str = "PING-FROM-TICK sweep the inbox for news";

/// A graph with no specs, a cron trigger and a `trigger_prompt`: the
/// spec-less shape a cron tick launches.
fn tick_graph(db: &Database, workdir: &str, prompt: &str) -> String {
    let graph_id = format!("tick-{}", uuid::Uuid::new_v4());
    db.insert_graph(&Graph {
        trigger_prompt: Some(prompt.to_string()),
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "tick-graph".to_string(),
        description: None,
        workdir: workdir.to_string(),
        status: GraphStatus::Draft,
        trigger: Some(crate::domain::models::Trigger::Cron {
            schedule_expr: "* * * * *".to_string(),
        }),
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    graph_id
}

fn tick_check_node(db: &Database, graph_id: &str, command: &str, position: i64) {
    db.insert_graph_node(&GraphNode {
        id: format!("check-{position}"),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: format!("check-{position}"),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": command, "success_condition": "exit_code_0"}),
        position,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

fn tick_pass_edge(db: &Database, graph_id: &str, from: &str, to: &str, position: i64) {
    db.insert_graph_edge(&GraphEdge {
        id: format!("edge-{position}"),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
}

/// The one bound spec of a tick run plus the `graph_completed` payload's
/// `outcome` (`None` when the event carries no payload).
fn tick_outcome(db: &Database, graph_id: &str) -> (GraphSpec, Option<String>) {
    let specs = db.list_graph_specs(graph_id).unwrap();
    assert_eq!(
        specs.len(),
        1,
        "a tick run has exactly its placeholder spec"
    );
    let events = db
        .list_graph_events(graph_id, None, None, None, 200)
        .unwrap();
    let completed = events
        .iter()
        .find(|event| event.kind == "graph_completed")
        .unwrap_or_else(|| panic!("a completed tick run records graph_completed: {events:?}"));
    let outcome = completed.payload.as_deref().map(|raw| {
        serde_json::from_str::<serde_json::Value>(raw).expect("payload is JSON")["outcome"]
            .as_str()
            .expect("outcome is a string")
            .to_string()
    });
    (specs.into_iter().next().expect("one spec"), outcome)
}

/// A tick run whose check fails with no outgoing Fail edge — and no agent
/// ran — completes idle: graph and spec `Completed`, `outcome: "idle"`,
/// no attempt counted and no failure hook fired.
#[tokio::test]
async fn tick_failing_check_with_no_fail_edge_completes_idle() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    tick_check_node(&db, &graph_id, "exit 1", 1);
    let marker = dir.path().join("on-failed-fired");
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnFailed,
        vec![GraphCompletionHook {
            platform: None,
            model: None,
            effort: None,
            prompt: None,
            command: Some(format!("touch {}", marker.display())),
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(1),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
            crew: None,
            spec_ids: None,
        }],
    );
    db.update_graph_hooks(&graph_id, &hooks).unwrap();

    engine
        .run_graph_mode(
            graph_id.clone(),
            None,
            None,
            Some(TICK_PROMPT.to_string()),
            None,
            DispatchMode::tick(),
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    let (spec, outcome) = tick_outcome(&db, &graph_id);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert_eq!(outcome.as_deref(), Some("idle"));
    assert_eq!(
        db.get_graph_spec_cross_run_attempts(&spec.id).unwrap(),
        0,
        "an idle tick never counts toward attempt limits"
    );
    assert!(!marker.exists(), "an idle tick fires no failure hook");
    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    assert!(
        !events.iter().any(|event| event.kind == "graph_failed"),
        "an idle tick records no failure: {events:?}"
    );
}

/// The same graph run manually — the identical check failure with no Fail
/// edge — keeps failing exactly as before.
#[tokio::test]
async fn manual_run_of_same_graph_still_fails() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    tick_check_node(&db, &graph_id, "exit 1", 1);

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].status, GraphSpecStatus::Failed);
    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    assert!(
        events.iter().any(|event| event.kind == "graph_failed"),
        "a manual run records its failure: {events:?}"
    );
}

/// A tick run that reaches its agent node carries `outcome: "worked"`,
/// and the agent receives the trigger prompt as its `{{spec_content}}`.
#[tokio::test]
async fn tick_reaching_agent_is_worked_and_agent_receives_prompt() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let db_path = dir
        .path()
        .join(crate::graph_engine::tests::fixtures_a::TEST_DB_FILENAME);
    let argv_file = dir.path().join("argv.log");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env.insert("SELF_REPORT_NODE_ID".to_string(), "node-agent".to_string());
    env.insert("SELF_REPORT_STATUS".to_string(), "pass".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);
    let _home = HomeGuard::set(home.path());

    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    tick_check_node(&db, &graph_id, "exit 0", 1);
    db.insert_graph_node(&GraphNode {
        id: "node-agent".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "resume-cli",
            "prompt_template": "{{spec_content}}",
            "infra_backoff_seconds": 0,
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    tick_pass_edge(&db, &graph_id, "check-1", "node-agent", 1);

    engine
        .run_graph_mode(
            graph_id.clone(),
            None,
            None,
            Some(TICK_PROMPT.to_string()),
            None,
            DispatchMode::tick(),
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    let (_spec, outcome) = tick_outcome(&db, &graph_id);
    assert_eq!(outcome.as_deref(), Some("worked"));
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains(TICK_PROMPT),
        "the agent receives the trigger prompt: {argv:?}"
    );
}

/// A tick run whose agent node fails is a failure exactly as today —
/// the idle carve-out never covers an agent verdict.
#[tokio::test]
async fn tick_failing_agent_is_still_failure() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let db_path = dir
        .path()
        .join(crate::graph_engine::tests::fixtures_a::TEST_DB_FILENAME);
    let argv_file = dir.path().join("argv.log");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env.insert("SELF_REPORT_NODE_ID".to_string(), "node-agent".to_string());
    env.insert("SELF_REPORT_STATUS".to_string(), "fail".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);
    let _home = HomeGuard::set(home.path());

    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    tick_check_node(&db, &graph_id, "exit 0", 1);
    db.insert_graph_node(&GraphNode {
        id: "node-agent".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "resume-cli",
            "prompt_template": "{{spec_content}}",
            "infra_backoff_seconds": 0,
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    tick_pass_edge(&db, &graph_id, "check-1", "node-agent", 1);

    engine
        .run_graph_mode(
            graph_id.clone(),
            None,
            None,
            Some(TICK_PROMPT.to_string()),
            None,
            DispatchMode::tick(),
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
}

/// FR5: a tick run whose agent work ran inside an ensemble still counts
/// as "an agent ran" — a later check-failure dead end is a failure,
/// never idle. The ensemble's member passes, the join routes onward,
/// and the terminal check fails with no outgoing Fail edge.
#[tokio::test]
async fn tick_ensemble_agent_run_blocks_idle_terminal() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let db_path = dir
        .path()
        .join(crate::graph_engine::tests::fixtures_a::TEST_DB_FILENAME);
    let argv_file = dir.path().join("argv.log");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env.insert("SELF_REPORT_NODE_ID".to_string(), "m1".to_string());
    env.insert("SELF_REPORT_STATUS".to_string(), "pass".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);
    let _home = HomeGuard::set(home.path());

    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    tick_check_node(&db, &graph_id, "exit 0", 1);
    tick_check_node(&db, &graph_id, "exit 1", 4);
    let now = chrono::Utc::now();
    let member = GraphNode {
        id: "m1".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "member-1".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "resume-cli",
            "prompt_template": "ignored by the member's test script",
            "timeout_minutes": 5,
            "infra_backoff_seconds": 0,
        }),
        position: 2,
        created_at: now,
    };
    let join = GraphNode {
        id: "join1".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 3,
        created_at: now,
    };
    let edges = vec![
        GraphEdge {
            id: "check-1->m1".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            from_node: "check-1".to_string(),
            to_node: "m1".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m1->join1".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            from_node: "m1".to_string(),
            to_node: "join1".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "join1->check-4".to_string(),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            from_node: "join1".to_string(),
            to_node: "check-4".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Pass,
        },
    ];
    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "Tick Ensemble".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "check-1".to_string(),
        entry_condition: crate::domain::graphs::GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "check-4".to_string(),
        on_fail_to: None,
        kind: crate::domain::graphs::EnsembleKind::Cascade,
        round_robin_index: None,
        created_at: now,
        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    db.insert_ensemble_unit(
        &ensemble,
        &[EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "resume-cli".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }],
        &[member],
        &join,
        &edges,
    )
    .unwrap();

    engine
        .run_graph_mode(
            graph_id.clone(),
            None,
            None,
            Some(TICK_PROMPT.to_string()),
            None,
            DispatchMode::tick(),
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(
        lp.status,
        GraphStatus::Failed,
        "an agent ensemble member ran, so the terminal check failure is a failure, not idle"
    );
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].status, GraphSpecStatus::Failed);
    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    assert!(
        events.iter().any(|event| event.kind == "graph_failed"),
        "the ensemble-agent tick records its failure: {events:?}"
    );
}

/// A graph with bound specs keeps today's behaviour when it also carries
/// a `trigger_prompt`: the prompt is ignored and no idea placeholder is
/// created.
#[tokio::test]
async fn bound_specs_ignore_prompt_and_run_as_before() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    let spec = GraphSpec {
        intent: None,
        id: "bound-spec".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "Bound work".to_string(),
        description: Some("Do the bound thing".to_string()),
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
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
    };
    db.insert_graph_spec(&spec).unwrap();
    db.insert_graph_node(&GraphNode {
        id: "bound-check".to_string(),
        spec_id: Some("bound-spec".to_string()),
        graph_id: None,
        name: "check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "exit 0", "success_condition": "exit_code_0"}),
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
    let specs = db.list_graph_specs(&graph_id).unwrap();
    assert_eq!(specs.len(), 1, "no idea placeholder is created");
    assert_eq!(specs[0].id, "bound-spec");
    let events = db
        .list_graph_events(&graph_id, None, None, None, 200)
        .unwrap();
    let completed = events
        .iter()
        .find(|event| event.kind == "graph_completed")
        .expect("a completion event is recorded");
    assert!(
        completed.payload.is_none(),
        "a manual run carries no outcome payload"
    );
}

/// A tick check failure with an outgoing Fail edge that leads to a
/// failing node is a failure, not idle.
#[tokio::test]
async fn tick_check_with_fail_edge_to_failing_node_still_fails() {
    let (dir, db, engine, _seed_graph, _seed_spec) = graph_fixture().unwrap();
    let workdir = dir.path().to_string_lossy().to_string();
    let graph_id = tick_graph(&db, &workdir, TICK_PROMPT);
    tick_check_node(&db, &graph_id, "exit 1", 1);
    tick_check_node(&db, &graph_id, "exit 1", 2);
    db.insert_graph_edge(&GraphEdge {
        id: "edge-fail".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        from_node: "check-1".to_string(),
        to_node: "check-2".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph_mode(
            graph_id.clone(),
            None,
            None,
            Some(TICK_PROMPT.to_string()),
            None,
            DispatchMode::tick(),
        )
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
}
