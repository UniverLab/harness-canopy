//! Part 04 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

// ── S2: pinned skills (node.config["skills"]) ───────────────────────

#[test]
fn node_pinned_skills_parses_array_of_strings() {
    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "skills": ["coder", "reviewer"]
    }));
    assert_eq!(
        node_pinned_skills(&node),
        vec!["coder".to_string(), "reviewer".to_string()]
    );
}

#[test]
fn node_pinned_skills_defaults_to_empty_when_absent() {
    let node = agent_node_with_config(serde_json::json!({ "platform": "claude" }));
    assert!(node_pinned_skills(&node).is_empty());
}

#[test]
fn node_pinned_skills_ignores_malformed_values_instead_of_failing() {
    // Not an array at all: no pins, not an error.
    let not_an_array = agent_node_with_config(serde_json::json!({ "skills": "coder" }));
    assert!(node_pinned_skills(&not_an_array).is_empty());

    // An array with a non-string element mixed in with valid names:
    // keep the valid entries, drop the malformed one.
    let mixed = agent_node_with_config(serde_json::json!({ "skills": ["coder", 42, "reviewer"] }));
    assert_eq!(
        node_pinned_skills(&mixed),
        vec!["coder".to_string(), "reviewer".to_string()]
    );
}

#[tokio::test]
async fn append_pinned_skills_is_noop_when_absent_or_empty() {
    // Absent/empty `skills` must behave exactly as today: byte-identical
    // prompt, no dynamic-skill-store call at all (passing `None` here
    // would panic if the empty-check didn't short-circuit first).
    let node_absent = agent_node_with_config(serde_json::json!({ "platform": "claude" }));
    let prompt = append_pinned_skills("base prompt".to_string(), &node_absent, None).await;
    assert_eq!(prompt, "base prompt");

    let node_empty = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "skills": []
    }));
    let prompt = append_pinned_skills("base prompt".to_string(), &node_empty, None).await;
    assert_eq!(prompt, "base prompt");
}

#[tokio::test]
async fn append_pinned_skills_appends_resolved_content_in_listed_order() {
    let registry = tempdir().unwrap();
    make_skill_registry(
        registry.path(),
        &[("alpha", "Alpha body."), ("beta", "Beta body.")],
    );
    let store_dir = tempdir().unwrap();
    let store = Arc::new(skill_store_for(registry.path(), store_dir.path()));

    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "skills": ["beta", "alpha"]
    }));

    let prompt = append_pinned_skills("base prompt".to_string(), &node, Some(&store)).await;

    assert!(prompt.starts_with("base prompt"));
    let beta_pos = prompt.find("## Skill: beta").expect("beta section present");
    let alpha_pos = prompt
        .find("## Skill: alpha")
        .expect("alpha section present");
    assert!(
        beta_pos < alpha_pos,
        "skills must be appended in the order listed on the node, not alphabetically"
    );
    assert!(prompt.contains("Beta body."));
    assert!(prompt.contains("Alpha body."));
}

#[tokio::test]
async fn append_pinned_skills_degrades_to_note_for_unknown_skill_name() {
    let registry = tempdir().unwrap();
    make_skill_registry(registry.path(), &[("alpha", "Alpha body.")]);
    let store_dir = tempdir().unwrap();
    let store = Arc::new(skill_store_for(registry.path(), store_dir.path()));

    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "skills": ["does-not-exist"]
    }));

    let prompt = append_pinned_skills("base prompt".to_string(), &node, Some(&store)).await;

    assert!(prompt.contains("## Skill: does-not-exist"));
    assert!(prompt.contains("Could not resolve"));
}

#[tokio::test]
async fn append_pinned_skills_degrades_gracefully_without_a_configured_store() {
    // No dynamic skill store at all (e.g. a GraphEngine built without
    // `with_dynamic_skills`) must not fail the spawn either — every pin
    // just becomes a note.
    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "skills": ["coder", "reviewer"]
    }));

    let prompt = append_pinned_skills("base prompt".to_string(), &node, None).await;

    assert!(prompt.contains("## Skill: coder"));
    assert!(prompt.contains("## Skill: reviewer"));
    assert!(prompt.contains("Could not resolve"));
}

