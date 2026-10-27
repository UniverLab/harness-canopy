//! Part 11 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// CM26 FR1: a parallel bounce resumes the member whose last run in this
/// spec passed, and cold-starts a member whose last run was a straggler
/// kill. `member-pass` (self-reports Pass as the last thing its script
/// does before exiting) and `member-straggler`
/// (never filed; its own node-level `timeout_minutes: 0` — NOT the
/// ensemble's shared `straggler_timeout_minutes`, which the parallel path
/// applies to every member alike and would kill `member-pass` too —
/// forces its own agent timeout to fire immediately every dispatch) run
/// twice each. A counter-gate downstream of the join bounces back into
/// the ensemble exactly once before letting the spec finish.
#[tokio::test]
async fn parallel_bounce_resumes_passer_and_cold_starts_straggler() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let script = write_argv_echo_cli(dir.path());

    let argv_pass = dir.path().join("argv-pass.log");
    let mut env_pass = HashMap::new();
    env_pass.insert(
        "ARGV_FILE".to_string(),
        argv_pass.to_string_lossy().into_owned(),
    );
    env_pass.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_pass.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env_pass.insert("SELF_REPORT_NODE_ID".to_string(), "member-pass".to_string());
    env_pass.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env_pass.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let mut cli_pass = argv_cli_config(&script, env_pass, Some("--resume"), Some("--set"), None);
    cli_pass.name = "cli-pass".to_string();

    let argv_straggler = dir.path().join("argv-straggler.log");
    let mut env_straggler = HashMap::new();
    env_straggler.insert(
        "ARGV_FILE".to_string(),
        argv_straggler.to_string_lossy().into_owned(),
    );
    env_straggler.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_straggler.insert("LINGER_SECONDS".to_string(), "3".to_string());
    let mut cli_straggler = argv_cli_config(
        &script,
        env_straggler,
        Some("--resume"),
        Some("--set"),
        None,
    );
    cli_straggler.name = "cli-straggler".to_string();

    let home = setup_multi_resume_cli_home(vec![cli_pass, cli_straggler]);

    let counter = dir.path().join("gate-counter");
    db.insert_graph_node(&counter_gate_node("gate", &spec_id, &counter, 60))
        .unwrap();
    let done_marker = dir.path().join("done.marker");
    db.insert_graph_node(&touch_marker_node("done", &spec_id, &done_marker, 61))
        .unwrap();

    insert_bouncing_ensemble(
        &db,
        &spec_id,
        EnsembleKind::Parallel,
        &[
            ("member-pass", "cli-pass"),
            ("member-straggler", "cli-straggler"),
        ],
        1,
        Some(5),
        None,
        "gate",
        "done",
    );
    // Force member-straggler's OWN agent timeout to fire immediately,
    // every dispatch, without touching the ensemble's shared straggler
    // window (which `member-pass` shares and needs to survive).
    db.update_graph_node_details(
        "member-straggler",
        None,
        None,
        Some(&serde_json::json!({
            "platform": "cli-straggler",
            "prompt_template": "ignored by the member's test script",
            "timeout_minutes": 0,
            "infra_backoff_seconds": 0,
        })),
        None,
    )
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "gate->done".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "gate".to_string(),
        to_node: "done".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    for edge in bounce_edges_into_ensemble(&spec_id, "gate", &["member-pass", "member-straggler"]) {
        db.insert_graph_edge(&edge).unwrap();
    }

    let _home = HomeGuard::set(home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        done_marker.exists(),
        "gate must eventually pass (second visit) and reach the terminal node"
    );

    let pass_runs = member_runs(&db, &spec_id, "member-pass");
    assert_eq!(
        pass_runs.len(),
        2,
        "member-pass must run once per ensemble dispatch"
    );
    let first_session = pass_runs[0]
        .session_id
        .clone()
        .expect("first (cold) run must capture a session id via --set");
    assert_eq!(
        pass_runs[1].session_id.as_deref(),
        Some(first_session.as_str()),
        "the resumed run's row keeps the SAME session id, never a fresh one"
    );

    let argv_pass_text = std::fs::read_to_string(&argv_pass).unwrap();
    let chunks: Vec<&str> = argv_pass_text.split("===\n").collect();
    assert!(
        !chunks[0].contains("--resume"),
        "member-pass's first visit must be cold"
    );
    assert!(
        chunks[1].contains("--resume") && chunks[1].contains(&first_session),
        "member-pass's second visit must resume the SAME session id the spawn actually received"
    );

    let straggler_runs = member_runs(&db, &spec_id, "member-straggler");
    assert_eq!(
        straggler_runs.len(),
        2,
        "member-straggler must run once per ensemble dispatch"
    );
    for run in &straggler_runs {
        assert_eq!(
            run.status,
            GraphRunStatus::Fail,
            "the straggler is killed by the 0-minute timeout every dispatch"
        );
    }
    let argv_straggler_text = std::fs::read_to_string(&argv_straggler).unwrap_or_default();
    assert!(
        !argv_straggler_text.contains("--resume"),
        "a member that never passed must never be offered a resume candidate"
    );
}

