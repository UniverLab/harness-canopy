//! Part 18 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// CB82: an ensemble in flight when the flag is set starts no further
/// round-robin member. Breaks if the loop-head guard (3.6) is removed
/// (member rows appear — today the log shows "trying next" on shutdown).
#[tokio::test]
async fn shutdown_round_robin_starts_no_members() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::RoundRobin,
        &[("rr-a", "rr-a"), ("rr-b", "rr-b")],
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    let details = db.get_ensemble_details("ens1").unwrap().unwrap();

    engine.mark_shutting_down();
    let mut resumable_sessions = HashMap::new();
    let mut round_robin_pins = HashMap::new();
    let node_outputs = HashMap::new();
    let all_node_names: Vec<String> = Vec::new();
    let available = details.members.clone();
    let execution = engine
        .execute_ensemble_round_robin(
            &lp,
            &spec,
            &details,
            &available,
            None,
            1,
            &lp.workdir,
            false,
            &node_outputs,
            &all_node_names,
            &mut resumable_sessions,
            &mut round_robin_pins,
        )
        .await
        .unwrap();

    assert_eq!(
        execution
            .output
            .get("interrupted")
            .and_then(|v| v.as_bool()),
        Some(true),
        "interrupted walk must return the interrupted marker"
    );
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert!(
        runs.iter()
            .all(|r| r.node_id != "rr-a" && r.node_id != "rr-b"),
        "no member may start during shutdown, runs={runs:?}"
    );
    assert!(
        runs.iter().all(|r| r.node_id != "join1"),
        "no join row is written for an interrupted walk, runs={runs:?}"
    );
}

/// CB82: `run_was_terminated_out_of_band` treats the interrupted marker
/// like the terminated one, and still lets genuine fails route. A genuine
/// node output never sets either key (constraint: genuine fails keep
/// their fail edge).
#[test]
fn run_was_terminated_out_of_band_recognises_interrupted_marker() {
    fn run_with_output(output: serde_json::Value) -> GraphNodeRun {
        GraphNodeRun {
            id: "run-1".to_string(),
            graph_id: "wf-test".to_string(),
            spec_id: "spec-test".to_string(),
            node_id: "node-a".to_string(),
            status: GraphRunStatus::Fail,
            input: None,
            output: Some(output),
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        }
    }

    assert!(
        run_was_terminated_out_of_band(&run_with_output(
            serde_json::json!({"interrupted": true, "reason": "daemon shutdown"})
        )),
        "Fail+interrupted:true is out-of-band"
    );
    assert!(
        run_was_terminated_out_of_band(&run_with_output(
            serde_json::json!({"terminated": true, "reason": "spec reset"})
        )),
        "Fail+terminated:true still recognised (unchanged)"
    );
    assert!(
        !run_was_terminated_out_of_band(&run_with_output(
            serde_json::json!({"exit_code": 1, "command": "false"})
        )),
        "a genuine check failure must still route its fail edge"
    );
}

// ── CB84: session end on termination ──────────────────────────────