/// B9: a large pinned skill's content, once appended, must be carried by
/// the same oversized-prompt mechanism as everything else in the
/// composed prompt — the injected content is not a special case.
#[tokio::test]
async fn large_pinned_skill_pushes_prompt_past_argv_threshold_forcing_stdin() {
    let registry = tempdir().unwrap();
    let huge_body = "z".repeat(ARGV_SAFETY_THRESHOLD + 1);
    make_skill_registry(registry.path(), &[("huge", &huge_body)]);
    let store_dir = tempdir().unwrap();
    let store = Arc::new(skill_store_for(registry.path(), store_dir.path()));

    let node = agent_node_with_config(serde_json::json!({
        "platform": "claude",
        "skills": ["huge"]
    }));

    let prompt = append_pinned_skills("small base prompt".to_string(), &node, Some(&store)).await;
    assert!(prompt.len() > ARGV_SAFETY_THRESHOLD);

    let base_strategy = sample_strategy("/bin/cat");
    assert!(!base_strategy.prompt_via_stdin);
    let sized = sized_strategy(&base_strategy, &prompt);
    assert!(
        sized.prompt_via_stdin,
        "an oversized pinned skill must force stdin transport, same as any other cause"
    );
}

/// Full pipeline, not just the helper: `GraphEngine::with_dynamic_skills`
/// through `execute_node` → `execute_agent_node` → `append_pinned_skills`
/// → the actual spawned process, proving the plumbing between the engine
/// and the S1 store is wired correctly end to end.
#[tokio::test]
async fn execute_node_injects_pinned_skill_into_composed_prompt_end_to_end() {
    let registry = tempdir().unwrap();
    make_skill_registry(registry.path(), &[("coder", "Write clean code.")]);
    let store_dir = tempdir().unwrap();
    let store = Arc::new(skill_store_for(registry.path(), store_dir.path()));

    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let engine = engine.with_dynamic_skills(Arc::clone(&store));

    let spec = standalone_spec("sid-pin", 1);
    db.insert_graph_spec(&spec).unwrap();
    let node = GraphNode {
        id: "node-pin".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "skills-test-cli",
            "prompt_template": "{{spec_content}}",
            "skills": ["coder"]
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();

    let cli = crate::domain::cli_config::CliConfig {
        name: "skills-test-cli".to_string(),
        binary: "/bin/cat".to_string(),
        prompt_via_stdin: true,
        ..Default::default()
    };
    let home = write_resume_cli_home(cli);
    let _guard = HomeGuard::set(home.path());

    let lp = db.get_graph(&graph_id).unwrap().unwrap();

    let execution = engine
        .execute_node(
            &lp,
            &spec,
            &node,
            None,
            None,
            None,
            "run-pin",
            dir.path().to_str().unwrap(),
            None,
            false,
            &HashMap::new(),
            &[],
        )
        .await
        .unwrap();

    let stdout = execution
        .output
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(stdout.contains("## Skill: coder"));
    assert!(stdout.contains("Write clean code."));
}

#[tokio::test]
async fn run_agent_process_records_set_at_spawn_session_id() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-sid");
    let cli = Cli::new("test-cli");
    let mut strategy = sample_strategy("/bin/echo");
    strategy.session_id_set_flag = Some("--session-id".to_string());

    run_agent_process(
        &db, "run-sid", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    let run = db.get_graph_run("run-sid").unwrap().unwrap();
    let sid = run
        .session_id
        .expect("set-at-spawn platform must record a session id");
    uuid::Uuid::parse_str(&sid).expect("recorded session id must be a uuid");
}

#[tokio::test]
async fn run_agent_process_leaves_session_id_null_without_set_flag() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-nosid");
    let cli = Cli::new("test-cli");
    let strategy = sample_strategy("/bin/echo");

    run_agent_process(
        &db,
        "run-nosid",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    let run = db.get_graph_run("run-nosid").unwrap().unwrap();
    assert_eq!(
        run.session_id, None,
        "no set flag and no capture: session_id must stay NULL"
    );
}

#[tokio::test]
async fn run_agent_process_captures_new_session_id_after_run() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-cap");
    let scratch = tempdir().unwrap();
    let statefile = scratch.path().join("sessions");
    std::fs::write(&statefile, "ses_pre_existing\n").unwrap();
    let script = write_fake_session_cli(scratch.path());

    let mut env = HashMap::new();
    env.insert("STATEFILE".to_string(), statefile.to_string_lossy().into());
    env.insert("APPEND_ID".to_string(), "ses_brand_new".to_string());
    let strategy = fake_session_strategy(&script, env);
    let cli = Cli::new("fake");

    let execution = run_agent_process(
        &db, "run-cap", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    let run = db.get_graph_run("run-cap").unwrap().unwrap();
    assert_eq!(
        run.session_id.as_deref(),
        Some("ses_brand_new"),
        "the single new session in the after-list must be attributed to the run"
    );
}

#[tokio::test]
async fn run_agent_process_no_new_session_leaves_session_id_null() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-nonew");
    let scratch = tempdir().unwrap();
    let statefile = scratch.path().join("sessions");
    std::fs::write(&statefile, "ses_pre_existing\n").unwrap();
    let script = write_fake_session_cli(scratch.path());

    // No APPEND_ID: the run creates no session, so the diff is empty.
    let mut env = HashMap::new();
    env.insert("STATEFILE".to_string(), statefile.to_string_lossy().into());
    let strategy = fake_session_strategy(&script, env);
    let cli = Cli::new("fake");

    let execution = run_agent_process(
        &db,
        "run-nonew",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    let run = db.get_graph_run("run-nonew").unwrap().unwrap();
    assert_eq!(
        run.session_id, None,
        "no new session in the diff must leave session_id NULL"
    );
}

#[tokio::test]
async fn run_agent_process_list_failure_leaves_null_and_verdict_unaffected() {
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-listfail");
    let scratch = tempdir().unwrap();
    let statefile = scratch.path().join("sessions");
    std::fs::write(&statefile, "ses_pre_existing\n").unwrap();
    let script = write_fake_session_cli(scratch.path());

    // FAIL_LIST makes every `list` invocation exit non-zero. Capture must
    // silently give up (NULL) while the run's own verdict is untouched.
    let mut env = HashMap::new();
    env.insert("STATEFILE".to_string(), statefile.to_string_lossy().into());
    env.insert("APPEND_ID".to_string(), "ses_brand_new".to_string());
    env.insert("FAIL_LIST".to_string(), "1".to_string());
    let strategy = fake_session_strategy(&script, env);
    let cli = Cli::new("fake");

    let execution = run_agent_process(
        &db,
        "run-listfail",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        // CM13: the fake session CLI never calls graph_complete_node, so the
        // run is unreported infra — but capture must still silently give up
        // (NULL) without affecting anything else.
        "CM13: unreported run is infra"
    );
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("unreported")
    );
    let run = db.get_graph_run("run-listfail").unwrap().unwrap();
    assert_eq!(run.session_id, None, "capture failure must leave NULL");
}

