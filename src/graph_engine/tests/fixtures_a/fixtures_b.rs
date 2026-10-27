//! Graph-engine test fixtures (b) (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

// ── CH3: interactive hook tests ────────────────────────────────────

fn interactive_hook_fixture(
    target: &str,
    prompt: &str,
) -> crate::domain::graphs::GraphCompletionHook {
    crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: Some(prompt.to_string()),
        command: None,
        target_session_id: Some(target.to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    }
}

fn interactive_hook_name_fixture(
    target_name: &str,
    prompt: &str,
) -> crate::domain::graphs::GraphCompletionHook {
    crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: Some(prompt.to_string()),
        command: None,
        target_session_id: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
        target_session_name: Some(target_name.to_string()),

        crew: None,
    }
}

// ── F1: ensemble execution (execute_ensemble) ───────────────────────

/// Writes an executable POSIX shell script at `dir/name` with `body` as
/// its content and returns its absolute path. Used to give each
/// ensemble member deterministic, script-controlled pass/fail/hang
/// behavior — the member's actual prompt content is irrelevant (the
/// script ignores stdin/argv entirely), so this sidesteps having to
/// reverse-engineer `render_agent_prompt`'s wrapped output as a runnable
/// shell script.
fn write_member_script(dir: &std::path::Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path.to_string_lossy().to_string()
}

/// CB57: a shell fragment that re-executes the test binary itself
/// (`self_report_exe`, filtered to `graph_engine::tests::
/// self_report_verdict_writer`) so a fake member script files its own
/// self-report verdict as the last thing it does before exiting — see
/// that test's doc comment for why the process-exit ordering can never
/// race the engine. Values are passed to the child as `CANOPY_TEST_*`
/// command-prefix env assignments (the child is the writer; in-suite
/// the variable is never set, so the no-op early-return holds under
/// any harness). `status` is `"pass"` or `"fail"`. The stdout string
/// is only passed when `Some`: with `None` the writer files no
/// `stdout` key, exactly like the old positional-4 invocation. None of
/// this file's fixed stdout strings ("A", "B", "OK",
/// "NODE_OUTPUT_DATA_1", ...) contain a single quote, so naive
/// single-quoting is safe here; this is not a general-purpose shell
/// escaper. The child's stdout (libtest chatter) goes to /dev/null so
/// the member's own stdout stays byte-identical; panics print to
/// stderr and the 101 exit still fails the member script loudly.
fn self_report_invocation(
    db_path: &std::path::Path,
    node_id: &str,
    status: &str,
    stdout: Option<&str>,
) -> String {
    let exe = self_report_exe();
    let mut vars = format!(
            "CANOPY_TEST_SELF_REPORT_DB='{}' CANOPY_TEST_SELF_REPORT_NODE_ID='{node_id}' CANOPY_TEST_SELF_REPORT_STATUS='{status}'",
            db_path.display()
        );
    if let Some(text) = stdout {
        vars += &format!(" CANOPY_TEST_SELF_REPORT_STDOUT='{text}'");
    }
    format!(
            "{vars} '{exe}' --exact graph_engine::tests::fixtures_a::fixtures_b::fixtures_c::part10_tests::self_report_verdict_writer --nocapture >/dev/null\n"
        )
}