/// CB84 6d: `terminate_run_row` must invoke the platform's session-end
/// command with the run's session id (after `{session_id}` substitution)
/// and the run's workdir as cwd.  Uses a fake binary that records its argv
/// and cwd to a file we assert on — no real harness is involved.
#[cfg(unix)]
#[tokio::test]
async fn terminate_run_row_calls_session_end_script() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    // Fake binary: records cwd + every argv word to RECORD, then a final
    // `done=1` marker so the test can wait for a complete record.
    let scratch = tempdir().unwrap();
    let record = scratch.path().join("end_record.txt");
    let record_str = record.to_string_lossy().to_string();
    let fake_bin = scratch.path().join("fake-end-cli");
    std::fs::write(
            &fake_bin,
            format!(
                "#!/bin/sh\necho \"cwd=$(pwd)\" >> '{record_str}'\nfor arg in \"$@\"; do echo \"arg=$arg\" >> '{record_str}'; done\necho \"done=1\" >> '{record_str}'\n"
            ),
        )
        .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Config: fake platform with session_end_args pointing at the script.
    let config_toml = format!(
        r#"[[clis]]
name = "fake-end"
binary = "{}"
session_end_args = ["end", "--session-id", "{{session_id}}"]
"#,
        fake_bin.to_string_lossy()
    );
    std::fs::write(canopy_dir.join("config.toml"), config_toml).unwrap();

    // Serializes against the other `CANOPY_HOME_OVERRIDE` tests and
    // restores the previous value on drop.
    let _home = HomeGuard::set(home.path());

    // DB + graph + run with executed_platform and session_id set.
    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let spec = standalone_spec("sid-end", 1);
    db.insert_graph_spec(&spec).unwrap();
    let mut node = sample_agent_node();
    node.spec_id = Some(spec.id.clone());
    db.insert_graph_node(&node).unwrap();

    let run = GraphNodeRun {
        id: "run-end-test".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec.id,
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: Some("ses_abc".to_string()),
        executed_platform: Some("fake-end".to_string()),
        executed_model: None,
    };
    db.insert_graph_run(&run).unwrap();

    // Call terminate_run_row — spawns end_harness_session detached.
    let workdir = workdir_for_run(&db, &run);
    assert!(!workdir.is_empty(), "fixture must have a real workdir");
    terminate_run_row(&db, &run, "test termination", &workdir);

    // Wait for the detached task to write a complete record.
    let recorded = loop {
        if let Ok(recorded) = std::fs::read_to_string(&record) {
            if recorded.contains("done=1") {
                break recorded;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };

    assert!(
        recorded.contains("arg=--session-id") && recorded.contains("arg=ses_abc"),
        "session id must reach the end script: {}",
        recorded
    );
    assert!(
        recorded.contains(&format!("cwd={workdir}")),
        "script must run with the run's workdir as cwd: {}",
        recorded
    );
}

/// CB84 6d (negative): a platform with no `session_end_args` leaves no
/// session to end, so `terminate_run_row` runs no command at all. This
/// proves the positive test's script invocation is driven by the config,
/// not by `terminate_run_row` unconditionally.
#[cfg(unix)]
#[tokio::test]
async fn terminate_run_row_skips_session_end_without_config() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let scratch = tempdir().unwrap();
    let record = scratch.path().join("end_record.txt");
    let record_str = record.to_string_lossy().to_string();
    let fake_bin = scratch.path().join("fake-end-cli");
    std::fs::write(
        &fake_bin,
        format!("#!/bin/sh\necho \"ran=1\" >> '{record_str}'\n"),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // Same fake platform, but no `session_end_args` — the binary must
    // never even be invoked.
    let config_toml = format!(
        r#"[[clis]]
name = "fake-end"
binary = "{}"
"#,
        fake_bin.to_string_lossy()
    );
    std::fs::write(canopy_dir.join("config.toml"), config_toml).unwrap();

    let _home = HomeGuard::set(home.path());

    let (_dir, db, _engine, graph_id) = bare_graph_fixture().unwrap();
    let spec = standalone_spec("sid-end", 1);
    db.insert_graph_spec(&spec).unwrap();
    let mut node = sample_agent_node();
    node.spec_id = Some(spec.id.clone());
    db.insert_graph_node(&node).unwrap();

    let run = GraphNodeRun {
        id: "run-end-skip".to_string(),
        graph_id: graph_id.clone(),
        spec_id: spec.id,
        node_id: node.id.clone(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: Some("ses_abc".to_string()),
        executed_platform: Some("fake-end".to_string()),
        executed_model: None,
    };
    db.insert_graph_run(&run).unwrap();

    let workdir = workdir_for_run(&db, &run);
    terminate_run_row(&db, &run, "test termination", &workdir);

    // Give any (wrongly) spawned task ample time to run, then assert the
    // binary never wrote anything.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert!(
        !record.exists(),
        "no session-end command must run for a platform without session_end_args"
    );
}

/// CB84 outcome 1: when the id is not on the row, the title lookup finds the
/// session by `canopy-<run_id>` and the end command runs with that id — the
/// run output then carries `session_end: "ended <id>"`. Fails if the
/// detached task never records anything, or records a different outcome.
#[cfg(unix)]
#[tokio::test]
async fn terminate_run_row_records_session_ended_by_title() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let scratch = tempdir().unwrap();
    let record = scratch.path().join("end_record.txt");
    let fake_bin = write_fake_end_session_cli(
        scratch.path(),
        r#"[{"id":"ses_found","title":"canopy-run-title-end"}]"#,
        &record,
    );
    std::fs::write(
        canopy_dir.join("config.toml"),
        format!(
            r#"[[clis]]
name = "fake-end"
binary = "{}"
session_title_flag = "--title"
session_list_args = ["session", "list", "--format", "json"]
session_end_args = ["end", "--session-id", "{{session_id}}"]
"#,
            fake_bin.to_string_lossy()
        ),
    )
    .unwrap();
    let _home = HomeGuard::set(home.path());

    // No session id on the row — the id must be discovered by title.
    let (_dir, db, _engine, run, workdir) = session_end_run("run-title-end", None);
    terminate_run_row(&db, &run, "test termination", &workdir);

    let value = wait_for_session_end(&db, "run-title-end").await;
    assert_eq!(
        value,
        serde_json::json!("ended ses_found"),
        "the found session id must be reported as ended"
    );
    let recorded = std::fs::read_to_string(&record).unwrap();
    assert!(
        recorded.contains("arg=--session-id") && recorded.contains("arg=ses_found"),
        "the end command must run with the id found by title: {recorded}"
    );
}