#[tokio::test]
async fn run_agent_process_set_at_spawn_skips_list_capture() {
    // A platform with BOTH a set-at-spawn flag and a list command must
    // use set-at-spawn (uuid, known before spawn) and never run the list
    // diff — set-at-spawn takes strict precedence.
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let node = seed_agent_run(&db, &graph_id, "run-precedence");
    let scratch = tempdir().unwrap();
    let statefile = scratch.path().join("sessions");
    std::fs::write(&statefile, "ses_pre_existing\n").unwrap();
    let script = write_fake_session_cli(scratch.path());

    let mut env = HashMap::new();
    env.insert("STATEFILE".to_string(), statefile.to_string_lossy().into());
    env.insert("APPEND_ID".to_string(), "ses_brand_new".to_string());
    let mut strategy = fake_session_strategy(&script, env);
    strategy.session_id_set_flag = Some("--session-id".to_string());
    let cli = Cli::new("fake");

    run_agent_process(
        &db,
        "run-precedence",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    let run = db.get_graph_run("run-precedence").unwrap().unwrap();
    let sid = run.session_id.expect("set-at-spawn must record an id");
    uuid::Uuid::parse_str(&sid)
        .expect("recorded id must be the set-at-spawn uuid, not a listed session id");
}

#[tokio::test]
async fn resume_uses_resume_flag_and_incremental_prompt() {
    let (execution, run, argv) =
        run_resume_agent_node(Value::Null, Some("ses_prev"), false, None, None, false).await;
    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    assert!(argv.contains("--resume"), "resume flag must be passed");
    assert!(argv.contains("ses_prev"), "the resumed id must be passed");
    // Incremental prompt: the resume continuation marker, but NOT the full
    // cold `[SPEC]` block the session already holds.
    assert!(argv.contains("[CONTINUE]"), "resume prompt must be sent");
    assert!(
        !argv.contains("# [SPEC]"),
        "a same-spec resume must not re-render the full spec block"
    );
    assert!(
        !argv.contains("finished and already committed"),
        "a same-spec resume must not carry the cross-spec boundary notice"
    );
    // The resumed run records the SAME session id; capture is skipped.
    assert_eq!(run.session_id.as_deref(), Some("ses_prev"));
}

#[tokio::test]
async fn resume_false_config_forces_cold_start() {
    let (execution, _run, argv) = run_resume_agent_node(
        serde_json::json!({ "resume": false }),
        Some("ses_prev"),
        false,
        None,
        None,
        false,
    )
    .await;
    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    assert!(
        !argv.contains("--resume"),
        "resume:false must force a cold start"
    );
    assert!(
        argv.contains("# [SPEC]"),
        "cold start renders the full spec"
    );
}

#[tokio::test]
async fn first_visit_without_session_is_cold() {
    // No resume_session_id offered (first visit to the node) → cold.
    let (execution, _run, argv) =
        run_resume_agent_node(Value::Null, None, false, None, None, false).await;
    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    assert!(!argv.contains("--resume"));
    assert!(argv.contains("# [SPEC]"));
}

#[tokio::test]
async fn resume_failure_falls_back_to_cold_and_verdict_from_cold_run() {
    // The resume attempt is rejected at spawn (FAIL_RESUME); the engine
    // must fall back to a cold start whose (passing) verdict the node uses.
    let (execution, _run, argv) =
        run_resume_agent_node(Value::Null, Some("ses_prev"), false, None, None, true).await;
    assert_eq!(
        execution.status,
        GraphRunStatus::Fail, // CM13: unreported
        "verdict must come from the cold fallback run"
    );
    assert!(argv.contains("--resume"), "the resume attempt ran first");
    assert!(
        argv.contains("# [SPEC]"),
        "the cold fallback ran and rendered the full spec"
    );
}

#[tokio::test]
async fn resume_skips_set_at_spawn_and_list_capture() {
    // Platform has BOTH a set-at-spawn flag and a session-list command, so
    // a cold start would either mint a uuid or diff a session list. On a
    // resume, neither may run: the run row must keep exactly the resumed id.
    let (execution, run, argv) = run_resume_agent_node(
        Value::Null,
        Some("ses_prev"),
        false,
        Some("--set"),
        Some("list"),
        false,
    )
    .await;
    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    assert_eq!(
        run.session_id.as_deref(),
        Some("ses_prev"),
        "resumed run keeps the resumed id — no set-at-spawn uuid, no listed id"
    );
    assert!(
        !argv.contains("--set"),
        "set-at-spawn flag must not be injected on a resumed spawn"
    );
}

#[tokio::test]
async fn cross_spec_resume_renders_new_spec_and_boundary_notice() {
    // RS3: the session being resumed was captured by a DIFFERENT spec (a
    // context-group handoff) — the resumed session has never seen THIS
    // spec, so unlike a same-spec bounce it must get the full spec
    // content plus a notice that the previous spec is done.
    let (execution, run, argv) =
        run_resume_agent_node(Value::Null, Some("ses_prev"), true, None, None, false).await;
    assert_eq!(execution.status, GraphRunStatus::Fail); // CM13: unreported
    assert!(argv.contains("--resume"), "still a genuine resume");
    assert!(argv.contains("ses_prev"), "the resumed id must be passed");
    assert!(
        argv.contains("# [SPEC]"),
        "a cross-spec resume must render the new spec's content"
    );
    assert!(
        argv.contains("Functional Requirements:\n- A"),
        "the rendered spec content must be THIS spec's own, not omitted"
    );
    assert!(
        argv.contains("finished and already committed"),
        "must state the previous spec is done"
    );
    assert!(
        !argv.contains("The full task context is already in your session history"),
        "the false same-session claim must never be sent on a cross-spec resume"
    );
    assert_eq!(run.session_id.as_deref(), Some("ses_prev"));
}

#[tokio::test]
async fn cross_spec_resume_ignores_node_level_resume_prompt_override() {
    // The boundary choice depends on runtime state (which spec captured
    // the resumed session) that a static per-node template cannot know,
    // so a cross-spec resume must use the engine's own template
    // regardless of any `resume_prompt` override configured on the node.
    let (_execution, _run, argv) = run_resume_agent_node(
        serde_json::json!({ "resume_prompt": "CUSTOM {{previous_feedback}}" }),
        Some("ses_prev"),
        true,
        None,
        None,
        false,
    )
    .await;
    assert!(
        !argv.contains("CUSTOM"),
        "a node-level resume_prompt override must be ignored on a cross-spec resume"
    );
    assert!(
        argv.contains("# [SPEC]"),
        "the engine's own cross-spec template must be used instead"
    );
}

/// CM13: an agent that never self-reports is unreported infra. This test
/// was originally about session resume (cold first visit, bounce-resume),
/// but CM13 reclassifies the agent's runs as infra because the bare
/// script never calls `graph_complete_node`. After retry exhaustion, the
/// agent counts as "no verdict" and the spec fails.
#[tokio::test]
async fn bounce_resumes_second_visit_after_cold_first_visit() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let argv_file = dir.path().join("argv.log");
    let counter = dir.path().join("counter");
    let script = write_argv_echo_cli(dir.path());
    let mut env = HashMap::new();
    env.insert(
        "ARGV_FILE".to_string(),
        argv_file.to_string_lossy().into_owned(),
    );
    env.insert("RESUME_FLAG".to_string(), "--resume".to_string());
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);

    db.insert_graph_node(&GraphNode {
        id: "node-impl".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "resume-cli", "infra_backoff_seconds": 0 }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
            id: "node-gate".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "gate".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": format!(
                    "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; [ \"$n\" -ge 2 ] && printf APPROVED || exit 1",
                    c = counter.display(),
                ),
                "success_condition": "exit_code_0"
            }),
            position: 2,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-impl-gate".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-impl".to_string(),
        to_node: "node-gate".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-gate-impl".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-gate".to_string(),
        to_node: "node-impl".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    let guard = HomeGuard::set(home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(guard);

    // CM13: the agent never self-reports, so every run is infra.
    // With retry_limit=2, the agent runs 3 times (initial + 2 retries).
    // After exhaustion, had_infra_crash=true, no pass edge → spec fails.
    let mut impl_runs: Vec<GraphNodeRun> = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == "node-impl")
        .collect();
    impl_runs.sort_by_key(|r| r.started_at);
    assert_eq!(
        impl_runs.len(),
        3,
        "CM13: unreported agent exhausts infra retries (initial + 2 retries)"
    );
    for run in &impl_runs {
        assert_eq!(
            run.status,
            GraphRunStatus::Fail,
            "CM13: every unreported agent run is infra-crash Fail"
        );
    }

    // The argv log still shows --set on the first invocation (set-at-spawn).
    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains("--set"),
        "the first (cold) visit sets a session id at spawn"
    );
}