/// CM26 FR2: round-robin's full within-spec contract in one topology —
/// `rr-a` (position 0) always fails, `rr-b` (position 1) always passes.
/// Visit 1 (cold, index 0) fails -> bounce. Visit 2 (bounce, pinned=0,
/// absent from `resumable_sessions` since it just failed -> advance to 1,
/// cold) passes -> bounce. Visit 3 (bounce, pinned=1, present -> resume)
/// passes -> spec ends. `rr-a` must never be revisited once the pin
/// advances past it, and the persisted `round_robin_index` must land on
/// 1 and stay there through the resumed visit 3 (which writes nothing).
#[tokio::test]
async fn round_robin_bounce_after_fail_advances_then_resumes_and_stays() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let script = write_argv_echo_cli(dir.path());

    let argv_a = dir.path().join("argv-a.log");
    let mut env_a = HashMap::new();
    env_a.insert(
        "ARGV_FILE".to_string(),
        argv_a.to_string_lossy().into_owned(),
    );
    env_a.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_a.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env_a.insert("SELF_REPORT_NODE_ID".to_string(), "rr-a".to_string());
    env_a.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env_a.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    env_a.insert("SELF_REPORT_STATUS".to_string(), "fail".to_string());
    let mut cli_a = argv_cli_config(&script, env_a, Some("--resume"), Some("--set"), None);
    cli_a.name = "cli-a".to_string();

    let argv_b = dir.path().join("argv-b.log");
    let mut env_b = HashMap::new();
    env_b.insert(
        "ARGV_FILE".to_string(),
        argv_b.to_string_lossy().into_owned(),
    );
    env_b.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_b.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env_b.insert("SELF_REPORT_NODE_ID".to_string(), "rr-b".to_string());
    env_b.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env_b.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let mut cli_b = argv_cli_config(&script, env_b, Some("--resume"), Some("--set"), None);
    cli_b.name = "cli-b".to_string();

    let home = setup_multi_resume_cli_home(vec![cli_a, cli_b]);

    let counter_fail = dir.path().join("gate-fail-counter");
    db.insert_graph_node(&counter_gate_node(
        "retry-gate-fail",
        &spec_id,
        &counter_fail,
        60,
    ))
    .unwrap();
    let counter_pass = dir.path().join("gate-pass-counter");
    db.insert_graph_node(&counter_gate_node(
        "retry-gate-pass",
        &spec_id,
        &counter_pass,
        61,
    ))
    .unwrap();
    let done_via_fail_gate = dir.path().join("done-via-fail-gate.marker");
    db.insert_graph_node(&touch_marker_node(
        "done-via-fail-gate",
        &spec_id,
        &done_via_fail_gate,
        62,
    ))
    .unwrap();
    let done_via_pass_gate = dir.path().join("done-via-pass-gate.marker");
    db.insert_graph_node(&touch_marker_node(
        "done-via-pass-gate",
        &spec_id,
        &done_via_pass_gate,
        63,
    ))
    .unwrap();

    insert_bouncing_ensemble(
        &db,
        &spec_id,
        EnsembleKind::RoundRobin,
        &[("rr-a", "cli-a"), ("rr-b", "cli-b")],
        1,
        Some(1),
        Some(0),
        "retry-gate-pass",
        "retry-gate-fail",
    );
    db.insert_graph_edge(&GraphEdge {
        id: "fail-gate->done".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "retry-gate-fail".to_string(),
        to_node: "done-via-fail-gate".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "pass-gate->done".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "retry-gate-pass".to_string(),
        to_node: "done-via-pass-gate".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    for edge in bounce_edges_into_ensemble(&spec_id, "retry-gate-fail", &["rr-a", "rr-b"]) {
        db.insert_graph_edge(&edge).unwrap();
    }
    for edge in bounce_edges_into_ensemble(&spec_id, "retry-gate-pass", &["rr-a", "rr-b"]) {
        db.insert_graph_edge(&edge).unwrap();
    }

    let _home = HomeGuard::set(home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        done_via_pass_gate.exists(),
        "the spec must finish via the pass-gate's second (APPROVED) call"
    );
    assert!(
        !done_via_fail_gate.exists(),
        "the fail-gate must never see its own second call in this sequence"
    );

    let a_runs = member_runs(&db, &spec_id, "rr-a");
    assert_eq!(
        a_runs.len(),
        1,
        "rr-a must never be revisited once the pin advances past it"
    );
    assert_eq!(a_runs[0].status, GraphRunStatus::Fail);

    let b_runs = member_runs(&db, &spec_id, "rr-b");
    assert_eq!(b_runs.len(), 2, "rr-b runs the cold advance and the resume");
    assert_eq!(b_runs[0].status, GraphRunStatus::Pass);
    assert_eq!(b_runs[1].status, GraphRunStatus::Pass);
    let b_session = b_runs[0]
        .session_id
        .clone()
        .expect("rr-b's cold run must capture a session id via --set");
    assert_eq!(
        b_runs[1].session_id.as_deref(),
        Some(b_session.as_str()),
        "rr-b's resumed run keeps the SAME session id"
    );

    let argv_b_text = std::fs::read_to_string(&argv_b).unwrap();
    let chunks: Vec<&str> = argv_b_text.split("===\n").collect();
    assert!(
        !chunks[0].contains("--resume"),
        "rr-b's cold advance visit must not resume"
    );
    assert!(
        chunks[1].contains("--resume") && chunks[1].contains(&b_session),
        "rr-b's third-visit run must resume the SAME session id"
    );

    assert_eq!(
        db.get_ensemble("ens1").unwrap().unwrap().round_robin_index,
        Some(1),
        "the persisted index lands on rr-b (set once, during the fail-advance) \
             and is unchanged by the later resume, which writes nothing"
    );
}

/// CM26 FR3: a cascade bounce resumes at the member that produced the
/// previous pass, not position 0. `casc-a` (position 0) is a genuinely
/// crashing script with no self-report, so visit 1 exhausts its infra
/// retries and falls through to `casc-b` (position 1), which passes cold.
/// The gate bounces once; visit 2 must start directly at `casc-b`,
/// resumed — `casc-a` must not run again.
#[tokio::test]
async fn cascade_bounce_resumes_previous_passer() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);

    let crash_script = write_member_script(dir.path(), "crash.sh", "exit 1");
    let cli_crash = crate::domain::cli_config::CliConfig {
        name: "cli-crash".to_string(),
        binary: crash_script,
        headless_mode: String::new(),
        prompt_via_stdin: true,
        ..Default::default()
    };

    let script_b = write_argv_echo_cli(dir.path());
    let argv_b = dir.path().join("argv-b.log");
    let mut env_b = HashMap::new();
    env_b.insert(
        "ARGV_FILE".to_string(),
        argv_b.to_string_lossy().into_owned(),
    );
    env_b.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_b.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env_b.insert("SELF_REPORT_NODE_ID".to_string(), "casc-b".to_string());
    env_b.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env_b.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let mut cli_b = argv_cli_config(&script_b, env_b, Some("--resume"), Some("--set"), None);
    cli_b.name = "cli-b".to_string();

    let home = setup_multi_resume_cli_home(vec![cli_crash, cli_b]);

    let counter = dir.path().join("gate-counter");
    db.insert_graph_node(&counter_gate_node("gate", &spec_id, &counter, 60))
        .unwrap();
    let done_marker = dir.path().join("done.marker");
    db.insert_graph_node(&touch_marker_node("done", &spec_id, &done_marker, 61))
        .unwrap();

    insert_bouncing_ensemble(
        &db,
        &spec_id,
        EnsembleKind::Cascade,
        &[("casc-a", "cli-crash"), ("casc-b", "cli-b")],
        1,
        Some(1),
        None,
        "gate",
        "done",
    );
    db.insert_graph_edge(&GraphEdge {
        id: "gate->done".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "gate".to_string(),
        to_node: "done".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    for edge in bounce_edges_into_ensemble(&spec_id, "gate", &["casc-a", "casc-b"]) {
        db.insert_graph_edge(&edge).unwrap();
    }

    let _home = HomeGuard::set(home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        done_marker.exists(),
        "the gate's second call must pass and reach the terminal node"
    );

    let a_runs_after = member_runs(&db, &spec_id, "casc-a").len();
    assert_eq!(
        a_runs_after, 3,
        "casc-a exhausts its infra-retry budget once in visit 1 (initial + 2 retries) \
             and must NOT run again in visit 2 — proves the walk started at casc-b, not position 0"
    );

    let b_runs = member_runs(&db, &spec_id, "casc-b");
    assert_eq!(
        b_runs.len(),
        2,
        "casc-b runs once cold (visit 1's fallthrough) and once resumed (visit 2)"
    );
    assert_eq!(b_runs[0].status, GraphRunStatus::Pass);
    assert_eq!(b_runs[1].status, GraphRunStatus::Pass);
    let first_session = b_runs[0]
        .session_id
        .clone()
        .expect("casc-b's cold run must capture a session id via --set");
    assert_eq!(
        b_runs[1].session_id.as_deref(),
        Some(first_session.as_str()),
        "casc-b's resumed run keeps the SAME session id"
    );

    let argv_b_text = std::fs::read_to_string(&argv_b).unwrap();
    let chunks: Vec<&str> = argv_b_text.split("===\n").collect();
    assert!(
        !chunks[0].contains("--resume"),
        "casc-b's first (fallthrough) visit must be cold"
    );
    assert!(
        chunks[1].contains("--resume") && chunks[1].contains(&first_session),
        "casc-b's second visit must resume the SAME session id"
    );
}

/// CM26 FR4: a member configured `resume: false` still cold-starts on a
/// bounce even though `resumable_sessions` holds a valid entry for it —
/// `node_allows_resume` (shared with plain nodes) gates it before the
/// ensemble candidate is ever consulted. Two members (parallel, `min_pass:
/// 1`) both pass every dispatch; only `member-noresume` carries the
/// override, so its behaviour is isolated from its sibling's.
#[tokio::test]
async fn ensemble_member_resume_false_forces_cold_start_on_bounce() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let script = write_argv_echo_cli(dir.path());

    let argv_noresume = dir.path().join("argv-noresume.log");
    let mut env_noresume = HashMap::new();
    env_noresume.insert(
        "ARGV_FILE".to_string(),
        argv_noresume.to_string_lossy().into_owned(),
    );
    env_noresume.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_noresume.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env_noresume.insert(
        "SELF_REPORT_NODE_ID".to_string(),
        "member-noresume".to_string(),
    );
    env_noresume.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env_noresume.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let mut cli_noresume =
        argv_cli_config(&script, env_noresume, Some("--resume"), Some("--set"), None);
    cli_noresume.name = "cli-noresume".to_string();

    let argv_sibling = dir.path().join("argv-sibling.log");
    let mut env_sibling = HashMap::new();
    env_sibling.insert(
        "ARGV_FILE".to_string(),
        argv_sibling.to_string_lossy().into_owned(),
    );
    env_sibling.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    env_sibling.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env_sibling.insert(
        "SELF_REPORT_NODE_ID".to_string(),
        "member-sibling".to_string(),
    );
    env_sibling.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env_sibling.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let mut cli_sibling =
        argv_cli_config(&script, env_sibling, Some("--resume"), Some("--set"), None);
    cli_sibling.name = "cli-sibling".to_string();

    let home = setup_multi_resume_cli_home(vec![cli_noresume, cli_sibling]);

    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let member_noresume = GraphNode {
        id: "member-noresume".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "member-noresume".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "cli-noresume",
            "prompt_template": "ignored by the member's test script",
            "timeout_minutes": 5,
            "infra_backoff_seconds": 0,
            "resume": false,
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    };
    let member_sibling = GraphNode {
        id: "member-sibling".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "member-sibling".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "cli-sibling",
            "prompt_template": "ignored by the member's test script",
            "timeout_minutes": 5,
            "infra_backoff_seconds": 0,
        }),
        position: 3,
        created_at: chrono::Utc::now(),
    };
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 50,
        created_at: chrono::Utc::now(),
    };
    let counter = dir.path().join("gate-counter");
    db.insert_graph_node(&counter_gate_node("gate", &spec_id, &counter, 60))
        .unwrap();
    let done_marker = dir.path().join("done.marker");
    db.insert_graph_node(&touch_marker_node("done", &spec_id, &done_marker, 61))
        .unwrap();

    let mut edges = vec![
        GraphEdge {
            id: "kickoff->noresume".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "member-noresume".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "kickoff->sibling".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "member-sibling".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "noresume->join1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "member-noresume".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "sibling->join1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "member-sibling".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "join1->gate".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "gate".to_string(),
            condition: GraphEdgeCondition::Pass,
        },
        GraphEdge {
            id: "join1->done".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "done".to_string(),
            condition: GraphEdgeCondition::Fail,
        },
        GraphEdge {
            id: "gate->done".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "gate".to_string(),
            to_node: "done".to_string(),
            condition: GraphEdgeCondition::Pass,
        },
    ];
    for edge in bounce_edges_into_ensemble(&spec_id, "gate", &["member-noresume", "member-sibling"])
    {
        edges.push(edge);
    }

    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "Resume False Ensemble".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: Some(1),
        quorum_grace_minutes: None,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "gate".to_string(),
        on_fail_to: Some("done".to_string()),
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: chrono::Utc::now(),

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members = vec![
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "member-noresume".to_string(),
            position: 0,
            platform: "cli-noresume".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "member-sibling".to_string(),
            position: 1,
            platform: "cli-sibling".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    db.insert_ensemble_unit(
        &ensemble,
        &ensemble_members,
        &[member_noresume, member_sibling],
        &join_node,
        &edges,
    )
    .unwrap();

    let _home = HomeGuard::set(home.path());
    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
        done_marker.exists(),
        "the gate's second call must pass and reach the terminal node"
    );

    let runs = member_runs(&db, &spec_id, "member-noresume");
    assert_eq!(runs.len(), 2, "the member runs once per ensemble dispatch");
    assert_eq!(runs[0].status, GraphRunStatus::Pass);
    assert_eq!(runs[1].status, GraphRunStatus::Pass);
    let first_session = runs[0]
        .session_id
        .clone()
        .expect("even a resume:false member still cold-captures a session id via --set");
    let second_session = runs[1]
        .session_id
        .clone()
        .expect("the bounce's cold start also mints/captures its own session id via --set");
    assert_ne!(
        first_session, second_session,
        "resume:false forces a cold start on the bounce -> a FRESH session id, not the first's, \
             even though resumable_sessions held a valid (Pass) entry for this member"
    );

    let argv_text = std::fs::read_to_string(&argv_noresume).unwrap();
    let chunks: Vec<&str> = argv_text.split("===\n").collect();
    assert!(!chunks[0].contains("--resume"), "first visit is cold");
    assert!(
        !chunks[1].contains("--resume"),
        "resume:false must force a cold start on the bounce too"
    );
}