/// CB84 outcome 2: a platform with no `session_end_args` records
/// `"not configured"` and never invokes its binary.
#[cfg(unix)]
#[tokio::test]
async fn terminate_run_row_records_not_configured_without_session_end_args() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let scratch = tempdir().unwrap();
    let record = scratch.path().join("end_record.txt");
    let fake_bin = write_fake_end_session_cli(scratch.path(), "[]", &record);
    std::fs::write(
        canopy_dir.join("config.toml"),
        format!(
            r#"[[clis]]
name = "fake-end"
binary = "{}"
"#,
            fake_bin.to_string_lossy()
        ),
    )
    .unwrap();
    let _home = HomeGuard::set(home.path());

    let (_dir, db, _engine, run, workdir) = session_end_run("run-not-configured", None);
    terminate_run_row(&db, &run, "test termination", &workdir);

    let value = wait_for_session_end(&db, "run-not-configured").await;
    assert_eq!(value, serde_json::json!("not configured"));
    assert!(
        !record.exists(),
        "no session-end command may run without session_end_args"
    );
}

/// CB84 outcome 3: a title lookup that matches nothing records `"not found"`
/// and invokes no end command.
#[cfg(unix)]
#[tokio::test]
async fn terminate_run_row_records_not_found_for_unmatched_title() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();

    let scratch = tempdir().unwrap();
    let record = scratch.path().join("end_record.txt");
    // The list is empty, so no title can match.
    let fake_bin = write_fake_end_session_cli(scratch.path(), "[]", &record);
    std::fs::write(
        canopy_dir.join("config.toml"),
        format!(
            r#"[[clis]]
name = "fake-end"
binary = "{}"
session_title_flag = "--title"
session_list_args = ["session", "list", "--format", "json"]
session_end_args = ["end", "--session-id", "{{session_id}}"]
"#,
            fake_bin.to_string_lossy()
        ),
    )
    .unwrap();
    let _home = HomeGuard::set(home.path());

    let (_dir, db, _engine, run, workdir) = session_end_run("run-no-title", None);
    terminate_run_row(&db, &run, "test termination", &workdir);

    let value = wait_for_session_end(&db, "run-no-title").await;
    assert_eq!(value, serde_json::json!("not found"));
    assert!(
        !record.exists(),
        "no end command may run when no session id is found"
    );
}

/// CB84 6e: a run that completes normally (pass/fail reported by the agent
/// via `graph_complete_node`) never triggers `end_harness_session`.  The
/// completion path only calls `update_graph_run_result` — neither
/// `terminate_run_row` nor `end_harness_session` may appear in it, so
/// completed sessions stay intact for warm-session groups that resume
/// them.
#[test]
fn normal_completion_does_not_end_session() {
    let source = include_str!("../../../../../daemon/handler_run_lifecycle.rs");
    let prod = source.split("mod tests {").next().unwrap_or(source);
    assert!(
        !prod.contains("terminate_run_row"),
        "normal completion path (graph_complete_node -> complete_node_impl) \
             must not call terminate_run_row — sessions stay intact for warm-session groups"
    );
    assert!(
        !prod.contains("end_harness_session"),
        "normal completion path must not call end_harness_session directly either — \
             sessions stay intact for warm-session groups"
    );
}