#[tokio::test]
async fn grouped_spec_resumes_prior_siblings_session() {
    // RS3 positive handoff: spec-a cold-starts and captures a session;
    // spec-b in the same group resumes it on its first node run and records
    // the SAME session id — the ONE exception to RS2's "first visit cold".
    let (db, argv_file) =
        run_grouped_queue(&[("spec-a", Some("ctx")), ("spec-b", Some("ctx"))]).await;

    assert_eq!(
        db.get_graph_spec("spec-a").unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );
    assert_eq!(
        db.get_graph_spec("spec-b").unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );

    let sid_a = impl_session(&db, "spec-a").expect("spec-a cold-start captures a session");
    let sid_b = impl_session(&db, "spec-b").expect("spec-b records a session");
    assert_eq!(
        sid_b, sid_a,
        "the grouped sibling must continue — and record — spec-a's session"
    );

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        argv.contains("--resume") && argv.contains(&sid_a),
        "spec-b's first visit resumes spec-a's session by id"
    );
}

#[tokio::test]
async fn ungrouped_specs_never_cross_resume() {
    // RS7: with no group, each spec cold-starts — spec-b mints its OWN
    // set-at-spawn session and never touches spec-a's.
    let (db, argv_file) = run_grouped_queue(&[("spec-a", None), ("spec-b", None)]).await;

    let sid_a = impl_session(&db, "spec-a").expect("spec-a captures a session");
    let sid_b = impl_session(&db, "spec-b").expect("spec-b captures its own session");
    assert_ne!(
        sid_a, sid_b,
        "ungrouped specs must not share a session across the queue"
    );

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    assert!(
        !argv.contains("--resume"),
        "no resume flag may appear for an ungrouped queue"
    );
}

