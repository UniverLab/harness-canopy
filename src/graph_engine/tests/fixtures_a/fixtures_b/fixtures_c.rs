//! Graph-engine test fixtures (c) (split from `src/graph_engine/mod.rs`, pure move).
use super::*;

/// Like [`insert_kind_ensemble`] but each member may carry its own
/// `timeout_minutes` override (CM23) — `None` uses the ensemble's shared
/// `timeout_minutes` (5, same fixture value `insert_kind_ensemble` uses),
/// exactly like an unset member override does in production.
fn insert_kind_ensemble_with_member_timeouts(
    db: &Database,
    spec_id: &str,
    kind: crate::domain::graphs::EnsembleKind,
    members: &[(&str, &str, Option<i64>)],
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
        .map(|(i, (node_id, platform, member_timeout))| GraphNode {
            id: node_id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": platform,
                "prompt_template": "ignored by the member's test script",
                // CB57: the node-level `timeout_minutes` feeds
                // `spawn_and_wait_cli_process`'s own kill-and-record-a-Fail-verdict
                // path, which cascade/round-robin treat as fail-fast — a
                // different outcome from the member-leash cut below
                // (`run_ensemble_member`'s no-verdict fallthrough). Arming
                // both with 0 races the two enforcers: under load the inner
                // one sometimes wins (captured: `members_tried: 1`, no m-ok
                // row at all, join Fail in 0.7 s — m-ok was never
                // dispatched, so no verdict-filer window was missed). The
                // member record below keeps the real `Some(0)` leash this
                // test is about; the node-level default is clamped to >= 1
                // minute so it can never fire inside the test.
                "timeout_minutes": member_timeout.map(|m| m.max(1)).unwrap_or(5),
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
    for (node_id, _, _) in members {
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
        .map(|(i, (node_id, platform, member_timeout))| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: node_id.to_string(),
            position: i as i64,
            platform: platform.to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: *member_timeout,
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

/// CM48: declare one of an ensemble's member platforms unavailable.
fn mark_member_platform_unavailable(db: &Database, platform: &str) {
    db.roster_upsert(crate::db::roster_entries::NewRosterEntry {
        platform,
        model_pattern: None,
        state: crate::db::roster_entries::RosterState::Unavailable,
        reason: Some("quota"),
        until: None,
    })
    .unwrap();
}

// ── M2: router node execution ────────────────────────────────────────

fn router_node(
    id: &str,
    spec_id: &str,
    platform: &str,
    routes: &[(&str, &str)],
    fallback: &str,
    position: i64,
) -> GraphNode {
    let routes_json: Vec<Value> = routes
            .iter()
            .map(|(label, description)| {
                serde_json::json!({ "label": label, "description": description })
            })
            .collect();
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Router,
        config: serde_json::json!({
            "platform": platform,
            "routes": routes_json,
            "fallback": fallback,
            "timeout_minutes": 1,
        }),
        position,
        created_at: chrono::Utc::now(),
    }
}

fn route_edge(id: &str, spec_id: &str, from_node: &str, to_node: &str, label: &str) -> GraphEdge {
    GraphEdge {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        from_node: from_node.to_string(),
        to_node: to_node.to_string(),
        condition: GraphEdgeCondition::Route(label.to_string()),
    }
}

fn sample_routes() -> Vec<RouterRoute> {
    vec![
        RouterRoute {
            label: "billing".to_string(),
            description: "Billing questions".to_string(),
        },
        RouterRoute {
            label: "technical".to_string(),
            description: "Technical issues".to_string(),
        },
        RouterRoute {
            label: "other".to_string(),
            description: "Everything else".to_string(),
        },
    ]
}

// ── CB31: graph_pause waits for the running node and spends no iteration ──

/// A one-check-node graph whose command is `cmd` (node passes iff it exits
/// 0). Returns an Arc'd engine ready to dispatch in the background.
fn cb31_fixture(cmd: &str) -> (TempDir, Arc<Database>, Arc<GraphEngine>, String, String) {
    let (dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    db.insert_graph_node(&GraphNode {
        id: "cb31-node".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "work".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": cmd, "success_condition": "exit_code_0" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    (dir, db, Arc::new(engine), graph_id, spec_id)
}

async fn cb31_wait_for_running_run(db: &Database, graph_id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while db
        .list_running_graph_runs(graph_id)
        .unwrap_or_default()
        .is_empty()
    {
        assert!(
            std::time::Instant::now() < deadline,
            "node run never started"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

fn setup_prompt_capturing_cli_home() -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let script = fake_home.path().join("capture-cli.sh");
    std::fs::write(
            &script,
            r#"#!/bin/sh
CAPTURE_DIR="${CAPTURE_DIR:-/tmp}"
COUNTER_FILE="${CAPTURE_DIR}/counter"
COUNTER=$(cat "$COUNTER_FILE" 2>/dev/null || echo 0)
COUNTER=$((COUNTER + 1))
echo $COUNTER > "$COUNTER_FILE"
cat > "${CAPTURE_DIR}/prompt_${COUNTER}"
echo "NODE_OUTPUT_DATA_${COUNTER}"
# CM13 test support: linger, historically so a background verdict filer had
# time to land its write.
if [ -n "$LINGER_SECONDS" ]; then
  sleep "$LINGER_SECONDS"
fi
# CB57 test support: look up which node this invocation belongs to by
# its counter (1st/2nd/3rd call), since one script backs all three
# nodes here — then self-report using this invocation's own output.
case "$COUNTER" in
  1) SELF_REPORT_NODE_ID="$SELF_REPORT_NODE_1" ;;
  2) SELF_REPORT_NODE_ID="$SELF_REPORT_NODE_2" ;;
  3) SELF_REPORT_NODE_ID="$SELF_REPORT_NODE_3" ;;
esac
if [ -n "$SELF_REPORT_NODE_ID" ]; then
  CANOPY_TEST_SELF_REPORT_DB="$SELF_REPORT_DB" \
  CANOPY_TEST_SELF_REPORT_NODE_ID="$SELF_REPORT_NODE_ID" \
  CANOPY_TEST_SELF_REPORT_STATUS="${SELF_REPORT_STATUS:-pass}" \
  CANOPY_TEST_SELF_REPORT_STDOUT="NODE_OUTPUT_DATA_${COUNTER}" \
    "$SELF_REPORT_EXE" --exact graph_engine::tests::fixtures_a::fixtures_b::fixtures_c::part10_tests::self_report_verdict_writer --nocapture >/dev/null
fi
exit 0
"#,
        )
        .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "capture-cli".to_string(),
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
            prompt_via_stdin: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

// ── CH4: graph hook tests ─────────────────────────────────────────

/// Helper: create a second graph with a simple graph and spec, returning its id.
fn create_target_graph(db: &Database, id: &str, name: &str) -> Result<String> {
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: name.to_string(),
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
    db.insert_graph(&lp)?;
    // Add a Check node so the graph can actually run specs.
    let node = crate::domain::graphs::GraphNode {
        id: format!("{id}-node-1"),
        spec_id: None,
        graph_id: Some(id.to_string()),
        name: "Check".to_string(),
        kind: crate::domain::graphs::GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node)?;
    // Add a spec so the graph has something to run.
    let spec = crate::domain::graphs::GraphSpec {
        id: format!("{id}-spec"),
        graph_id: Some(id.to_string()),
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
    db.insert_graph_spec(&spec)?;
    Ok(id.to_string())
}

fn create_target_graph_no_specs(db: &Database, id: &str, name: &str) -> Result<String> {
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: name.to_string(),
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
    db.insert_graph(&lp)?;
    let node = crate::domain::graphs::GraphNode {
        id: format!("{id}-node-1"),
        spec_id: None,
        graph_id: Some(id.to_string()),
        name: "Check".to_string(),
        kind: crate::domain::graphs::GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node)?;
    Ok(id.to_string())
}

/// Helper: set up a source graph with an on_completed hook that launches
/// a target graph.
fn setup_graph_hook_source(
    db: &Database,
    source_id: &str,
    target_id: &str,
    queue_id: Option<&str>,
    idea: Option<&str>,
) -> Result<()> {
    let hook = crate::domain::graphs::GraphCompletionHook {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: Some(target_id.to_string()),
        queue_id: queue_id.map(str::to_string),
        workdir_override: None,
        idea: idea.map(str::to_string),

        crew: None,
    };
    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(GraphHookEvent::OnCompleted, vec![hook]);
    let lp = crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: source_id.to_string(),
        name: format!("Source {source_id}"),
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
    db.insert_graph(&lp)?;
    // Add a graph node so the graph can actually run specs.
    let node = crate::domain::graphs::GraphNode {
        id: format!("{source_id}-node-1"),
        spec_id: None,
        graph_id: Some(source_id.to_string()),
        name: "Check".to_string(),
        kind: crate::domain::graphs::GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node)?;
    // Add a spec so the graph has something to run.
    let spec = crate::domain::graphs::GraphSpec {
        id: format!("{source_id}-spec"),
        graph_id: Some(source_id.to_string()),
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
    db.insert_graph_spec(&spec)?;
    Ok(())
}

fn missing_member_details() -> EnsembleDetails {
    EnsembleDetails {
        ensemble: crate::domain::graphs::Ensemble {
            mutex: Vec::new(),
            id: "ens-missing".to_string(),
            spec_id: None,
            graph_id: None,
            name: "missing".to_string(),
            prompt_template: String::new(),
            join_node_id: "join1".to_string(),
            entry_from_node: "start".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 5,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "done".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            commit_rights: false,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: vec![EnsembleMember {
            ensemble_id: "ens-missing".to_string(),
            node_id: "ghost-node".to_string(),
            position: 0,
            platform: "fake".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }],
    }
}

/// Fixture for the `session_end` recording tests: a `Running` node run on
/// platform `fake-end`, plus the graph workdir it is scoped to. Returns the
/// owners so the `TempDir`/`GraphEngine` outlive the detached task.
#[cfg(unix)]
fn session_end_run(
    run_id: &str,
    session_id: Option<&str>,
) -> (TempDir, Arc<Database>, GraphEngine, GraphNodeRun, String) {
    let (dir, db, engine, graph_id) = bare_graph_fixture().unwrap();
    let spec = standalone_spec(&format!("sid-{run_id}"), 1);
    db.insert_graph_spec(&spec).unwrap();
    let mut node = sample_agent_node();
    node.spec_id = Some(spec.id.clone());
    db.insert_graph_node(&node).unwrap();
    let run = GraphNodeRun {
        id: run_id.to_string(),
        graph_id,
        spec_id: spec.id,
        node_id: node.id,
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: session_id.map(str::to_string),
        executed_platform: Some("fake-end".to_string()),
        executed_model: None,
    };
    db.insert_graph_run(&run).unwrap();
    let workdir = workdir_for_run(&db, &run);
    (dir, db, engine, run, workdir)
}

/// Poll the run row until `terminate_run_row`'s detached task has written
/// its `session_end` outcome, and return that value. Panics if it never
/// appears, so a test can then assert the exact string.
#[cfg(unix)]
async fn wait_for_session_end(db: &Database, run_id: &str) -> serde_json::Value {
    for _ in 0..500 {
        if let Ok(Some(run)) = db.get_graph_run(run_id) {
            if let Some(value) = run.output.as_ref().and_then(|o| o.get("session_end")) {
                return value.clone();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("session_end was never recorded on run {run_id}");
}

/// Writes a fake harness that answers `session list` with `list_json` and,
/// for an `end` invocation, appends every argv word plus a `done=1` marker
/// to `record`. Returns the binary path.
#[cfg(unix)]
fn write_fake_end_session_cli(
    scratch: &std::path::Path,
    list_json: &str,
    record: &std::path::Path,
) -> std::path::PathBuf {
    let record_str = record.to_string_lossy().to_string();
    let fake_bin = scratch.join("fake-end-cli");
    let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  session) printf '%s\\n' '{list_json}' ;;\n  end) for arg in \"$@\"; do echo \"arg=$arg\" >> '{record_str}'; done; echo \"done=1\" >> '{record_str}' ;;\nesac\nexit 0\n"
        );
    std::fs::write(&fake_bin, script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    fake_bin
}

// ── CB88: decision-mode router tests ─────────────────────────────

/// A router node config with `decision: true` and `min_confidence`.
fn router_decision_node(
    id: &str,
    spec_id: &str,
    platform: &str,
    routes: &[(&str, &str)],
    fallback: &str,
    position: i64,
    min_confidence: f64,
) -> GraphNode {
    let routes_json: Vec<Value> = routes
            .iter()
            .map(|(label, description)| {
                serde_json::json!({ "label": label, "description": description })
            })
            .collect();
    GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Router,
        config: serde_json::json!({
            "platform": platform,
            "routes": routes_json,
            "fallback": fallback,
            "decision": true,
            "min_confidence": min_confidence,
            "timeout_minutes": 1,
        }),
        position,
        created_at: chrono::Utc::now(),
    }
}

/// Like `setup_multi_cli_home` but each entry also carries `decision_args`
/// (CB88). A separate helper so existing callers keep working.
fn setup_decision_cli_home(clis: &[(&str, &str, &str)]) -> tempfile::TempDir {
    let fake_home = tempfile::tempdir().unwrap();
    let canopy_dir = fake_home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        configured_at: Some(chrono::Utc::now().to_rfc3339()),
        clis: clis
            .iter()
            .map(
                |(name, binary, decision_args)| crate::domain::cli_config::CliConfig {
                    name: name.to_string(),
                    binary: binary.to_string(),
                    headless_mode: String::new(),
                    model_flag: Some("-m".to_string()),
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
                    decision_args: Some(decision_args.to_string()),
                    ..Default::default()
                },
            )
            .collect(),
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    fake_home
}

/// A minimal `GraphSpec` for decision router tests.
fn decision_spec(spec_id: &str) -> GraphSpec {
    GraphSpec {
        id: spec_id.to_string(),
        graph_id: None,
        name: format!("Spec {spec_id}"),
        description: Some(format!("Description for {spec_id}")),
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
    }
}

/// A minimal `Graph` for decision router tests.
fn decision_graph() -> crate::domain::graphs::Graph {
    use std::collections::BTreeMap;
    crate::domain::graphs::Graph {
        id: "graph-decision-test".to_string(),
        name: "decision-test-graph".to_string(),
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
        hooks: BTreeMap::new(),
        archived: false,
        paused_by_reconciliation: false,
        infra_node_id: None,
        allow_dirty_start: false,
        require_spec_type: false,
    }
}

mod part01_tests;
mod part02_tests;
mod part03_tests;
mod part04_tests;
mod part05_tests;
mod part06_tests;
mod part07_tests;
mod part08_tests;
mod part09_tests;
mod part10_tests;
mod part11_tests;
mod part12_tests;
mod part13_tests;
mod part14_tests;
mod part15_tests;
mod part16_tests;
mod part17_tests;
mod part18_tests;
mod part19_tests;
mod part20_tests;
mod part21_tests;