#[tokio::test]
async fn decision_confident_choice_is_taken() {
    let (dir, db) = test_db();
    let answer = serde_json::json!({
        "answers": {
            "route": {
                "type": "choice",
                "choice": "quota_with_reset",
                "confidence": 0.92,
                "probabilities": {
                    "quota_with_reset": 0.92,
                    "transient_glitch": 0.05,
                    "other": 0.03
                }
            }
        },
        "usage": {"tokens": 12}
    });
    let answer_str = serde_json::to_string(&answer).unwrap();
    let answer_file = dir.path().join("answer.json");
    std::fs::write(&answer_file, &answer_str).unwrap();
    let script = write_member_script(
        dir.path(),
        "decision-cli.sh",
        &format!("cat '{}'", answer_file.to_string_lossy()),
    );
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = router_decision_node(
        "r1",
        "spec-1",
        "decision-cli",
        &[
            ("quota_with_reset", "Quota was reset"),
            ("transient_glitch", "Transient glitch"),
            ("other", "Other"),
        ],
        "other",
        1,
        0.5,
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
        "run-decision-confident",
        "/tmp",
    )
    .await
    .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output.get("route").and_then(Value::as_str),
        Some("quota_with_reset")
    );
    assert_eq!(
        execution.output.get("mode").and_then(Value::as_str),
        Some("decision")
    );
    assert_eq!(
        execution
            .output
            .get("used_fallback")
            .and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        execution.output.get("confidence").and_then(Value::as_f64),
        Some(0.92)
    );
    let probs = execution
        .output
        .get("probabilities")
        .and_then(Value::as_object)
        .unwrap();
    assert_eq!(
        probs.get("quota_with_reset").and_then(Value::as_f64),
        Some(0.92)
    );
    assert_eq!(
        execution
            .output
            .get("usage")
            .and_then(|u| u.get("tokens"))
            .and_then(Value::as_u64),
        Some(12)
    );
}

#[tokio::test]
async fn decision_low_confidence_falls_back() {
    let (dir, db) = test_db();
    let answer = serde_json::json!({
        "answers": {
            "route": {
                "type": "choice",
                "choice": "quota_with_reset",
                "confidence": 0.2,
                "probabilities": {"quota_with_reset": 0.2, "transient_glitch": 0.1, "other": 0.7}
            }
        },
        "usage": {"tokens": 5}
    });
    let answer_str = serde_json::to_string(&answer).unwrap();
    let answer_file = dir.path().join("answer.json");
    std::fs::write(&answer_file, &answer_str).unwrap();
    let script = write_member_script(
        dir.path(),
        "decision-cli.sh",
        &format!("cat '{}'", answer_file.to_string_lossy()),
    );
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = router_decision_node(
        "r1",
        "spec-1",
        "decision-cli",
        &[
            ("quota_with_reset", "Quota was reset"),
            ("transient_glitch", "Transient glitch"),
            ("other", "Other"),
        ],
        "other",
        1,
        0.5,
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
        "run-decision-low-conf",
        "/tmp",
    )
    .await
    .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output.get("route").and_then(Value::as_str),
        Some("other")
    );
    assert_eq!(
        execution
            .output
            .get("used_fallback")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        execution.output.get("confidence").and_then(Value::as_f64),
        Some(0.2)
    );
}