#[tokio::test]
async fn grouped_spec_resume_across_boundary_shows_new_spec_not_old() {
    // Reproduces the incident on graph 824de730-7fec-4031-800a-7933d2cf94c1,
    // group `rag`: spec 2's implementer resumed spec 1's session and, with
    // the old RESUME_PROMPT_DEFAULT claiming full context was already in
    // history, was never shown ANY spec at all — it reported PASS after
    // fourteen minutes describing spec 1's (already-committed) work,
    // the only work it had ever seen. The fix must show the resumed
    // session its OWN (spec 2's) content, plus a notice that spec 1 is
    // done, so it neither regurgitates spec 1 nor works blind.
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
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);

    let mut spec_1 = standalone_spec("rag-1", 1);
    spec_1.description = Some("SPEC-1-MARKER: bridge restart recovery".to_string());
    let mut spec_2 = standalone_spec("rag-2", 2);
    spec_2.description = Some("SPEC-2-MARKER: rag ingestion pipeline".to_string());
    db.insert_graph_spec(&spec_1).unwrap();
    db.insert_graph_spec(&spec_2).unwrap();
    insert_queue_with_grouped_members(
        &db,
        "queue-1",
        &[("rag-1", Some("rag")), ("rag-2", Some("rag"))],
    );

    // Graph-level agent node: both grouped members drain the same node id,
    // exactly like the incident's implementer node.
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
    // CB57: the script self-reports its Pass verdict so both specs
    // complete and the cross-boundary resume below is exercised.
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
    drop(guard);

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let invocations: Vec<&str> = argv.split("===\n").collect();
    assert_eq!(
        invocations.len(),
        3, // two real invocations + trailing empty split segment
        "expected exactly one cold spawn (spec 1) and one resumed spawn (spec 2)"
    );

    // Invocation 1: spec 1 cold-starts and gets its own content.
    assert!(invocations[0].contains("SPEC-1-MARKER"));

    // Invocation 2: spec 2 resumes spec 1's session, but must be shown
    // its OWN spec — never spec 1's — plus the boundary notice.
    let spec_2_invocation = invocations[1];
    assert!(
        spec_2_invocation.contains("--resume"),
        "spec 2 must resume spec 1's session"
    );
    assert!(
        spec_2_invocation.contains("SPEC-2-MARKER"),
        "the resumed session must be shown spec 2's OWN content — the bug \
             this reproduces showed it no spec content at all"
    );
    assert!(
        spec_2_invocation.contains("finished and already committed"),
        "must state spec 1 is done so its conclusions aren't restated"
    );
    assert!(
        !spec_2_invocation.contains("The full task context is already in your session history"),
        "the false same-session claim is exactly what caused the incident's \
             14-minute regurgitation and must never be sent on a cross-spec resume"
    );
}