/// Wait-all (F1): the join must never fire before every member has
/// finished. A fast member (instant) and a deliberately slower member
/// (sleeps ~1s) run side by side; if the engine consolidated as soon as
/// the fast one finished, the whole ensemble would complete in well
/// under a second. Asserting on wall-clock elapsed time — not just the
/// final consolidated output — is what actually proves the wait, since
/// the output alone can't distinguish "waited" from "raced and got
/// lucky".
#[tokio::test]
async fn ensemble_execute_waits_for_slowest_member_before_joining() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-fast",
            &write_member_script(dir.path(), "fast.sh", "printf FAST; exit 0"),
        ),
        (
            "member-slow",
            &write_member_script(dir.path(), "slow.sh", "sleep 1; printf SLOW; exit 0"),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[("m-fast", "member-fast"), ("m-slow", "member-slow")],
        2,
        Some(1),
        None,
        "on-pass",
        None,
    );

    let _home = HomeGuard::set(fake_home.path());
    let started = std::time::Instant::now();
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    drop(_home);

    assert!(
        elapsed >= std::time::Duration::from_millis(900),
        "join must not fire before the slow member finishes (elapsed: {elapsed:?})"
    );
    // CM13: bare member scripts never call graph_complete_node, so both
    // members are unreported infra and the join is Error with no_verdict
    // (CB66). Wait-all is still proven by the elapsed wall-clock time
    // above and the doc below.
    assert!(
        !pass_marker.exists(),
        "CM13: unreported members are infra, so the ensemble cannot pass"
    );

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    let doc = join.output.unwrap()["consolidated_doc"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(doc.contains("FAST") && doc.contains("SLOW"));
}