#[tokio::test]
async fn decision_unknown_label_falls_back() {
    let (dir, db) = test_db();
    let answer = serde_json::json!({
        "answers": {
            "route": {
                "type": "choice",
                "choice": "nonsense",
                "confidence": 0.99,
                "probabilities": {"nonsense": 0.99}
            }
        },
        "usage": {"tokens": 3}
    });
    let answer_str = serde_json::to_string(&answer).unwrap();
    let answer_file = dir.path().join("answer.json");
    std::fs::write(&answer_file, &answer_str).unwrap();
    let script = write_member_script(
        dir.path(),
        "decision-cli.sh",
        &format!("cat '{}'", answer_file.to_string_lossy()),
    );
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = router_decision_node(
        "r1",
        "spec-1",
        "decision-cli",
        &[
            ("quota_with_reset", "Quota was reset"),
            ("transient_glitch", "Transient glitch"),
            ("other", "Other"),
        ],
        "other",
        1,
        0.0,
    );
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution =
        execute_router_node(&db, &lp, &spec, &node, None, "run-decision-unknown", "/tmp")
            .await
            .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output.get("route").and_then(Value::as_str),
        Some("other")
    );
    assert_eq!(
        execution
            .output
            .get("used_fallback")
            .and_then(Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn decision_invalid_json_falls_back() {
    let (dir, db) = test_db();
    let script = write_member_script(dir.path(), "decision-cli.sh", "printf 'not json'");
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = router_decision_node(
        "r1",
        "spec-1",
        "decision-cli",
        &[
            ("quota_with_reset", "Quota was reset"),
            ("transient_glitch", "Transient glitch"),
            ("other", "Other"),
        ],
        "other",
        1,
        0.0,
    );
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution =
        execute_router_node(&db, &lp, &spec, &node, None, "run-decision-invalid", "/tmp")
            .await
            .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution.output.get("route").and_then(Value::as_str),
        Some("other")
    );
    assert_eq!(
        execution
            .output
            .get("used_fallback")
            .and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        execution.output.get("confidence").and_then(Value::as_f64),
        Some(0.0)
    );
}

#[tokio::test]
async fn decision_nonzero_exit_is_node_failure() {
    let (dir, db) = test_db();
    let script = write_member_script(dir.path(), "decision-cli.sh", "exit 3");
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = router_decision_node(
        "r1",
        "spec-1",
        "decision-cli",
        &[
            ("quota_with_reset", "Quota was reset"),
            ("transient_glitch", "Transient glitch"),
            ("other", "Other"),
        ],
        "other",
        1,
        0.0,
    );
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution = execute_router_node(&db, &lp, &spec, &node, None, "run-decision-exit3", "/tmp")
        .await
        .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(
        execution.output.get("route").is_none(),
        "a non-zero exit must not carry a chosen route"
    );
}

#[tokio::test]
async fn decision_process_timeout_is_node_failure() {
    let (dir, db) = test_db();
    // A script that sleeps longer than the node's timeout.
    let script = write_member_script(dir.path(), "decision-cli.sh", "sleep 30");
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = {
        let mut n = router_decision_node(
            "r1",
            "spec-1",
            "decision-cli",
            &[
                ("quota_with_reset", "Quota was reset"),
                ("transient_glitch", "Transient glitch"),
                ("other", "Other"),
            ],
            "other",
            1,
            0.0,
        );
        // timeout_minutes: 0 → 0*60=0s fires immediately.
        n.config["timeout_minutes"] = Value::from(0);
        n
    };
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution =
        execute_router_node(&db, &lp, &spec, &node, None, "run-decision-timeout", "/tmp")
            .await
            .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(
        execution.output.get("route").is_none(),
        "a timeout must not carry a chosen route"
    );
}

#[tokio::test]
async fn decision_request_file_holds_criteria_and_rendered_state() {
    let (dir, db) = test_db();
    // The stub script copies stdin to a known path so we can inspect it.
    let capture_path = dir.path().join("captured_stdin.json");
    let capture_path_str = capture_path.to_string_lossy().to_string();
    let script_body = format!("cat > '{capture_path_str}'");
    let script = write_member_script(dir.path(), "decision-cli.sh", &script_body);
    let fake_home = setup_decision_cli_home(&[("decision-cli", &script, "-p")]);
    let node = {
        let mut n = router_decision_node(
            "r1",
            "spec-1",
            "decision-cli",
            &[
                ("quota_with_reset", "Quota was reset"),
                ("transient_glitch", "Transient glitch"),
                ("other", "Other"),
            ],
            "other",
            1,
            0.0,
        );
        n.config["prompt_template"] =
            Value::String("FAIL? {{previous_output}} [{{spec_name}}]".to_string());
        n
    };
    let previous_output = serde_json::json!({"err": "quota reset at 12:00"});
    let lp = decision_graph();
    let spec = decision_spec("spec-1");

    let _home = HomeGuard::set(fake_home.path());
    let execution = execute_router_node(
        &db,
        &lp,
        &spec,
        &node,
        Some(&previous_output),
        "run-decision-request-file",
        "/tmp",
    )
    .await
    .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    let captured = std::fs::read_to_string(&capture_path).unwrap();
    let parsed: Value = serde_json::from_str(&captured).unwrap();
    let criteria = parsed
        .get("questions")
        .and_then(|q| q.get("route"))
        .and_then(|r| r.get("criteria"))
        .and_then(Value::as_object)
        .unwrap();
    assert_eq!(criteria.len(), 3);
    assert!(criteria.contains_key("quota_with_reset"));
    assert!(criteria.contains_key("transient_glitch"));
    assert!(criteria.contains_key("other"));
    assert_eq!(
        criteria.get("quota_with_reset").and_then(Value::as_str),
        Some("Quota was reset")
    );
    let instructions = parsed
        .get("questions")
        .and_then(|q| q.get("route"))
        .and_then(|r| r.get("instructions"))
        .and_then(Value::as_str)
        .unwrap();
    assert!(instructions.contains("FAIL?"));
    let state = parsed.get("state").and_then(Value::as_str).unwrap();
    assert!(state.contains("quota reset at 12:00"));
    assert!(state.contains("spec-1"));
}

#[test]
fn decision_platform_without_invocation_is_refused_at_write_time() {
    // A fake home where the platform has no decision_args.
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "no-dec-cli".to_string(),
            binary: "/bin/echo".to_string(),
            headless_mode: String::new(),
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
            decision_args: None,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let config_json = serde_json::json!({
        "routes": [
            {"label": "a", "description": "A"},
            {"label": "b", "description": "B"}
        ],
        "fallback": "b",
        "platform": "no-dec-cli",
        "decision": true,
    });
    let err = crate::daemon::handler_nodes::validate_node_config(
        crate::domain::graphs::GraphNodeKind::Router,
        &config_json,
    )
    .unwrap_err();
    drop(_home);

    assert!(
        err.contains("no-dec-cli"),
        "error must name the platform: {err}"
    );
    assert!(
        err.contains("decision-model invocation"),
        "error must mention decision-model invocation: {err}"
    );
}

#[test]
fn resolve_decision_route_boundary_equality_passes() {
    let routes = vec![
        RouterRoute {
            label: "a".to_string(),
            description: "A".to_string(),
        },
        RouterRoute {
            label: "b".to_string(),
            description: "B".to_string(),
        },
    ];
    let (route, used_fallback) = resolve_decision_route(Some("a"), 0.5, &routes, "b", 0.5);
    assert_eq!(route, "a");
    assert!(!used_fallback);
}

#[test]
fn parse_decision_answer_defaults_probabilities_and_usage() {
    let stdout = r#"{"answers":{"route":{"type":"choice","choice":"x","confidence":0.8}}}"#;
    let ans = parse_decision_answer(stdout).unwrap();
    assert_eq!(ans.choice, "x");
    assert_eq!(ans.confidence, 0.8);
    assert!(ans.probabilities.is_empty());
    assert_eq!(ans.usage, Value::Null);
}

#[test]
fn build_decision_argv_contains_decision_args_and_model_flag() {
    let strategy = crate::domain::cli_strategy::CliStrategy {
        binary: "/usr/local/bin/cmd".to_string(),
        headless_mode: String::new(),
        model_flag: Some("-m".to_string()),
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        prompt_via_stdin: true,
        session_id_set_flag: None,
        session_list_cmd: None,
        session_list_format_args: None,
        session_id_pattern: None,
        session_title_flag: None,
        session_list_args: None,
        session_end_args: None,
        session_resume_cmd: None,
        trust_flag: None,
        invocation_template: None,
        effort_declaration: None,
        decision_args: Some("-p".to_string()),
        identity: None,
    };
    let (binary, argv) = build_decision_argv(&strategy, Some("typesafe/jev")).unwrap();
    assert_eq!(binary, std::path::PathBuf::from("/usr/local/bin/cmd"));
    assert!(argv.contains(&"-p".to_string()));
    let m_pos = argv.iter().position(|a| a == "-m").unwrap();
    assert_eq!(argv[m_pos + 1], "typesafe/jev");
}

#[test]
fn build_decision_argv_omits_model_flag_when_blank() {
    let strategy = crate::domain::cli_strategy::CliStrategy {
        binary: "/usr/local/bin/cmd".to_string(),
        headless_mode: String::new(),
        model_flag: Some(String::new()),
        supports_working_dir: false,
        working_dir_flag: None,
        env_vars: std::collections::HashMap::new(),
        prompt_via_stdin: true,
        session_id_set_flag: None,
        session_list_cmd: None,
        session_list_format_args: None,
        session_id_pattern: None,
        session_title_flag: None,
        session_list_args: None,
        session_end_args: None,
        session_resume_cmd: None,
        trust_flag: None,
        invocation_template: None,
        effort_declaration: None,
        decision_args: Some("-p".to_string()),
        identity: None,
    };
    let (_binary, argv) = build_decision_argv(&strategy, Some("typesafe/jev")).unwrap();
    assert!(!argv.iter().any(|a| a == "typesafe/jev"));
}

#[tokio::test]
async fn agent_mode_router_unchanged_without_decision_flag() {
    let (dir, db) = test_db();
    let script = write_member_script(dir.path(), "router-agent-mode.sh", "printf 'billing\\n'");
    let fake_home = setup_multi_cli_home(&[("router-agent-mode-cli", &script)]);
    let node = router_node(
        "r1",
        "spec-1",
        "router-agent-mode-cli",
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
        "run-agent-mode-unchanged",
        "/tmp",
    )
    .await
    .unwrap();
    drop(_home);

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert!(
        execution.output.get("mode").is_none(),
        "agent-mode router output must not carry a 'mode' key"
    );
}

// ── CT28: agent nodes stream; snapshot-then-delete ───────────────

/// CT28: an agent node whose fake CLI prints 3 lines with 200ms sleeps
/// between them has ≥ 1 `graph_run_output` row before it exits, and after
/// it exits the run carries the snapshot and zero chunk rows.
#[tokio::test]
async fn agent_node_streams_output_to_live_tail() {
    let (dir, db, _engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let script = write_member_script(
        dir.path(),
        "agent-stream.sh",
        "#!/bin/sh\n\
             printf 'line one\\n'\n\
             sleep 0.2\n\
             printf 'line two\\n'\n\
             sleep 0.2\n\
             printf 'line three\\n'\n\
             exit 0\n",
    );
    let node = GraphNode {
        id: "node-agent-stream".to_string(),
        spec_id: Some("spec-test".to_string()),
        graph_id: None,
        name: "stream-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-agent-stream".to_string(),
        graph_id: graph_id.clone(),
        spec_id: "spec-test".to_string(),
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
    })
    .unwrap();
    // A chunk that predates the child. Completion must delete it; the
    // snapshot must carry only the streamed bytes, never this sentinel.
    db.append_graph_run_output("run-agent-stream", "stdout", "PRE-EXISTING-CHUNK\n")
        .unwrap();

    let cli = Cli::new("test-cli");
    let strategy = sample_strategy(&script);
    let run_fut = crate::graph_engine::agent_spawn::run_agent_process(
        &db,
        "run-agent-stream",
        &cli,
        &strategy,
        &node,
        "prompt",
        None,
        None,
        "/tmp",
        1,
        None,
    );
    tokio::pin!(run_fut);

    let mut saw_chunk = false;
    for _ in 0..100 {
        if tokio::time::timeout(std::time::Duration::from_millis(100), &mut run_fut)
            .await
            .is_ok()
        {
            break;
        }
        let (stdout, _) = db.get_graph_run_tail("run-agent-stream", 1000).unwrap();
        if stdout.contains("line one") {
            saw_chunk = true;
            break;
        }
    }
    assert!(
        saw_chunk,
        "agent node must stream output to graph_run_output while running"
    );
    let execution = run_fut.await.unwrap();

    // CM13: an agent node with no self-report is a Fail regardless of exit
    // code — but the streaming path must still have captured every byte.
    assert_eq!(execution.status, GraphRunStatus::Fail);

    // After completion: the run carries the snapshot and zero chunk rows.
    // The raw count is asserted rather than inferred from
    // `get_graph_run_tail`, which serves the same text from leftover
    // chunks — this assertion fails if the tee path never snapshots or
    // never deletes.
    let chunks = db
        .count_graph_run_output_chunks("run-agent-stream")
        .unwrap();
    assert_eq!(
        chunks, 0,
        "completion must delete the run's graph_run_output chunk rows"
    );
    // With zero chunk rows, `get_graph_run_tail` can only be served by the
    // snapshot columns written at completion.
    let (stdout, _) = db.get_graph_run_tail("run-agent-stream", 1000).unwrap();
    assert!(
        stdout.contains("line three"),
        "snapshot tail must serve completed output: {stdout}"
    );
    assert!(
        !stdout.contains("PRE-EXISTING-CHUNK"),
        "snapshot must carry only the streamed bytes, not stale chunks: {stdout}"
    );

    // The execution output carries stdout/stderr; the streaming path must
    // not have lost any bytes vs. the old wait_with_output contract.
    let stdout = execution.output.get("stdout").unwrap().as_str().unwrap();
    assert_eq!(stdout, "line one\nline two\nline three");
}

/// CT28: a timed-out agent node still yields TimedOut, and its partial
/// output is in the snapshot.
#[tokio::test]
async fn agent_node_timeout_snapshots_partial_output() {
    let (dir, db, _engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let script = write_member_script(
        dir.path(),
        "agent-sleeper.sh",
        "#!/bin/sh\n\
             printf 'partial before timeout\\n'\n\
             sleep 600\n",
    );
    let node = GraphNode {
        id: "node-agent-timeout".to_string(),
        spec_id: Some("spec-test".to_string()),
        graph_id: None,
        name: "timeout-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-agent-timeout".to_string(),
        graph_id: graph_id.clone(),
        spec_id: "spec-test".to_string(),
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
    })
    .unwrap();
    // A chunk that predates the child; the timeout path must delete it and
    // snapshot only the partial streamed bytes.
    db.append_graph_run_output("run-agent-timeout", "stdout", "PRE-EXISTING-CHUNK\n")
        .unwrap();

    let cli = Cli::new("test-cli");
    let strategy = sample_strategy(&script);
    let execution = crate::graph_engine::agent_spawn::run_agent_process(
        &db,
        "run-agent-timeout",
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

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(
        execution.summary.contains("timed out"),
        "summary: {}",
        execution.summary
    );

    // Partial output is in the snapshot, and the chunks are gone: the raw
    // count fails if the timeout path neither snapshots nor deletes.
    let chunks = db
        .count_graph_run_output_chunks("run-agent-timeout")
        .unwrap();
    assert_eq!(
        chunks, 0,
        "timeout must delete the run's graph_run_output chunk rows"
    );
    let (stdout, _) = db.get_graph_run_tail("run-agent-timeout", 1000).unwrap();
    assert!(
        stdout.contains("partial before timeout"),
        "timeout snapshot must carry partial output: {stdout}"
    );
    assert!(
        !stdout.contains("PRE-EXISTING-CHUNK"),
        "timeout snapshot must carry only the streamed bytes: {stdout}"
    );
}

/// CT28: `CliProcessOutcome::Finished.stdout` equals the old
/// `wait_with_output` result for a fixture with trailing whitespace.
#[tokio::test]
async fn agent_stdout_trim_matches_previous_semantics() {
    let (dir, db, _engine, graph_id, _spec_id) = graph_fixture().unwrap();
    let script = write_member_script(
        dir.path(),
        "agent-trim.sh",
        "#!/bin/sh\n\
             printf 'hello   \\n\\n'\n\
             exit 0\n",
    );
    let node = GraphNode {
        id: "node-agent-trim".to_string(),
        spec_id: Some("spec-test".to_string()),
        graph_id: None,
        name: "trim-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-agent-trim".to_string(),
        graph_id: graph_id.clone(),
        spec_id: "spec-test".to_string(),
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
    })
    .unwrap();

    let cli = Cli::new("test-cli");
    let strategy = sample_strategy(&script);

    // The pre-CT28 contract: `wait_with_output`, converted and trimmed in
    // one shot — what every downstream parser received before CT28. Run
    // the same fixture through the no-tee path for a real baseline.
    let baseline = match crate::graph_engine::agent_spawn::spawn_and_wait_cli_process(
        &strategy,
        "prompt",
        None,
        None,
        "/tmp",
        1,
        None,
        None,
        false,
        None,
        |_pid| {},
        None,
        None,
        false,
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => panic!("no-tee baseline must spawn: {}", error.message),
    };
    let (old_stdout, old_stderr) = match baseline {
        crate::graph_engine::agent_spawn::CliProcessOutcome::Finished {
            stdout, stderr, ..
        } => (stdout, stderr),
        crate::graph_engine::agent_spawn::CliProcessOutcome::TimedOut => {
            panic!("the fixture exits immediately; it must not time out")
        }
    };

    // CT28: the same fixture streamed through `spawn_output_reader` into
    // the buffers, then trimmed. Must be byte-identical to the baseline —
    // this is the assertion that fails if the streamed path trims (or
    // accumulates) differently from `wait_with_output`.
    let execution = crate::graph_engine::agent_spawn::run_agent_process(
        &db,
        "run-agent-trim",
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
        execution.output.get("stdout").unwrap().as_str().unwrap(),
        old_stdout,
        "streamed stdout must equal the old wait_with_output result"
    );
    assert_eq!(
        execution.output.get("stderr").unwrap().as_str().unwrap(),
        old_stderr,
        "streamed stderr must equal the old wait_with_output result"
    );
    assert_eq!(
        old_stdout, "hello",
        "trailing whitespace is trimmed, as before"
    );
}

/// Invalid mutex names on a node config are refused naming the node and the
/// value — uppercase, leading digits, bad characters, and empties alike —
/// while valid lists (and an absent key) pass.
#[test]
fn invalid_mutex_names_node_refused_naming_node_and_value() {
    use crate::daemon::handler_validate::validate_node_mutex_config;

    for bad in [
        "CPU",
        "9lives",
        "has space",
        "under_score",
        "",
        "way-too-long-resource-name-for-a-mutex",
    ] {
        let config = serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0",
            "mutex": [bad],
        });
        let err = validate_node_mutex_config("gates", &config).unwrap_err();
        assert!(
            err.contains("gates") && err.contains(bad),
            "refusal must name the node and the value: {err}"
        );
    }
    // A misshaped `mutex` (not a list of strings) is refused the same way.
    let err =
        validate_node_mutex_config("gates", &serde_json::json!({"mutex": "cpu"})).unwrap_err();
    assert!(err.contains("gates"), "refusal must name the node: {err}");

    // Valid names pass, alone and combined; absent stays valid.
    for good in [
        serde_json::json!({"mutex": ["cpu"]}),
        serde_json::json!({"mutex": ["cpu", "gpu-1", "a", "z9-0-x"]}),
        serde_json::json!({"mutex": []}),
        serde_json::json!({}),
    ] {
        validate_node_mutex_config("gates", &good)
            .unwrap_or_else(|err| panic!("valid mutex config refused: {err}"));
    }
}
