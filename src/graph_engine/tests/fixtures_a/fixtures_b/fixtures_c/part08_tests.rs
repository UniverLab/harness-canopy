//! Part 08 of graph-engine `tests` (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

// ── N1: graph lifecycle notifications ──────────────────────────────────

#[tokio::test]
async fn graph_engine_notifies_started_spec_completed_and_finished_on_success() {
    // A retrying check (self-graph on fail) must not spam a
    // spec-completed notification per attempt — only once, when the
    // spec actually reaches `completed`.
    let (dir, db, engine, notifications, graph_id, spec_id) = graph_fixture_with_mock().unwrap();
    let counter = dir.path().join("counter");

    db.insert_graph_node(&GraphNode {
            id: "node-check".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({
                "command": format!(
                    "n=$(cat \"{counter}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{counter}\"; [ \"$n\" -ge 3 ] && printf APPROVED || exit 1",
                    counter = counter.display(),
                ),
                "success_condition": "exit_code_0"
            }),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-self".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "node-check".to_string(),
        to_node: "node-check".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        notifications.events(),
        vec![
            RecordedNotification::GraphStarted {
                graph_name: "Graph".to_string(),
                spec_count: 1,
                resumed: false,
                first_pending: Some("Spec".to_string()),
            },
            RecordedNotification::SpecCompleted {
                graph_name: "Graph".to_string(),
                spec_name: "Spec".to_string(),
                done: 1,
                total: 1,
                next_pending: None,
            },
            RecordedNotification::GraphFinishedCompleted {
                graph_name: "Graph".to_string(),
                done: 1,
                total: 1,
                hook_launched: false,
            },
        ],
        "exactly one start, one spec-completed (not one per retry), and one finish notification"
    );
}

#[tokio::test]
async fn graph_engine_notifies_failed_variant_with_failing_spec_name() {
    let (_dir, _db, engine, notifications, graph_id, spec_id) = graph_fixture_with_mock().unwrap();

    _db.insert_graph_node(&GraphNode {
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
        .run_graph(graph_id, None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        notifications.events(),
        vec![
            RecordedNotification::GraphStarted {
                graph_name: "Graph".to_string(),
                spec_count: 1,
                resumed: false,
                first_pending: Some("Spec".to_string()),
            },
            RecordedNotification::GraphFinishedFailed {
                graph_name: "Graph".to_string(),
                spec_name: "Spec".to_string(),
            },
        ],
        "a spec that never completes must not fire spec-completed, only start + failed finish"
    );
}

#[tokio::test]
async fn graph_engine_notify_blocked_fires_graph_finished_blocked() {
    let (_dir, _db, engine, notifications, graph_id, _spec_id) = graph_fixture_with_mock().unwrap();

    engine
        .notify_blocked(&graph_id, "needs human review")
        .unwrap();

    assert_eq!(
        notifications.events(),
        vec![RecordedNotification::GraphFinishedBlocked {
            graph_name: "Graph".to_string(),
            summary: "needs human review".to_string(),
        }],
    );
}

// ── B11: check nodes run under a non-login shell ─────────────────────

#[tokio::test]
async fn shell_command_runs_check_commands_with_sh_c_semantics() {
    let mut process = shell_command("printf ok && exit 0");
    let output = process.output().await.unwrap();

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
}

#[tokio::test]
async fn shell_command_does_not_source_a_profile_with_bashisms() {
    // A login shell (`sh -l`) sources `~/.profile` before running the
    // command; a non-login `sh -c` never does. Point HOME at a profile
    // containing a bashism (`[[ ... ]]`, which dash chokes on with
    // `sh: N: [[: not found`) and confirm it never gets read: no stderr
    // noise, and the command's own exit code is unaffected.
    let fake_home = tempdir().unwrap();
    std::fs::write(fake_home.path().join(".profile"), "[[ x ]]\n").unwrap();

    let mut process = shell_command("exit 0");
    process.env("HOME", fake_home.path());
    let output = process.output().await.unwrap();

    assert!(output.status.success());
    assert!(
        output.stderr.is_empty(),
        "expected no stderr noise from a bashism in ~/.profile, got: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── B12: process-group kill on all abnormal ends ────────────────────

/// Iteration budget exhaustion must terminate any in-flight child
/// processes and finalize all runs. A check node that always fails
/// graphs back to itself via a self-graph edge until the per-node
/// iteration budget (`graphs.max_iterations_per_node`) is hit.
#[cfg(unix)]
#[tokio::test]
async fn iteration_budget_exhaustion_kills_inflight_child() {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    // This node always fails. A self-graph edge routes its failure
    // back to itself, forcing retries until the budget is exhausted.
    db.insert_graph_node(&GraphNode {
        id: "flaky".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "flaky".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 0.2; exit 1",
            "success_condition": "exit_code_0",
            "timeout_seconds": 60,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // Self-graph: failure routes back to the same node for retry.
    db.insert_graph_edge(&GraphEdge {
        id: "self-graph".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "flaky".to_string(),
        to_node: "flaky".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    // The spec must have failed on budget exhaustion.
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Failed);

    // All runs for this spec must be finalized (no longer `running`).
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(
        runs.len(),
        crate::domain::settings::default_int("graphs.max_iterations_per_node") as usize
    );
    assert!(
        runs.iter().all(|r| r.status != GraphRunStatus::Running),
        "no run should still be running after budget exhaustion"
    );
    // CB7: el blocker debe ser visible en el último run
    let last_run = runs.last().expect("should have at least one run");
    let blocker = last_run
        .output
        .as_ref()
        .and_then(|o| o.get("blocker"))
        .and_then(|v| v.as_str())
        .expect("blocker must be set on the last run after iteration budget exhaustion");
    assert!(
        blocker.contains(&spec.name)
            && blocker.contains("agotó")
            && blocker.contains(
                &crate::domain::settings::default_int("graphs.max_iterations_per_node").to_string()
            ),
        "blocker must name the spec, mention exhaustion, and state the limit; got: {}",
        blocker
    );
}

/// CM52: the per-node iteration ceiling is read from
/// `graphs.max_iterations_per_node` at each check, not a hardcoded 5.
/// With the setting at 2 the engine blocks on the third bounce (two
/// attempts run). The default-config run still blocks on the sixth,
/// covered by `iteration_budget_exhaustion_kills_inflight_child`.
#[cfg(unix)]
#[tokio::test]
async fn iteration_ceiling_reads_the_setting() {
    let config_dir = tempdir().unwrap();
    let mut config = crate::domain::canopy_config::CanopyConfig::default();
    config.graphs.max_iterations_per_node = 2;
    config.save_for_tests(config_dir.path()).unwrap();

    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let engine = engine.with_canopy_dir(config_dir.path().to_path_buf());

    db.insert_graph_node(&GraphNode {
        id: "flaky".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "flaky".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0",
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "self-graph".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "flaky".to_string(),
        to_node: "flaky".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Fail,
    })
    .unwrap();

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Failed);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    assert_eq!(
        runs.len(),
        2,
        "ceiling 2 must block on the third bounce, not the sixth"
    );
}

/// An agent node's timeout must kill the spawned OS process, not just
/// mark the run failed. Uses `sh -c "sleep 5; touch <marker>"` as a
/// stand-in for a hung agent CLI (a real long-running child process,
/// exercised through the exact same `run_agent_process` code path a
/// real agent CLI goes through) with an immediate timeout (agent
/// timeouts are minute-granular, so `0` is the only way to force one
/// without actually waiting a minute): if the timeout kill didn't
/// happen, the marker would appear ~5s later; if it did, the process is
/// gone long before that and the marker never appears.
#[cfg(unix)]
#[tokio::test]
async fn agent_timeout_kills_child_process() {
    let (dir, db, _engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("agent_survived");

    let node = GraphNode {
        id: "agent-timeout".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "agent-timeout".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    let run_id = "run-agent-timeout".to_string();
    db.insert_graph_run(&GraphNodeRun {
        id: run_id.clone(),
        graph_id: graph_id.clone(),
        spec_id: spec_id.clone(),
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
    let mut strategy = sample_strategy("sh");
    strategy.headless_mode = "-c".to_string();
    let prompt = format!("sleep 5; touch \"{}\"", marker.display());

    let result = run_agent_process(
        &db, &run_id, &cli, &strategy, &node, &prompt, None, None, "/tmp", 0, None,
    )
    .await;
    // B28: a timeout resolves as a failed `NodeExecution`, not a hard
    // error — it must be routable through the graph's fail edge rather
    // than aborting the whole spec.
    let execution = result.expect("a timed-out agent process must not be a hard error");
    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert_eq!(execution.output["error"], "timed out");

    // Wait out the grace period (plus a margin) before checking — the
    // kill is `SIGTERM` now, `SIGKILL` after `KILL_GRACE` on a detached
    // task, and either one reaps a plain `sh`/`sleep` well within that
    // window since neither ignores `SIGTERM`.
    tokio::time::sleep(KILL_GRACE + std::time::Duration::from_secs(2)).await;

    assert!(
        !marker.exists(),
        "agent process should have been killed on timeout; marker file should not exist"
    );
}

/// CM49 req 6: a crew-bound agent node resolves its pair at spawn from the
/// crew's order — the first member the roster reports available — and the
/// run row records that resolved pair.
#[tokio::test]
async fn crew_bound_node_picks_first_available_member() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "crew-a",
            &write_self_reporting_member_script(
                dir.path(),
                "crew-a.sh",
                "printf ok",
                &db_path,
                "node-crew",
                "pass",
                None,
            ),
        ),
        (
            "crew-b",
            &write_self_reporting_member_script(
                dir.path(),
                "crew-b.sh",
                "printf ok",
                &db_path,
                "node-crew",
                "pass",
                None,
            ),
        ),
    ]);
    db.crew_create(
        "impl",
        None,
        &[test_crew_member("crew-a"), test_crew_member("crew-b")],
    )
    .unwrap();
    // `crew-a` (the first member) is unavailable on the roster, so the
    // node must resolve to `crew-b`.
    db.roster_upsert(crate::db::roster_entries::NewRosterEntry {
        platform: "crew-a",
        model_pattern: None,
        state: crate::db::roster_entries::RosterState::Unavailable,
        reason: Some("quota"),
        until: None,
    })
    .unwrap();

    db.insert_graph_node(&GraphNode {
        id: "node-crew".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "crew-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "crew": "impl",
            "prompt_template": "ignored by the test script",
            "timeout_minutes": 5,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    insert_crew_done_node(&db, &spec_id);

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let run = runs.iter().find(|r| r.node_id == "node-crew").unwrap();
    assert_eq!(run.status, GraphRunStatus::Pass, "{:?}", run.output);
    assert_eq!(
        run.executed_platform.as_deref(),
        Some("crew-b"),
        "the first AVAILABLE crew member runs, skipping the unavailable one"
    );
}

/// CM49 req 6: an infra crash on a crew-bound node rotates to the NEXT
/// available crew member (wrapping), and the retry's run names it.
#[tokio::test]
async fn crew_node_infra_retry_rotates_to_next_member() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "crew-a",
            &write_member_script(dir.path(), "crew-a.sh", "exit 1"),
        ),
        (
            "crew-b",
            &write_self_reporting_member_script(
                dir.path(),
                "crew-b.sh",
                "printf ok",
                &db_path,
                "node-crew",
                "pass",
                None,
            ),
        ),
    ]);
    db.crew_create(
        "impl",
        None,
        &[test_crew_member("crew-a"), test_crew_member("crew-b")],
    )
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-crew".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "crew-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "crew": "impl",
            "prompt_template": "ignored by the test script",
            "timeout_minutes": 5,
            "infra_retry_limit": 1,
            "infra_crash_max_seconds": 60,
            "infra_backoff_seconds": 0,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    insert_crew_done_node(&db, &spec_id);

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let platforms: Vec<String> = runs
        .iter()
        .filter(|r| r.node_id == "node-crew")
        .filter_map(|r| r.executed_platform.clone())
        .collect();
    assert!(
        platforms.iter().any(|p| p == "crew-a"),
        "the first attempt ran the first crew member: {platforms:?}"
    );
    assert!(
        platforms.iter().any(|p| p == "crew-b"),
        "the retry rotated to the next crew member: {platforms:?}"
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed,
        "the rotated retry recovered the node"
    );
}

/// CM49 req 5(a): the engine's lazy sync at ensemble dispatch picks up a
/// crew edit made while the graph was running (the eager path did not
/// apply it) — the member rows are rewritten before the members run.
#[tokio::test]
async fn crew_lazy_sync_at_dispatch_picks_up_a_mid_run_edit() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let db_path = dir.path().join(TEST_DB_FILENAME);
    let fake_home = setup_multi_cli_home(&[
        (
            "seed-a",
            &write_member_script(dir.path(), "seed-a.sh", "printf ok"),
        ),
        (
            "seed-b",
            &write_member_script(dir.path(), "seed-b.sh", "printf ok"),
        ),
        (
            "new-a",
            &write_self_reporting_member_script(
                dir.path(),
                "new-a.sh",
                "printf ok",
                &db_path,
                "m1",
                "pass",
                None,
            ),
        ),
        (
            "new-b",
            &write_self_reporting_member_script(
                dir.path(),
                "new-b.sh",
                "printf ok",
                &db_path,
                "m2",
                "pass",
                None,
            ),
        ),
    ]);
    // The ensemble starts bound to a crew holding the "seed" pair.
    insert_kind_ensemble(
        &db,
        &spec_id,
        crate::domain::graphs::EnsembleKind::Parallel,
        &[("m1", "seed-a"), ("m2", "seed-b")],
    );
    db.crew_create(
        "impl",
        None,
        &[test_crew_member("seed-a"), test_crew_member("seed-b")],
    )
    .unwrap();
    db.bind_ensemble_to_crew("ens1", "impl").unwrap();

    // A mid-run crew edit: the DB layer is used directly, so the eager
    // sync (which lives in the `crew_update` MCP tool) does NOT run —
    // exactly the state a running graph is left in.
    db.crew_update(
        "impl",
        Some(&[test_crew_member("new-a"), test_crew_member("new-b")]),
        None,
        None,
    )
    .unwrap();
    let stale = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(
        stale.ensemble.crew_version_applied,
        Some(1),
        "stale before the dispatch (the edit was not eagerly applied)"
    );

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    let after = db.get_ensemble_details("ens1").unwrap().unwrap();
    let platforms: Vec<String> = after
        .members
        .iter()
        .map(|member| member.platform.clone())
        .collect();
    assert_eq!(
        platforms,
        vec!["new-a".to_string(), "new-b".to_string()],
        "the lazy dispatch sync rewrote the member rows to the crew edit"
    );
    assert_eq!(after.ensemble.crew_version_applied, Some(2));
}

/// An agent node that times out must resolve as a FAIL that traverses
/// its fail edge — not abort the whole spec. The fail edge routes to a
/// recovery node whose marker file only appears if the graph actually
/// kept running past the timeout.
#[tokio::test]
async fn agent_node_timeout_with_fail_edge_traverses_it() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("recovered.marker");

    let fake_home = setup_multi_cli_home(&[(
        "hang-cli",
        &write_member_script(dir.path(), "hang.sh", "sleep 5"),
    )]);

    db.insert_graph_node(&GraphNode {
        id: "node-agent-timeout".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "slow-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "hang-cli",
            "prompt_template": "ignored by the test script",
            "timeout_minutes": 0,
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
        from_node: "node-agent-timeout".to_string(),
        to_node: "node-recovery".to_string(),
        condition: GraphEdgeCondition::Fail,
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    assert_eq!(spec.status, GraphSpecStatus::Completed);
    assert!(
        marker.exists(),
        "fail edge must have been traversed after the agent node timed out"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let timeout_run = runs
        .iter()
        .find(|r| r.node_id == "node-agent-timeout")
        .unwrap();
    assert_eq!(timeout_run.status, GraphRunStatus::Fail);
    assert_eq!(timeout_run.output.as_ref().unwrap()["error"], "timed out");
}

/// An agent node that times out with no fail edge must fail the spec
/// (and the graph) cleanly — same as any other dead-end fail — rather
/// than propagating a hard error out of `run_graph`.
#[tokio::test]
async fn agent_node_timeout_without_fail_edge_fails_spec_cleanly() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();

    let fake_home = setup_multi_cli_home(&[(
        "hang-cli",
        &write_member_script(dir.path(), "hang.sh", "sleep 5"),
    )]);

    db.insert_graph_node(&GraphNode {
        id: "node-agent-timeout".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "slow-agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "hang-cli",
            "prompt_template": "ignored by the test script",
            "timeout_minutes": 0,
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

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    assert_eq!(spec.status, GraphSpecStatus::Failed);

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let timeout_run = runs
        .iter()
        .find(|r| r.node_id == "node-agent-timeout")
        .unwrap();
    assert_eq!(timeout_run.status, GraphRunStatus::Fail);
    assert_eq!(timeout_run.output.as_ref().unwrap()["error"], "timed out");
}

/// A check node that times out must behave exactly like any other check
/// fail: it traverses its fail edge instead of aborting the spec.
#[tokio::test]
async fn check_node_timeout_behaves_as_check_fail() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("recovered.marker");

    db.insert_graph_node(&GraphNode {
        id: "node-check-timeout".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "slow-check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "sleep 5",
            "success_condition": "exit_code_0",
            "timeout_seconds": 0,
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
        from_node: "node-check-timeout".to_string(),
        to_node: "node-recovery".to_string(),
        condition: GraphEdgeCondition::Fail,
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
    assert!(
        marker.exists(),
        "fail edge must have been traversed after the check node timed out"
    );

    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    let timeout_run = runs
        .iter()
        .find(|r| r.node_id == "node-check-timeout")
        .unwrap();
    assert_eq!(timeout_run.status, GraphRunStatus::Fail);
    assert_eq!(timeout_run.output.as_ref().unwrap()["error"], "timed out");
}

/// An ensemble member whose own agent timeout fires (not the ensemble's
/// straggler watchdog) must count as a member fail with the "timed out"
/// marker intact — and must never prevent the join from resolving.
/// `min_pass: 1` alongside one passing member proves the join still
/// reaches quorum despite the timed-out member.
#[tokio::test]
async fn ensemble_member_agent_timeout_counts_as_member_fail_without_killing_join() {
    let (dir, db, engine, _graph_id, spec_id) = graph_fixture().unwrap();

    let fake_home = setup_multi_cli_home(&[
        (
            "hang-member",
            &write_member_script(dir.path(), "hang.sh", "sleep 5"),
        ),
        (
            "member-ok",
            &write_member_script(dir.path(), "ok.sh", "printf ok"),
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
        &[("m-hang", "hang-member"), ("m-ok", "member-ok")],
        1,       // min_pass: only one member needs to pass
        Some(5), // generous straggler window — the member's own timeout must fire first
        None,
        "on-pass",
        None,
    );

    // Force the hanging member's own agent timeout to fire immediately,
    // well before the ensemble's straggler watchdog would.
    db.update_graph_node_details(
        "m-hang",
        None,
        None,
        Some(&serde_json::json!({
            "platform": "hang-member",
            "prompt_template": "ignored by the test script",
            "timeout_minutes": 0,
        })),
        None,
    )
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    engine
        .run_graph("wf-test".to_string(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);

    assert!(
            !pass_marker.exists(),
            "CM13: the surviving member never self-reports, so it is unreported infra and the join fails (0/2)"
        );

    let join = join_run(&db, &spec_id, "join1");
    assert_eq!(join.status, GraphRunStatus::Fail);
    assert_eq!(join.output.as_ref().unwrap()["passed"], 0);

    let hang_run = db
        .list_graph_runs_for_spec(&spec_id)
        .unwrap()
        .into_iter()
        .find(|r| r.node_id == "m-hang")
        .unwrap();
    assert_eq!(hang_run.status, GraphRunStatus::Fail);
    assert_eq!(
        hang_run.output.as_ref().unwrap()["error"],
        "timed out",
        "member's own agent timeout must be recorded as such, not a straggler kill"
    );
}

/// DIAGNOSTIC (temporary): while a spec's agent node is actually
/// in-flight (mid `run_graph_dispatch`), does a concurrent DB write
/// targeting the SAME graph id block until the dispatch finishes?
#[tokio::test]
async fn diag_concurrent_db_write_during_dispatch() {
    let fake_home = setup_sleeping_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let engine = Arc::new(engine);

    db.insert_graph_node(&GraphNode {
        id: "node-sleep".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "sleep".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "sleep-cli",
            "timeout_minutes": 1,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let engine2 = Arc::clone(&engine);
    let graph_id2 = graph_id.clone();
    let dispatch = tokio::spawn(async move {
        engine2
            .run_graph(graph_id2, None, None, None, None, None)
            .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    let lp_mid = db.get_graph(&graph_id).unwrap().unwrap();
    eprintln!("DIAG: mid-dispatch graph status = {:?}", lp_mid.status);

    let db2 = Arc::clone(&db);
    let graph_id3 = graph_id.clone();
    let start = std::time::Instant::now();
    let write = tokio::time::timeout(std::time::Duration::from_secs(3), async move {
        db2.get_graph(&graph_id3).unwrap();
        db2.schedule_graph_autorun(&graph_id3, chrono::Utc::now() + chrono::Duration::hours(1))
            .unwrap();
    })
    .await;
    eprintln!(
        "DIAG: write result = {:?}, elapsed = {:?}",
        write,
        start.elapsed()
    );

    let dispatch_start = std::time::Instant::now();
    dispatch.await.unwrap().unwrap();
    eprintln!(
        "DIAG: dispatch total elapsed = {:?}",
        dispatch_start.elapsed()
    );
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    eprintln!("DIAG: final graph status = {:?}", lp.status);
    drop(_home);

    assert!(
        write.is_ok(),
        "node-initiated DB write blocked during dispatch"
    );
}

/// DIAGNOSTIC (temporary) control: same as above, minus the concurrent
/// write — isolates whether the slow dispatch is caused by the write or
/// is inherent to dispatching a plain sleeping node.
#[tokio::test]
async fn diag_dispatch_alone_no_concurrent_write() {
    let fake_home = setup_sleeping_cli_home();
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let engine = Arc::new(engine);

    db.insert_graph_node(&GraphNode {
        id: "node-sleep".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "sleep".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "sleep-cli",
            "timeout_minutes": 1,
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let _home = HomeGuard::set(fake_home.path());
    let start = std::time::Instant::now();
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    eprintln!("DIAG: dispatch-alone total elapsed = {:?}", start.elapsed());
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    eprintln!("DIAG: final graph status = {:?}", lp.status);
    let runs = db.list_graph_runs_for_spec(&spec_id).unwrap();
    for r in &runs {
        eprintln!("DIAG: run status={:?} output={:?}", r.status, r.output);
    }
    drop(_home);
}

/// Hook fires once when the graph completes. The mock process writes a
/// marker file so we can verify it actually ran.
#[tokio::test]
async fn graph_engine_on_completed_hook_fires_on_completion() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("hook_fired.marker");

    let node = GraphNode {
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
    db.insert_graph_node(&node).unwrap();

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

    // Cli::strategy() reads from $CANOPY_HOME_OVERRIDE/.canopy/config.toml.
    let _home = HomeGuard::set(fake_home.path());
    let result = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await;
    drop(_home);
    drop(fake_home);
    result.unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);
    assert!(marker.exists(), "on_completed hook must have run");

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 1);
    assert_eq!(hook_runs[0].status, GraphRunStatus::Pass);
}

/// Hook must NOT fire when the graph fails (a spec's check node returns
/// non-zero). Only `Completed` triggers it.
#[tokio::test]
async fn graph_engine_on_completed_hook_does_not_fire_on_failure() {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("hook_should_not_exist.marker");

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

    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Failed);
    assert!(
        !marker.exists(),
        "on_completed hook must NOT fire on a failed graph"
    );

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert!(hook_runs.is_empty(), "no hook runs should be recorded");
}

/// After a completed→reset→recomplete cycle, the hook fires again (once
/// per completion).
#[tokio::test]
async fn graph_engine_on_completed_hook_fires_again_after_reset_and_recomplete() {
    let fake_home = setup_test_cli_home();
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let marker = dir.path().join("hook_count.log");

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

    let marker_path = marker.to_string_lossy().to_string();
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: Some("test-cli".to_string()),
        model: None,
        effort: None,
        prompt: Some(format!("echo fire >> \"{}\"", marker_path)),
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
    // First completion.
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    // Reset and recomplete. `reset_graph`'s default (`specs: None`) leaves
    // an already-`Completed` spec untouched (see its doc) — pass the
    // spec id explicitly so it actually re-runs (B17: a dispatch that
    // executes zero specs must not fire the hook a second time for
    // doing nothing).
    db.reset_graph(&graph_id, Some(std::slice::from_ref(&spec_id)))
        .unwrap();
    engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap();
    drop(_home);
    drop(fake_home);

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Completed);

    let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
    assert_eq!(hook_runs.len(), 2, "hook must fire once per completion");
}

// ── B17: empty effective spec set is a launch error, not a completion ──

/// A graph with *neither* bound specs *nor* an explicit idea has genuinely
/// nothing to run and must refuse to launch (CB22: a top-level graph
/// alone is not a spec). Status must stay untouched and no run recorded.
#[tokio::test]
async fn graph_engine_zero_bound_specs_and_no_queue_is_a_launch_error() {
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
        "an empty launch must leave the graph's status untouched"
    );
    assert!(
        db.list_graph_runs_for_graph(&graph_id).unwrap().is_empty(),
        "an empty launch must record no run"
    );
}

/// (CB22) A top-level graph alone is not a spec: `empty_launch_check`
/// must refuse a spec-less, idea-less launch even when the graph has
/// top-level graph nodes. Only an explicit non-empty `idea` may supply
/// `spec_content` for a graph with zero bound specs.
#[tokio::test]
async fn empty_launch_check_refuses_spec_less_run_even_when_a_top_level_graph_exists() {
    let (_dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-only".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "only".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf APPROVED",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let message = engine
        .empty_launch_check(&graph_id, None, None)
        .unwrap()
        .expect("a spec-less, idea-less launch must be refused even with a graph");
    assert!(
        message.contains("no specs to run"),
        "unexpected refusal message: {message}"
    );
}

/// (CB22) A graph with a top-level graph but zero bound specs and no
/// explicit `idea` is refused: the engine must not manufacture a blank
/// bound spec and spend agents on an empty `{{spec_content}}`. No agent
/// node ever executes, no `graph_runs` row is recorded, and no placeholder
/// row is left behind.
#[tokio::test]
async fn spec_less_run_with_graph_and_no_idea_is_refused() {
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

    assert!(
        db.list_graph_specs(&graph_id).unwrap().is_empty(),
        "sanity: this graph has no bound specs"
    );

    let guard = HomeGuard::set(home.path());
    let error = engine
        .run_graph(graph_id.clone(), None, None, None, None, None)
        .await
        .unwrap_err();
    drop(guard);
    assert!(
        error.to_string().contains("no specs to run"),
        "a graph-only launch with no idea must be refused: {error}"
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
        db.list_graph_specs(&graph_id).unwrap().is_empty(),
        "a refused launch must not manufacture a blank bound spec"
    );
    assert!(
        std::fs::read_to_string(&argv_file)
            .unwrap_or_default()
            .is_empty(),
        "no agent node may execute on a refused launch"
    );
}