/// Concurrency cap (F1): `with_ensemble_concurrency_cap` must actually
/// bound how many members run at once, not just accept the value. Three
/// members each sleep ~0.3s; under a cap of 1 they're forced to run one
/// at a time, so the ensemble can only finish in >= ~0.9s. Asserting on
/// wall-clock elapsed time is what actually proves serialization — the
/// consolidated output alone can't distinguish "capped" from "raced and
/// got lucky", mirroring `ensemble_execute_waits_for_slowest_member_before_joining`.
#[tokio::test]
async fn ensemble_execute_respects_configured_concurrency_cap() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let engine = engine.with_ensemble_concurrency_cap(1);
    let fake_home = setup_multi_cli_home(&[
        (
            "member-a",
            &write_member_script(dir.path(), "a.sh", "sleep 0.3; printf A; exit 0"),
        ),
        (
            "member-b",
            &write_member_script(dir.path(), "b.sh", "sleep 0.3; printf B; exit 0"),
        ),
        (
            "member-c",
            &write_member_script(dir.path(), "c.sh", "sleep 0.3; printf C; exit 0"),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[
            ("m-a", "member-a"),
            ("m-b", "member-b"),
            ("m-c", "member-c"),
        ],
        3,
        Some(1),
        None,
        "on-pass",
        None,
    );

    let _home = HomeGuard::set(fake_home.path());
    let started = std::time::Instant::now();
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    let elapsed = started.elapsed();
    drop(_home);

    assert!(
        elapsed >= std::time::Duration::from_millis(850),
        "a concurrency cap of 1 must serialize all three members (elapsed: {elapsed:?})"
    );
    // CM13: bare member scripts never call graph_complete_node, so every
    // member is unreported infra and the join is Error with no_verdict
    // (CB66: 0/3 verdicts, never Fail). Serialization is still proven by
    // the elapsed wall-clock time above.
    assert!(
        !pass_marker.exists(),
        "CM13: unreported members are infra, so the ensemble cannot pass"
    );

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    let doc = join.output.unwrap()["consolidated_doc"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(doc.contains('A') && doc.contains('B') && doc.contains('C'));
}

/// CM13: bare member scripts never call graph_complete_node, so no member
/// can pass — the quorum sees 0 filed verdicts, the join is Error with
/// `no_verdict` (CB66) and the graph pauses with a blocker instead of
/// routing to on_fail_to, even though a majority "exited ok". Quorum
/// counting itself is covered by unit tests over NodeExecution; the join
/// Pass -> on_pass_to edge is covered live by
/// [`cm13_round_robin_falls_through_unreported_member_then_passes_on_next`].
#[tokio::test]
async fn ensemble_execute_unreported_members_route_to_on_fail_to() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-ok-a",
            &write_member_script(dir.path(), "a.sh", "printf ok"),
        ),
        (
            "member-ok-b",
            &write_member_script(dir.path(), "b.sh", "printf ok"),
        ),
        (
            "member-bad",
            &write_member_script(dir.path(), "c.sh", "exit 1"),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    let fail_marker = dir.path().join("fail.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    db.insert_graph_node(&touch_marker_node("on-fail", &spec_id, &fail_marker, 101))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[
            ("m-a", "member-ok-a"),
            ("m-b", "member-ok-b"),
            ("m-c", "member-bad"),
        ],
        2,
        Some(1),
        None,
        "on-pass",
        Some("on-fail"),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: script-backed members never self-report, so the quorum sees
    // 0 filed verdicts -> Error with no_verdict (CB66), pausing the graph
    // instead of routing to on_fail_to.
    assert_eq!(join.status, GraphRunStatus::Error);
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.unwrap()["passed"], 0);
    assert!(!pass_marker.exists(), "must not route to on_pass_to");
    assert!(
        !fail_marker.exists(),
        "CB66: a verdictless join never follows the Fail edge to on_fail_to"
    );
}