/// Build a graph with a single top-level agent node backed by the argv-echo
/// `resume-cli` (set-at-spawn capture + resume-by-id), queue `member_specs`
/// into `queue-1`, run the queue, and hand back the argv log path plus the db.
/// Each grouped member shares the one top-level node id `node-impl`, which
/// is exactly what a warm-context queue looks like: several small specs
/// draining one graph. The fake CLI script self-reports its own Pass
/// verdict as the last thing it does before exiting (see
/// `self_report_invocation`), so `graph_complete_node` never needs to be
/// called for the run to read as reported instead of unreported infra.
async fn run_grouped_queue(
    member_specs: &[(&str, Option<&str>)],
) -> (Arc<Database>, std::path::PathBuf) {
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
    // CM13: the echo script never calls graph_complete_node, so without a
    // self-report every run would be unreported infra and the queue
    // would halt at the first spec. The self-report lands at script
    // exit (CB57); the linger below only spaces runs and is not
    // load-bearing for the verdict write.
    env.insert("LINGER_SECONDS".to_string(), "1".to_string());
    env.insert("SELF_REPORT_NODE_ID".to_string(), "node-impl".to_string());
    env.insert("SELF_REPORT_EXE".to_string(), self_report_exe());
    env.insert(
        "SELF_REPORT_DB".to_string(),
        db_path.to_string_lossy().into_owned(),
    );
    // set-at-spawn capture on a cold run; resume-by-id for the handoff.
    let cli = argv_cli_config(&script, env, Some("--resume"), Some("--set"), None);
    let home = write_resume_cli_home(cli);

    for (position, (spec_id, _)) in member_specs.iter().enumerate() {
        db.insert_graph_spec(&standalone_spec(spec_id, (position as i64) + 1))
            .unwrap();
    }
    insert_queue_with_grouped_members(&db, "queue-1", member_specs);

    // Graph-level agent node: every queue member with no graph of its own
    // drains this shared node, so grouped siblings share the node id.
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
    // CB57: the script self-reports its Pass verdict so the grouped
    // session mechanics run against completed specs, as they did before
    // unreported runs became infra.
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

    // Keep `dir` alive until after the run (workdir + argv log live in it).
    let argv_file = std::fs::canonicalize(&argv_file).unwrap_or(argv_file);
    std::mem::forget(dir);
    (db, argv_file)
}

/// CB57: like [`write_member_script`], but appends a
/// [`self_report_invocation`] after `body` so the script files its own
/// verdict before exiting — replacing the old background-poller pattern
/// for members whose script never calls back into the engine.
fn write_self_reporting_member_script(
    dir: &std::path::Path,
    name: &str,
    body: &str,
    db_path: &std::path::Path,
    node_id: &str,
    status: &str,
    stdout: Option<&str>,
) -> String {
    let full_body = format!(
        "{body}\n{}",
        self_report_invocation(db_path, node_id, status, stdout)
    );
    write_member_script(dir, name, &full_body)
}