#[tokio::test]
async fn grouped_reviewer_resume_across_boundary_gets_new_spec_not_stuck() {
    // Reproduces the second incident, group `daemon`: the REVIEWER node
    // resumed across a spec boundary and, with the old prompt, had only
    // spec 1 ("bridge restart recovery", already complete) in context —
    // it said so plainly: "I need the spec content to know what work to
    // do next." Same fix, different node role — a `review` node instead
    // of `impl`, proving the fix is node-role-agnostic.
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
    env.insert("SELF_REPORT_NODE_ID".to_string(), "node-review".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);

    let mut spec_1 = standalone_spec("daemon-1", 1);
    spec_1.description = Some("DAEMON-SPEC-1: bridge restart recovery".to_string());
    let mut spec_2 = standalone_spec("daemon-2", 2);
    spec_2.description = Some("DAEMON-SPEC-2: watchdog heartbeat timeout".to_string());
    db.insert_graph_spec(&spec_1).unwrap();
    db.insert_graph_spec(&spec_2).unwrap();
    insert_queue_with_grouped_members(
        &db,
        "queue-1",
        &[("daemon-1", Some("daemon")), ("daemon-2", Some("daemon"))],
    );

    db.insert_graph_node(&GraphNode {
        id: "node-review".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "review".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "resume-cli" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let guard = HomeGuard::set(home.path());
    // CB57: the script self-reports its Pass verdict so both specs
    // complete and the cross-boundary resume below is exercised.
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
    drop(guard);

    let argv = std::fs::read_to_string(&argv_file).unwrap();
    let invocations: Vec<&str> = argv.split("===\n").collect();
    assert_eq!(invocations.len(), 3, "one cold spawn, one resumed spawn");

    let reviewer_invocation = invocations[1];
    assert!(
        reviewer_invocation.contains("--resume"),
        "the reviewer must resume spec 1's session"
    );
    assert!(
        reviewer_invocation.contains("DAEMON-SPEC-2"),
        "the resumed reviewer must be shown spec 2's content — the incident's \
             reviewer had none and had to ask for it"
    );
    assert!(
        reviewer_invocation.contains("finished and already committed"),
        "must state spec 1 is done"
    );
    assert!(
        !reviewer_invocation.contains("The full task context is already in your session history"),
        "must not claim stale context is complete"
    );
}

#[tokio::test]
async fn run_agent_process_reports_spawn_failure_as_node_fail_not_hard_error() {
    // Simulates the E2BIG incident: the process fails to spawn. This
    // must come back as a failed node run (routed like any other node
    // failure) rather than an `Err` that would abort the whole graph.
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

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(execution.summary.contains("failed to spawn"));
    assert!(execution.output.get("error").is_some());
}

/// The `mimocode`/`mimo-auto` incident, reproduced exactly: a CLI that
/// fails to start its model, prints its complaint to stderr, produces
/// empty stdout, and still exits 0. This must never be recorded as
/// `Pass` — a downstream `pass` edge must not fire for a run that never
/// actually did anything.
#[tokio::test]
async fn run_agent_process_empty_stdout_zero_exit_is_fail_not_pass() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(
        dir.path(),
        "mimocode.sh",
        ">&2 printf 'Error: Unsupported model mimo-auto'\nexit 0",
    );
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        "empty stdout must be a Fail even though the process exited 0"
    );
    assert_eq!(
        execution.output.get("exit_code").and_then(Value::as_i64),
        Some(0)
    );
    assert_eq!(
        execution.output.get("no_output").and_then(Value::as_bool),
        Some(true),
        "the no-output reason must be distinguishable from a self-reported FAIL"
    );
    // The stderr text must be surfaced in a form a downstream node's
    // `(none)` fallback can act on, not silently dropped.
    let error_text = execution
        .output
        .get("error")
        .and_then(Value::as_str)
        .expect("no-output run must carry an error field");
    assert!(error_text.contains("Unsupported model mimo-auto"));
    assert_eq!(
        execution.output.get("stderr").and_then(Value::as_str),
        Some("Error: Unsupported model mimo-auto")
    );
    assert!(
        !execution.summary.to_lowercase().contains("reported"),
        "must not read as a self-report of any kind"
    );

    // CM13: a no_output run that never filed a verdict IS an infra crash —
    // the run never called graph_complete_node, so it is infrastructure
    // regardless of exit code or output. The retry gate (attempt < retry_limit)
    // still applies; this just means it qualifies for infra-crash retry.
    let run = GraphNodeRun {
        id: "run-test".to_string(),
        graph_id: "loop1".to_string(),
        spec_id: "spec1".to_string(),
        node_id: node.id.clone(),
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
    assert!(
        is_infra_crash(&node, &execution, &run, 0, 3, 60),
        "CM13: an empty-output zero-exit run that never reported is infra crash"
    );
}

/// CM7 pre-mortem guard: an `effort` the platform can't honour must land
/// in the run record, not vanish. `sample_strategy` has no
/// `effort_declaration`, so any effort is "not supported".
#[tokio::test]
async fn run_agent_process_records_effort_not_applied_when_unsupported() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "ok.sh", "printf ok");
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db,
        "run-effort",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        Some("high"),
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        execution
            .output
            .get("effort_not_applied")
            .and_then(Value::as_str),
        Some("platform 'test-cli' does not support effort"),
        "the non-application notice must be in the run record"
    );
    assert!(execution.output.get("effort_applied").is_none());
}