/// min_pass routing: too few members pass -> join Fail -> on_fail_to.
/// (CB66: when NO member files a verdict at all, the join is Error and
/// pauses instead — see the no_verdict assertions below.)
#[tokio::test]
async fn ensemble_execute_min_pass_not_met_routes_to_on_fail_to() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-ok",
            &write_member_script(dir.path(), "a.sh", "printf ok"),
        ),
        (
            "member-bad-a",
            &write_member_script(dir.path(), "b.sh", "exit 1"),
        ),
        (
            "member-bad-b",
            &write_member_script(dir.path(), "c.sh", "exit 1"),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    let fail_marker = dir.path().join("fail.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    db.insert_graph_node(&touch_marker_node("on-fail", &spec_id, &fail_marker, 101))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[
            ("m-a", "member-ok"),
            ("m-b", "member-bad-a"),
            ("m-c", "member-bad-b"),
        ],
        2,
        Some(1),
        None,
        "on-pass",
        Some("on-fail"),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Error);
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    // CM13: the script-backed member never self-reports, so passed is 0.
    assert_eq!(join.output.unwrap()["passed"], 0);
    assert!(!pass_marker.exists(), "must not route to on_pass_to");
    assert!(
        !fail_marker.exists(),
        "CB66: a verdictless join never follows the Fail edge to on_fail_to"
    );
}