/// A fake `.canopy/config.toml` registering one CLI entry per
/// `(name, binary)` pair — lets each ensemble member run its own script
/// under its own `platform` name, so a single ensemble can exercise
/// pass/fail/hang members side by side in the same run.
fn setup_multi_cli_home(clis: &[(&str, &str)]) -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: clis
            .iter()
            .map(|(name, binary)| crate::domain::cli_config::CliConfig {
                name: name.to_string(),
                binary: binary.to_string(),
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
                prompt_via_stdin: true,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

/// CM30: one fake-home CLI entry with a platform-level infra-retry
/// budget: `(name, binary, infra_retry_limit, infra_crash_max_seconds,
/// infra_backoff_seconds)`.
type InfraCliEntry<'a> = (&'a str, &'a str, Option<u32>, Option<u64>, Option<u64>);

/// CM30: like [`setup_multi_cli_home`] but each entry also carries the
/// platform-level infra-retry budget
/// `(name, binary, infra_retry_limit, infra_crash_max_seconds,
/// infra_backoff_seconds)` — `None` per field means "platform sets
/// nothing, the engine default applies". A separate helper (rather than
/// an extended signature) so the 20+ existing `setup_multi_cli_home`
/// callers keep working positionally.
fn setup_multi_cli_home_with_infra(clis: &[InfraCliEntry<'_>]) -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: clis
            .iter()
            .map(|(name, binary, retry_limit, crash_max, backoff)| {
                crate::domain::cli_config::CliConfig {
                    name: name.to_string(),
                    binary: binary.to_string(),
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
                    prompt_via_stdin: true,
                    infra_retry_limit: *retry_limit,
                    infra_crash_max_seconds: *crash_max,
                    infra_backoff_seconds: *backoff,
                    ..Default::default()
                }
            })
            .collect(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

/// Like [`setup_multi_cli_home`] but for CM26 resume tests: the caller
/// builds each member's full [`crate::domain::cli_config::CliConfig`]
/// (session_resume_cmd / session_id_set_flag / per-member env such as its
/// own `ARGV_FILE`) via [`argv_cli_config`] rather than the bare
/// name/binary pair `setup_multi_cli_home` accepts.
fn setup_multi_resume_cli_home(
    clis: Vec<crate::domain::cli_config::CliConfig>,
) -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis,
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

/// Builds a real ensemble unit (kickoff -> N members -> join -> pass/fail
/// exits) directly against the DB, the same shape `graph_add_ensemble`
/// assembles in one MCP call — but constructed here node-by-node so
/// engine tests can drive it through the real `execute_ensemble` path
/// via `GraphEngine::run_graph` without spinning up the MCP server.
#[expect(clippy::too_many_arguments)]
fn insert_test_ensemble(
    db: &Database,
    spec_id: &str,
    kickoff_id: &str,
    ensemble_id: &str,
    join_id: &str,
    members: &[(&str, &str)], // (node_id, cli_platform_name)
    min_pass: i64,
    straggler_timeout_minutes: Option<i64>,
    quorum_grace_minutes: Option<i64>,
    on_pass_to: &str,
    on_fail_to: Option<&str>,
) {
    let now = chrono::Utc::now();

    db.insert_graph_node(&GraphNode {
        id: kickoff_id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "printf ok",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: now,
    })
    .unwrap();

    let member_nodes: Vec<GraphNode> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| GraphNode {
            id: node_id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": platform,
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
                // A member that exits non-zero inside `crash_max_seconds`
                // reads as an infra crash, so the engine retries it behind
                // the doubling backoff. At the 30s default that parked two
                // of these tests on a real 60s sleep each and set the floor
                // for the whole suite; zero keeps the retry path exercised
                // without the wait.
                "infra_backoff_seconds": 0,
            }),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();

    let join_node = GraphNode {
        id: join_id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": ensemble_id }),
        position: 2 + members.len() as i64,
        created_at: now,
    };

    let mut edges = Vec::new();
    for (node_id, _) in members {
        edges.push(GraphEdge {
            id: format!("{kickoff_id}->{node_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: kickoff_id.to_string(),
            to_node: node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{node_id}->{join_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: node_id.to_string(),
            to_node: join_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: format!("{join_id}->pass"),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: join_id.to_string(),
        to_node: on_pass_to.to_string(),
        condition: GraphEdgeCondition::Pass,
    });
    if let Some(fail_to) = on_fail_to {
        edges.push(GraphEdge {
            id: format!("{join_id}->fail"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: join_id.to_string(),
            to_node: fail_to.to_string(),
            condition: GraphEdgeCondition::Fail,
        });
    }

    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: ensemble_id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Test Ensemble".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: join_id.to_string(),
        entry_from_node: kickoff_id.to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass,
        straggler_timeout_minutes,
        quorum_grace_minutes,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: on_pass_to.to_string(),
        on_fail_to: on_fail_to.map(str::to_string),
        kind: crate::domain::graphs::EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members: Vec<EnsembleMember> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| EnsembleMember {
            ensemble_id: ensemble_id.to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
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
}

/// CM26: like [`insert_test_ensemble`] but the ensemble `kind` and
/// round-robin's starting index are caller-chosen, since the CM26 tests
/// need a bounce edge to route back INTO the ensemble (one `GraphEdge`
/// per member, `to_node` = that member's node id, matching condition —
/// the `select_next_step` fan-out convention documented on
/// [`select_next_step`]) rather than always terminating at a fixed
/// pass/fail node the way [`insert_test_ensemble`] does.
#[expect(clippy::too_many_arguments)]
fn insert_bouncing_ensemble(
    db: &Database,
    spec_id: &str,
    kind: crate::domain::graphs::EnsembleKind,
    members: &[(&str, &str)], // (node_id, cli_platform_name)
    min_pass: i64,
    straggler_timeout_minutes: Option<i64>,
    round_robin_index: Option<i64>,
    on_pass_to: &str,
    on_fail_to: &str,
) {
    let now = chrono::Utc::now();

    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: now,
    })
    .unwrap();

    let member_nodes: Vec<GraphNode> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| GraphNode {
            id: node_id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": platform,
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
                "infra_backoff_seconds": 0,
            }),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 50,
        created_at: now,
    };

    let mut edges = Vec::new();
    for (node_id, _) in members {
        edges.push(GraphEdge {
            id: format!("kickoff->{node_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{node_id}->join1"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: node_id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: "join1->pass".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: on_pass_to.to_string(),
        condition: GraphEdgeCondition::Pass,
    });
    edges.push(GraphEdge {
        id: "join1->fail".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: on_fail_to.to_string(),
        condition: GraphEdgeCondition::Fail,
    });

    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Bouncing Ensemble".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass,
        straggler_timeout_minutes,
        quorum_grace_minutes: None,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: on_pass_to.to_string(),
        on_fail_to: Some(on_fail_to.to_string()),
        kind,
        round_robin_index,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members: Vec<EnsembleMember> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
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
}

/// Edges routing a gate node's `Fail` verdict back INTO the ensemble
/// (`ens1`) — one `GraphEdge` per member, `to_node` = that member's node
/// id, condition `Fail`, matching `select_next_step`'s "distinct targets
/// equal one ensemble's member set" fan-out convention.
fn bounce_edges_into_ensemble(spec_id: &str, gate_id: &str, member_ids: &[&str]) -> Vec<GraphEdge> {
    member_ids
        .iter()
        .map(|member_id| GraphEdge {
            id: format!("{gate_id}->{member_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: gate_id.to_string(),
            to_node: member_id.to_string(),
            condition: GraphEdgeCondition::Fail,
        })
        .collect()
}

/// A `Check` node that fails on its first invocation and passes
/// (`printf APPROVED`) from the second invocation on — used to force
/// exactly one bounce back into an ensemble before a spec is allowed to
/// terminate.
fn counter_gate_node(
    id: &str,
    spec_id: &str,
    counter: &std::path::Path,
    position: i64,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!(
                "n=$(cat \"{c}\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"{c}\"; [ \"$n\" -ge 2 ] && printf APPROVED || exit 1",
                c = counter.display(),
            ),
            "success_condition": "exit_code_0"
        }),
        position,
        created_at: chrono::Utc::now(),
    }
}

fn touch_marker_node(
    id: &str,
    spec_id: &str,
    marker: &std::path::Path,
    position: i64,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": format!("touch \"{}\"", marker.display()),
            "success_condition": "exit_code_0"
        }),
        position,
        created_at: chrono::Utc::now(),
    }
}

fn join_run(db: &Database, spec_id: &str, join_id: &str) -> GraphNodeRun {
    db.list_graph_runs_for_spec(spec_id)
        .unwrap()
        .into_iter()
        .rfind(|run| run.node_id == join_id)
        .expect("join must have produced a run row")
}

// ── B26: ensemble member infra-crash retry ──────────────────────────

/// Like [`insert_test_ensemble`], but merges `member_config` into every
/// member node's config — used by the B26 tests to set
/// `infra_backoff_seconds: 0` so retries don't actually sleep.
fn insert_infra_ensemble(
    db: &Database,
    spec_id: &str,
    members: &[(&str, &str)],
    min_pass: i64,
    straggler_timeout_minutes: Option<i64>,
    quorum_grace_minutes: Option<i64>,
    member_config: &Value,
) {
    let now = chrono::Utc::now();
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: now,
    })
    .unwrap();

    let member_nodes: Vec<GraphNode> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| {
            let mut config = serde_json::json!({
                "platform": platform,
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
            });
            if let Value::Object(extra) = member_config {
                for (k, v) in extra {
                    config[k] = v.clone();
                }
            }
            GraphNode {
                id: node_id.to_string(),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: format!("member-{}", i + 1),
                kind: GraphNodeKind::Agent,
                config,
                position: 2 + i as i64,
                created_at: now,
            }
        })
        .collect();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 2 + members.len() as i64,
        created_at: now,
    };

    let mut edges = Vec::new();
    for (node_id, _) in members {
        edges.push(GraphEdge {
            id: format!("kickoff->{node_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{node_id}->join1"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: node_id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: "join1->pass".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "done".to_string(),
        condition: GraphEdgeCondition::Pass,
    });

    // Terminal marker node so a passing join has somewhere to route.
    db.insert_graph_node(&GraphNode {
        id: "done".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "done".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 200,
        created_at: now,
    })
    .unwrap();

    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Test Ensemble".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass,
        straggler_timeout_minutes,
        quorum_grace_minutes,
        timeout_minutes: 5,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "done".to_string(),
        on_fail_to: None,
        kind: crate::domain::graphs::EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members: Vec<EnsembleMember> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
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
}

fn member_runs(db: &Database, spec_id: &str, node_id: &str) -> Vec<GraphNodeRun> {
    let mut runs: Vec<GraphNodeRun> = db
        .list_graph_runs_for_spec(spec_id)
        .unwrap()
        .into_iter()
        .filter(|r| r.node_id == node_id)
        .collect();
    runs.sort_by_key(|r| r.started_at);
    runs
}

/// CM3: like [`insert_infra_ensemble`] but for the non-parallel kinds.
/// Sets `ensemble.kind` (and `round_robin_index: Some(0)` for round-robin)
/// and wires the join to BOTH a pass and a fail terminal so a test can
/// observe which verdict the ensemble routed on. `infra_backoff_seconds:
/// 0` keeps the retry path fast.
fn insert_kind_ensemble(
    db: &Database,
    spec_id: &str,
    kind: crate::domain::graphs::EnsembleKind,
    members: &[(&str, &str)],
) {
    let now = chrono::Utc::now();
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: now,
    })
    .unwrap();

    let member_nodes: Vec<GraphNode> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| GraphNode {
            id: node_id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": platform,
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": 5,
                "infra_backoff_seconds": 0,
            }),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 50,
        created_at: now,
    };

    let mut edges = Vec::new();
    for (node_id, _) in members {
        edges.push(GraphEdge {
            id: format!("kickoff->{node_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{node_id}->join1"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: node_id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    for (i, (term, cond)) in [
        ("done-pass", GraphEdgeCondition::Pass),
        ("done-fail", GraphEdgeCondition::Fail),
    ]
    .into_iter()
    .enumerate()
    {
        db.insert_graph_node(&GraphNode {
                id: term.to_string(),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: term.to_string(),
                kind: GraphNodeKind::Check,
                config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
                position: 100 + i as i64,
                created_at: now,
            })
            .unwrap();
        edges.push(GraphEdge {
            id: format!("join1->{term}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: term.to_string(),
            condition: cond,
        });
    }

    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Switched Ensemble".to_string(),
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
        on_pass_to: "done-pass".to_string(),
        on_fail_to: Some("done-fail".to_string()),
        kind,
        round_robin_index: if kind == crate::domain::graphs::EnsembleKind::RoundRobin {
            Some(0)
        } else {
            None
        },
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members: Vec<EnsembleMember> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
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
}

/// Like [`insert_kind_ensemble`], but the ensemble's `timeout_minutes`
/// and `infra_retry_limit` are caller-supplied instead of fixed at 5/None
/// — CB67 needs a short, non-zero `timeout_minutes` (so a member can
/// sleep past HALF of it while staying under the whole of it) and
/// `infra_retry_limit: 0` baked into each member NODE's config (so a
/// silent/no-verdict member falls through after exactly one attempt
/// instead of retrying in place — dispatch resolves the retry budget
/// from the member node's config, so the value has to land there, not
/// only on the ensemble row, which dispatch never reads for this).
///
/// CB67 deviation from the original plan draft: `straggler_timeout_minutes`
/// is `Some(0)` (the pre-CB67 legacy shape — `graph_add_ensemble` used to
/// accept it on any kind), not `None`. FR1 requires sequential dispatch
/// to never consult that field: with `None` the pre-fix fallback
/// (`effective_straggler_timeout_minutes()`) equals `timeout_minutes`,
/// so a test built on it passes with or without the fix. With `Some(0)`
/// the pre-fix code bounds every member to a zero-second window and
/// cuts them all instantly — the test then fails before the fix and
/// passes after, exactly as the spec's failing-test-first guideline
/// requires.
fn insert_kind_ensemble_with_timeout(
    db: &Database,
    spec_id: &str,
    kind: crate::domain::graphs::EnsembleKind,
    timeout_minutes: i64,
    infra_retry_limit: Option<i64>,
    members: &[(&str, &str)],
) {
    let now = chrono::Utc::now();
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
        position: 1,
        created_at: now,
    })
    .unwrap();

    let member_nodes: Vec<GraphNode> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| {
            let mut config = serde_json::json!({
                "platform": platform,
                "prompt_template": "ignored by the member's test script",
                "timeout_minutes": timeout_minutes,
                "infra_backoff_seconds": 0,
            });
            if let Some(limit) = infra_retry_limit {
                config["infra_retry_limit"] = serde_json::json!(limit);
            }
            GraphNode {
                id: node_id.to_string(),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: format!("member-{}", i + 1),
                kind: GraphNodeKind::Agent,
                config,
                position: 2 + i as i64,
                created_at: now,
            }
        })
        .collect();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": "ens1" }),
        position: 50,
        created_at: now,
    };

    let mut edges = Vec::new();
    for (node_id, _) in members {
        edges.push(GraphEdge {
            id: format!("kickoff->{node_id}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: node_id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{node_id}->join1"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: node_id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    for (i, (term, cond)) in [
        ("done-pass", GraphEdgeCondition::Pass),
        ("done-fail", GraphEdgeCondition::Fail),
    ]
    .into_iter()
    .enumerate()
    {
        db.insert_graph_node(&GraphNode {
                id: term.to_string(),
                spec_id: Some(spec_id.to_string()),
                graph_id: None,
                name: term.to_string(),
                kind: GraphNodeKind::Check,
                config: serde_json::json!({ "command": "printf ok", "success_condition": "exit_code_0" }),
                position: 100 + i as i64,
                created_at: now,
            })
            .unwrap();
        edges.push(GraphEdge {
            id: format!("join1->{term}"),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: term.to_string(),
            condition: cond,
        });
    }

    let ensemble = crate::domain::graphs::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: "Switched Ensemble".to_string(),
        prompt_template: "ignored by the member's test script".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: Some(0),
        quorum_grace_minutes: None,
        timeout_minutes,
        infra_retry_limit,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "done-pass".to_string(),
        on_fail_to: Some("done-fail".to_string()),
        kind,
        round_robin_index: if kind == crate::domain::graphs::EnsembleKind::RoundRobin {
            Some(0)
        } else {
            None
        },
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let ensemble_members: Vec<EnsembleMember> = members
        .iter()
        .enumerate()
        .map(|(i, (node_id, platform))| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
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
}

mod fixtures_c;