/// The mirror: when the platform accepts the value, the run record says so
/// and carries no non-application notice.
#[tokio::test]
async fn run_agent_process_records_effort_applied_when_supported() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "ok.sh", "printf ok");
    let mut strategy = sample_strategy(&script);
    strategy.effort_declaration = Some(crate::domain::cli_config::EffortDeclaration {
        form: Some("--effort".to_string()),
        values: vec!["low".to_string(), "high".to_string()],
    });
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db,
        "run-effort-ok",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        Some("high"),
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        execution
            .output
            .get("effort_applied")
            .and_then(Value::as_str),
        Some("high")
    );
    assert!(execution.output.get("effort_not_applied").is_none());
}

/// CB34: a `model` requested on a platform whose `model_flag` is blank
/// (antigravity) must land in the run record as an explicit not-applied
/// notice, naming platform and model — not vanish.
#[tokio::test]
async fn run_agent_process_records_model_not_applied_when_flag_blank() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "ok.sh", "printf ok");
    let mut strategy = sample_strategy(&script);
    strategy.model_flag = Some(String::new());
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db,
        "run-model-blank",
        &cli,
        &strategy,
        &node,
        "prompt",
        Some("claude-opus-4-8"),
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    let notice = execution
        .output
        .get("model_not_applied")
        .and_then(Value::as_str)
        .expect("model_not_applied must be in the run record");
    assert!(
        notice.contains("test-cli"),
        "notice names the platform: {notice}"
    );
    assert!(
        notice.contains("claude-opus-4-8"),
        "notice names the model: {notice}"
    );
}

/// The mirror: a real `model_flag` carries no not-applied notice.
#[tokio::test]
async fn run_agent_process_no_model_notice_when_flag_is_real() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(dir.path(), "ok.sh", "printf ok");
    let mut strategy = sample_strategy(&script);
    strategy.model_flag = Some("--model".to_string());
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db,
        "run-model-real",
        &cli,
        &strategy,
        &node,
        "prompt",
        Some("claude-opus-4-8"),
        None,
        "/tmp",
        1,
        None,
    )
    .await
    .unwrap();

    assert!(execution.output.get("model_not_applied").is_none());
}

/// CB43 (T1/T2): the stored `executed_model` is what the CLI argv would
/// receive — trimmed when selectable, `None` when the platform cannot
/// select a model or none was requested. A test that stored the raw
/// request would fail the blank-flag case below.
#[test]
fn cb43_resolved_model_for_run_gates_on_model_flag() {
    assert_eq!(
        resolved_model_for_run(Some("--model"), Some("opencode/big-pickle")),
        Some("opencode/big-pickle".to_string())
    );
    assert_eq!(
        resolved_model_for_run(Some("--model"), Some("  spaced  ")),
        Some("spaced".to_string())
    );
    // Blank/None flag (antigravity shape): the request is NOT stored.
    assert_eq!(resolved_model_for_run(Some(""), Some("m")), None);
    assert_eq!(resolved_model_for_run(Some("   "), Some("m")), None);
    assert_eq!(resolved_model_for_run(None, Some("m")), None);
    // No model requested: nothing to store regardless of flag.
    assert_eq!(resolved_model_for_run(Some("--model"), None), None);
    assert_eq!(resolved_model_for_run(Some("--model"), Some("  ")), None);
}