/// Consolidation order is deterministic (member position order), not
/// completion order: member 1 is the slow one here, member 2 finishes
/// first, but the consolidated doc must still list member 1 before
/// member 2.
#[tokio::test]
async fn ensemble_execute_consolidates_in_member_position_order() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let fake_home = setup_multi_cli_home(&[
        (
            "member-a-slow",
            &write_member_script(dir.path(), "a.sh", "sleep 1; printf A; exit 0"),
        ),
        (
            "member-b-fast",
            &write_member_script(dir.path(), "b.sh", "printf B; exit 0"),
        ),
    ]);
    let pass_marker = dir.path().join("pass.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[("m-a", "member-a-slow"), ("m-b", "member-b-fast")],
        2,
        Some(1),
        None,
        "on-pass",
        None,
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    let doc = join.output.unwrap()["consolidated_doc"]
        .as_str()
        .unwrap()
        .to_string();
    let pos_a = doc
        .find("## member-a-slow")
        .expect("member-a-slow section must exist");
    let pos_b = doc
        .find("## member-b-fast")
        .expect("member-b-fast section must exist");
    assert!(
        pos_a < pos_b,
        "consolidated doc must list members in position order, not completion order"
    );
}

/// A multi-angle panel (the point of per-member `prompt_override`): three
/// members sharing the SAME platform/model — so `member_label`'s old
/// "platform/model" text alone would produce three identical, unlabeled
/// "## shared-cli [pass]" headings — each renders its own override
/// instead of the (unused here) shared prompt. The quorum must still
/// attribute each section to its own member (by position, since
/// platform/model can no longer do it) and each section's content must
/// be that member's own rendered prompt, not another member's or the
/// shared template.
#[tokio::test]
async fn ensemble_execute_attributes_members_sharing_platform_by_prompt_override() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    // A single registered CLI, `cat`, echoes its composed prompt back on
    // stdout verbatim — letting the consolidated doc prove which prompt
    // text each member actually rendered.
    let fake_home = setup_multi_cli_home(&[(
        "shared-cli",
        &write_member_script(dir.path(), "echo.sh", "cat"),
    )]);

    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "printf ok", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let pass_marker = dir.path().join("pass.marker");
    db.insert_graph_node(&touch_marker_node("on-pass", &spec_id, &pass_marker, 100))
        .unwrap();

    let overrides = [
        ("m-context", "OVERRIDE-CONTEXT-ANGLE"),
        ("m-security", "OVERRIDE-SECURITY-ANGLE"),
        ("m-conventions", "OVERRIDE-CONVENTIONS-ANGLE"),
    ];
    let now = chrono::Utc::now();
    let member_nodes: Vec<GraphNode> = overrides
        .iter()
        .enumerate()
        .map(|(i, (node_id, prompt))| GraphNode {
            id: node_id.to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: format!("Panel [{}]", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "shared-cli",
                "prompt_template": prompt,
                "timeout_minutes": 5,
            }),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 2 + overrides.len() as i64,
        created_at: now,
    };
    let mut edges = Vec::new();
    for (node_id, _) in &overrides {
        edges.push(GraphEdge {
            id: format!("kickoff->{node_id}"),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{node_id}->join1"),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: node_id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: "join1->on-pass".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "on-pass".to_string(),
        condition: GraphEdgeCondition::Pass,
    });
    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "Panel".to_string(),
        prompt_template: "shared prompt (unused — every member overrides it)".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 3,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "on-pass".to_string(),
        on_fail_to: None,
        kind: crate::domain::graphs::EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members: Vec<EnsembleMember> = overrides
        .iter()
        .enumerate()
        .map(|(i, (node_id, prompt))| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: "shared-cli".to_string(),
            model: None,
            prompt_override: Some(prompt.to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    db.insert_ensemble_unit(
        &ensemble,
        &ensemble_members,
        &member_nodes,
        &join_node,
        &edges,
    )
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    // CM13: the cat-backed members never call graph_complete_node, so each
    // is unreported infra and the join is Error with no_verdict (CB66).
    // Attribution is still proven by the position-numbered headings and
    // per-member prompt text below.
    assert_eq!(join.status, GraphRunStatus::Error);
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert!(!pass_marker.exists());
    let doc = join.output.unwrap()["consolidated_doc"]
        .as_str()
        .unwrap()
        .to_string();

    // Every member shares "shared-cli" with no model, so the label alone
    // no longer disambiguates — three distinct, position-numbered
    // headings must still exist.
    assert!(doc.contains("## shared-cli #1 [fail]"), "{doc}");
    assert!(doc.contains("## shared-cli #2 [fail]"), "{doc}");
    assert!(doc.contains("## shared-cli #3 [fail]"), "{doc}");

    // Each section carries that member's own rendered prompt, not the
    // (unused) shared template and not another member's override.
    let pos1 = doc.find("## shared-cli #1").unwrap();
    let pos2 = doc.find("## shared-cli #2").unwrap();
    let pos3 = doc.find("## shared-cli #3").unwrap();
    assert!(doc[pos1..pos2].contains("OVERRIDE-CONTEXT-ANGLE"));
    assert!(!doc[pos1..pos2].contains("OVERRIDE-SECURITY-ANGLE"));
    assert!(doc[pos2..pos3].contains("OVERRIDE-SECURITY-ANGLE"));
    assert!(!doc[pos2..pos3].contains("OVERRIDE-CONVENTIONS-ANGLE"));
    assert!(doc[pos3..].contains("OVERRIDE-CONVENTIONS-ANGLE"));
    assert!(!doc.contains("shared prompt (unused"));
}

/// Straggler kill + no-verdict counting (B12, CB66): a member that hangs
/// past the ensemble's straggler timeout is killed at the OS level (not
/// just marked failed while the process keeps running), and produces no
/// verdict — so an all-straggler join is Error with no_verdict, never Fail. Both members hang here — using a
/// `straggler_timeout_minutes: 0` (immediate) alongside a member that's
/// meant to finish quickly would race the timeout against real work;
/// isolating the straggler behavior to every member avoids that.
#[tokio::test]
async fn ensemble_execute_straggler_timeout_kills_process_and_counts_as_fail() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();
    let marker_a = dir.path().join("a_survived.marker");
    let marker_b = dir.path().join("b_survived.marker");
    let fake_home = setup_multi_cli_home(&[
        (
            "member-hang-a",
            &write_member_script(
                dir.path(),
                "a.sh",
                &format!("sleep 3; touch \"{}\"", marker_a.display()),
            ),
        ),
        (
            "member-hang-b",
            &write_member_script(
                dir.path(),
                "b.sh",
                &format!("sleep 3; touch \"{}\"", marker_b.display()),
            ),
        ),
    ]);
    let fail_marker = dir.path().join("fail.marker");
    db.insert_graph_node(&touch_marker_node(
        "on-pass",
        &spec_id,
        &dir.path().join("pass.marker"),
        100,
    ))
    .unwrap();
    db.insert_graph_node(&touch_marker_node("on-fail", &spec_id, &fail_marker, 101))
        .unwrap();
    insert_test_ensemble(
        &db,
        &spec_id,
        "kickoff",
        "ens1",
        "join1",
        &[("m-a", "member-hang-a"), ("m-b", "member-hang-b")],
        1,
        Some(0),
        None,
        "on-pass",
        Some("on-fail"),
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(
        join.status,
        GraphRunStatus::Error,
        "both members killed as stragglers -> zero verdicts -> join is Error (CB66)"
    );
    assert_eq!(
        join.output.as_ref().unwrap()["no_verdict"],
        serde_json::json!(true)
    );
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);
    assert!(
        !fail_marker.exists(),
        "CB66: a verdictless join never follows the Fail edge to on_fail_to"
    );

    // Give the OS a moment past the members' scripted 3s sleep to prove
    // the processes were actually killed, not merely marked failed
    // while still running in the background.
    tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    assert!(
        !marker_a.exists(),
        "straggler member a must have been killed"
    );
    assert!(
        !marker_b.exists(),
        "straggler member b must have been killed"
    );
}