/// CB43: node dispatch resolves the pair without panicking, even for a
/// platform with no registry entry (stored as-is), and yields `(None,
/// None)` for nodes that dispatch no model.
#[test]
fn cb43_executed_pair_for_node_never_panics_or_invents() {
    let mut node = sample_agent_node();
    node.config = serde_json::json!({
        "platform": "cb43-unknown-platform",
        "model": "some-model",
    });
    let (platform, model) = executed_pair_for_node(&node);
    assert_eq!(platform.as_deref(), Some("cb43-unknown-platform"));
    assert_eq!(model.as_deref(), Some("some-model"));

    node.config = serde_json::json!({"command": "true"});
    assert_eq!(
        executed_pair_for_node(&node),
        (None, None),
        "a check node dispatches no model"
    );

    node.config = serde_json::json!({"platform": "  ", "model": "m"});
    assert_eq!(
        executed_pair_for_node(&node),
        (None, None),
        "a blank platform is no platform"
    );
}

/// C3: the 2026-08-13 `gitkit-composition` incident, reproduced with the
/// stderr the harness actually printed — exit 0, empty stdout, and a
/// warning that the workdir was untrusted so `.agents/` (MCP config) was
/// ignored. Must be classified as the specific `untrusted_workdir` cause,
/// not just the generic no-output case, so a human (or the resilience
/// node's own summary) can tell "the harness lost its tools" apart from
/// "the harness said nothing for some other reason".
#[tokio::test]
async fn run_agent_process_untrusted_workdir_stderr_is_classified_specifically() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let stderr_text = "Warning: /home/jheisonmblivecom/Projects/UniverLab/gitkit is not trusted; \
                            project configuration (.agents/) will be ignored. \
                            Re-run with --trust to trust this folder temporarily.";
    let script = write_member_script(
        dir.path(),
        "untrusted.sh",
        &format!(">&2 printf '%s' '{stderr_text}'\nexit 0"),
    );
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("no_output").and_then(Value::as_bool),
        Some(true),
        "existing no_output mechanism must still fire (this sharpens it, not replaces it)"
    );
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("untrusted_workdir"),
        "must join the failure_kind vocabulary alongside no_report"
    );
    let error_text = execution
        .output
        .get("error")
        .and_then(Value::as_str)
        .expect("must carry an error field");
    assert!(
        error_text.contains("not trusted"),
        "the matched stderr text must be visible in the report: {error_text}"
    );
    assert!(
        execution.summary.to_lowercase().contains("untrusted"),
        "summary must state the cause, not read as a generic empty response: {}",
        execution.summary
    );
}

/// Guard against over-matching: an unrelated stderr on an empty-stdout,
/// zero-exit run must stay the generic no-output case, never picking up
/// the `untrusted_workdir` cause just because stdout happened to be
/// empty.
#[tokio::test]
async fn run_agent_process_unrelated_stderr_stays_generic_no_output() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let script = write_member_script(
        dir.path(),
        "unrelated.sh",
        ">&2 printf 'Error: rate limited, try again later'\nexit 0",
    );
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(
        execution.output.get("no_output").and_then(Value::as_bool),
        Some(true)
    );
    assert!(
        execution.output.get("failure_kind").and_then(Value::as_str) == Some("unreported"),
        "CM13: unrelated stderr stays generic, carrying only the unreported marker"
    );
}

/// Harnesses warn about many things; an untrusted-workdir mention in
/// stderr alongside REAL stdout must never add the `untrusted_workdir`
/// cause — only the empty-stdout shape is diagnostic here.
/// CM13: the run still fails as unreported infra (the script never called
/// `graph_complete_node`); the warning changes nothing about that.
#[tokio::test]
async fn run_agent_process_untrusted_warning_with_real_output_still_passes() {
    let (dir, db) = test_db();
    let cli = Cli::new("test-cli");
    let stderr_text = "Warning: workdir is not trusted; project configuration will be ignored.";
    let script = write_member_script(
        dir.path(),
        "warned-but-worked.sh",
        &format!(">&2 printf '%s' '{stderr_text}'\nprintf 'all done'\nexit 0"),
    );
    let strategy = sample_strategy(&script);
    let node = sample_agent_node();

    let execution = run_agent_process(
        &db, "run-test", &cli, &strategy, &node, "prompt", None, None, "/tmp", 1, None,
    )
    .await
    .unwrap();

    assert_eq!(
        execution.status,
        GraphRunStatus::Fail,
        // CM13: unreported infra (the script never called
        // graph_complete_node); the warning neither downgrades nor rescues.
        "CM13: an unreported run is never a pass"
    );
    assert!(execution.output.get("no_output").is_none());
    assert_eq!(
        execution.output.get("failure_kind").and_then(Value::as_str),
        Some("unreported")
    );
}
