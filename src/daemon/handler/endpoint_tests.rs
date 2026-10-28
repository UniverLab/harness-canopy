//! `endpoint_tests` tests (split from `src/daemon/handler.rs`, pure move).
use super::*;
use crate::application::notification_service::{DefaultNotificationService, NotificationService};
use crate::daemon::params_extract::Parameters;
use crate::db::Database;
use crate::domain::models::{Agent, Cli, RunLog, RunStatus, TriggerType};
use crate::executor::Executor;
use crate::graph_engine::GraphEngine;
use crate::rag::ingestion::IngestionManager;
use crate::watchers::WatcherEngine;
use tempfile::tempdir;
use tokio::sync::Notify;

/// RAII guard: sets the real `$HOME` env var to `path` for the test
/// body, restoring the previous value on drop. `make_log_path` /
/// `data_dir()` read `dirs::home_dir()` directly (real `$HOME`, not the
/// `CANOPY_HOME_OVERRIDE` some other subsystems honor), so any handler
/// path that writes a log file needs this to avoid touching the real
/// developer's `~/.canopy`. Safe under `cargo nextest` (one process per
/// test) but would race under plain `cargo test`.
struct HomeVar {
    prev: Option<std::ffi::OsString>,
}

impl HomeVar {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", path) };
        HomeVar { prev }
    }
}

impl Drop for HomeVar {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

fn endpoint_test_handler() -> (
    tempfile::TempDir,
    std::sync::Arc<Database>,
    TaskTriggerHandler,
) {
    let dir = tempdir().unwrap();
    let db = Arc::new(Database::new(&dir.path().join("test.db")).unwrap());
    let notif: Arc<dyn NotificationService> = Arc::new(DefaultNotificationService);
    let executor = Arc::new(Executor::new(Arc::clone(&db), Arc::clone(&notif)));
    let graph_engine = Arc::new(GraphEngine::new(Arc::clone(&db), Arc::clone(&notif)));
    let watcher_engine = Arc::new(WatcherEngine::new(
        Arc::clone(&db),
        Arc::clone(&executor),
        Arc::clone(&graph_engine),
    ));
    let ingestion = Arc::new(IngestionManager::new(
        Arc::clone(&db),
        dir.path().to_path_buf(),
    ));
    let dynamic_skills = Arc::new(crate::dynamic_skills::SkillStore::new(
        dir.path().join("skills"),
        Vec::new(),
        15,
    ));
    let handler = TaskTriggerHandler::with_deps(HandlerDeps {
        db: Arc::clone(&db),
        executor,
        watcher_engine,
        scheduler_notify: Arc::new(Notify::new()),
        graph_engine,
        notification_service: notif,
        ingestion,
        dynamic_skills,
        start_time: std::time::Instant::now(),
        port: 0,
    });
    (dir, db, handler)
}

fn text(result: &CallToolResult) -> String {
    format!("{:?}", result.content)
}

fn is_err(result: &CallToolResult) -> bool {
    result.is_error == Some(true)
}

/// The raw (unescaped) text of a result's first content block — unlike
/// [`text`] (which Debug-formats the whole `Vec<ContentBlock>`, escaping
/// embedded quotes), this is safe to `serde_json::from_str` or otherwise
/// parse structurally.
fn raw_text(result: &CallToolResult) -> String {
    result.content[0].as_text().unwrap().text.clone()
}

/// Parse a `build_id_result`-shaped response (`{"<key>": "<id>"}`) and
/// return the id.
fn extract_id(result: &CallToolResult, key: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(&raw_text(result))
        .unwrap_or_else(|e| panic!("expected JSON body, got {:?}: {e}", raw_text(result)));
    value[key]
        .as_str()
        .unwrap_or_else(|| panic!("expected string field '{key}' in {value}"))
        .to_string()
}

// ── task_list / agent_list ───────────────────────────────────

fn sample_agent(id: &str, trigger: Option<Trigger>) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "do things".to_string(),
        trigger,
        cli: Cli::new("opencode"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: format!("/tmp/{id}.log"),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

// ── task_report / agent_report ───────────────────────────────

fn sample_run(id: &str, agent_id: &str, status: RunStatus) -> RunLog {
    RunLog {
        id: id.to_string(),
        background_agent_id: agent_id.to_string(),
        status,
        trigger_type: TriggerType::Manual,
        summary: None,
        started_at: chrono::Utc::now(),
        finished_at: None,
        exit_code: None,
        timeout_at: None,
        executed_platform: None,
        executed_model: None,
    }
}

// ── graph_edit / graph_edit / graph tool handlers ─────

fn valid_spec_description() -> String {
    "<spec>\n  <objective>Ship the feature.</objective>\n  <functional_requirements>Does the thing.</functional_requirements>\n  <non_functional_requirements>Is fast.</non_functional_requirements>\n  <constraints>None extra.</constraints>\n  <guidelines>Follow house style.</guidelines>\n  <in_scope>This change.</in_scope>\n  <out_of_scope>Everything else.</out_of_scope>\n</spec>"
            .to_string()
}

fn agent_node_config(platform: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(
        "platform".to_string(),
        serde_json::Value::String(platform.to_string()),
    );
    map
}

fn check_node_config(command: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(
        "command".to_string(),
        serde_json::Value::String(command.to_string()),
    );
    map
}

/// Builds a graph with implementer -> gate -> committer (agent, check,
/// agent), returning the graph id and each node's id in graph order.
async fn build_simple_graph(
    handler: &TaskTriggerHandler,
    workdir: &str,
    graph_name: &str,
) -> (String, String, String, String) {
    let created = call_graph_action(
        handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: graph_name.to_string(),
            description: Some("A shareable design".to_string()),
            workdir: workdir.to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let n1 = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "implementer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let n1_id = extract_id(&n1, "node_id");

    let n2 = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "gate".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("cargo test")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let n2_id = extract_id(&n2, "node_id");

    let n3 = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "committer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let n3_id = extract_id(&n3, "node_id");

    for (from, to, condition) in [(&n1_id, &n2_id, "always"), (&n2_id, &n3_id, "always")] {
        let edge = call_graph_action(
            handler,
            "graph_edge",
            "add",
            params_id(GraphAddEdgeParams {
                spec_id: None,
                graph_id: Some(graph_id.clone()),
                from_node: from.clone(),
                to_node: to.clone(),
                condition: condition.to_string(),
                route: None,
            }),
        )
        .await
        .unwrap();
        assert!(!is_err(&edge), "{}", text(&edge));
    }

    (graph_id, n1_id, n2_id, n3_id)
}

/// Build a graph whose only ensemble has a pass exit but no `on_fail_to`
/// and no manual fail/always edge from its quorum. Returns the graph, the
/// ensemble id, the quorum node id and the arbiter (pass target) id.
async fn graph_with_unexited_quorum(
    dir: &tempfile::TempDir,
    db: &Database,
    handler: &TaskTriggerHandler,
) -> (Graph, String, String, String) {
    let lp = insert_test_graph(db, dir.path());

    let mk_node = |name: &str, position: i64| GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        // Unconfigured platform -> probe returns NotConfigured instantly,
        // no quota spent, preflight still succeeds.
        config: serde_json::json!({"platform": "cm19-unconfigured-platform"}),
        position,
        created_at: chrono::Utc::now(),
    };
    let entry = mk_node("entry", 1);
    let arbiter = mk_node("arbiter", 2);
    db.insert_graph_node(&entry).unwrap();
    db.insert_graph_node(&arbiter).unwrap();

    let added = call_graph_action(
        handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            name: "Implementer".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "cm19-unconfigured-platform".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "cm19-unconfigured-platform".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            node: None,
            from_node: entry.id.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.id.clone(),
            on_fail_to: None,
            commit_rights: None,
            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let ensembles = db.list_ensembles_for_graph(&lp.id).unwrap();
    assert_eq!(ensembles.len(), 1, "expected one ensemble");
    let ensemble = &ensembles[0].ensemble;
    (
        lp,
        ensemble.id.clone(),
        ensemble.join_node_id.clone(),
        arbiter.id.clone(),
    )
}

/// Run `graph_control` and return the `graph_warnings` array. Shared by
/// the quorum tests so each can assert the baseline warning first: a
/// negative test that only checks for silence would still pass if the
/// warning never existed at all.
async fn preflight_warnings(
    handler: &TaskTriggerHandler,
    graph_id: &str,
) -> Vec<serde_json::Value> {
    let result = call_graph_action(
        handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: graph_id.to_string(),
            timeout_seconds: Some(5),
            reviewer: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    body["graph_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("preflight must include graph_warnings: {body}"))
        .clone()
}

fn insert_test_graph(db: &Database, workdir: &std::path::Path) -> Graph {
    let lp = Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: uuid::Uuid::new_v4().to_string(),
        name: "Test Graph".to_string(),
        description: None,
        workdir: workdir.to_string_lossy().to_string(),
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
    };
    db.insert_graph(&lp).unwrap();
    lp
}

fn insert_test_spec(db: &Database, graph_id: &str, position: i64) -> GraphSpec {
    let spec = GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: Some(graph_id.to_string()),
        name: "Test Spec".to_string(),
        description: Some(valid_spec_description()),
        position,
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    db.insert_graph_spec(&spec).unwrap();
    spec
}

fn insert_named_spec(
    db: &Database,
    graph_id: &str,
    name: &str,
    position: i64,
    status: GraphSpecStatus,
) -> GraphSpec {
    let spec = GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: Some(graph_id.to_string()),
        name: name.to_string(),
        description: Some(valid_spec_description()),
        position,
        parallelizable: false,
        status,
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    db.insert_graph_spec(&spec).unwrap();
    spec
}

fn init_git_repo(path: &std::path::Path) {
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(path)
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("git command failed to run");
        assert!(status.success(), "git {:?} failed", args);
    };
    run(&["init", "-q"]);
    run(&["config", "user.name", "Test"]);
    run(&["config", "user.email", "test@example.com"]);
    std::fs::write(path.join("README.md"), "test").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "init"]);
}

// ── graph_edge / graph_edge ──────────────────────────

async fn add_agent_node(handler: &TaskTriggerHandler, spec_id: &str, name: &str) -> String {
    let result = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    extract_id(&result, "node_id")
}

/// CB52 helper: a 2-member ensemble on this spec's graph, entered from
/// `entry`, exiting to `arbiter` (pass) and optionally `fallback`
/// (fail).
async fn add_cb52_ensemble(
    handler: &TaskTriggerHandler,
    spec_id: &str,
    name: &str,
    entry: &str,
    arbiter: &str,
    on_fail_to: Option<&str>,
) -> String {
    let member = || crate::daemon::params::EnsembleMemberParams {
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    let result = call_graph_action(
        handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(vec![member(), member()]),
            node: None,
            from_node: entry.to_string(),
            condition: "always".to_string(),
            min_pass: Some(2),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.to_string(),
            on_fail_to: on_fail_to.map(str::to_string),

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    extract_id(&result, "ensemble_id")
}

// ── router nodes (routes + route edges) ──────────────────────────

fn router_node_config(
    routes: &serde_json::Value,
    fallback: &str,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::json!({ "routes": routes, "fallback": fallback })
        .as_object()
        .unwrap()
        .clone()
}

fn two_routes() -> serde_json::Value {
    serde_json::json!([
        { "label": "retry", "description": "Retry the current step." },
        { "label": "escalate", "description": "Hand off to a human." },
    ])
}

// ── graph / graph ────────────────────

fn insert_named_node(db: &Database, spec_id: &str, name: &str, position: i64) -> GraphNode {
    let node = GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    node
}

#[allow(clippy::too_many_arguments)]
fn insert_finalized_node_run(
    db: &Database,
    graph_id: &str,
    spec_id: &str,
    node_id: &str,
    status: GraphRunStatus,
    output: Option<serde_json::Value>,
    started_at: chrono::DateTime<chrono::Utc>,
) -> GraphNodeRun {
    let run = GraphNodeRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: node_id.to_string(),
        status,
        input: None,
        output,
        started_at,
        completed_at: Some(started_at),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: Some("ses_test_123".to_string()),
        executed_platform: None,
        executed_model: None,
    };
    db.insert_graph_run(&run).unwrap();
    run
}

// ── graph_complete_node / graph_report_blocker ──────────────────

/// Creates a spec + agent node under `graph_id` and a `Running` node-run
/// against it — the FK-satisfying fixture `graph_complete_node` /
/// `graph_report_blocker` need (`graph_runs.spec_id`/`node_id` are both
/// `NOT NULL REFERENCES`).
fn insert_running_node_run(db: &Database, graph_id: &str) -> GraphNodeRun {
    let spec = insert_test_spec(db, graph_id, 1);
    let node = GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Node".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node).unwrap();
    let run = GraphNodeRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: graph_id.to_string(),
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
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    db.insert_graph_run(&run).unwrap();
    run
}

// ── CM14: ensemble entry rewiring, multi-source entry, deletion ──

/// Build a two-member ensemble for the CM14 tests: `name` entered from
/// `from_node`, routing on pass to `on_pass_to`.
async fn add_two_member_ensemble(
    handler: &TaskTriggerHandler,
    spec_id: &str,
    name: &str,
    from_node: &str,
    on_pass_to: &str,
) -> String {
    let created = call_graph_action(
        handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: None,
            prompt_template: Some("Do the thing {{spec_name}}".to_string()),
            members: Some(vec![
                crate::daemon::params::EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                crate::daemon::params::EnsembleMemberParams {
                    platform: "opencode".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            node: None,
            from_node: from_node.to_string(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: on_pass_to.to_string(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    extract_id(&created, "ensemble_id")
}

fn blank_ensemble_update(ensemble_id: &str) -> GraphUpdateEnsembleParams {
    GraphUpdateEnsembleParams {
        mutex: None,
        ensemble_id: ensemble_id.to_string(),
        kind: None,
        prompt_template: None,
        members: None,
        min_pass: None,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: None,
        on_fail_to: None,
        from_node: None,
        condition: None,
        add_entry_from: None,
        add_entry_condition: None,
        remove_entry_from: None,
        commit_rights: None,

        crew: None,
    }
}

/// Builds a graph with one spec, two candidate entry nodes (`entry`,
/// `alt_entry`), an `arbiter` exit node, and a parallel ensemble entered
/// from both with `member_count` members. Returns (graph, spec, ensemble_id,
/// entry, alt_entry).
async fn build_two_source_ensemble_graph(
    db: &Database,
    handler: &TaskTriggerHandler,
    git_workdir: &std::path::Path,
    member_count: usize,
) -> (Graph, GraphSpec, String, String, String) {
    let lp = insert_test_graph(db, git_workdir);
    let spec = insert_test_spec(db, &lp.id, 1);
    let entry = add_agent_node(handler, &spec.id, "Entry").await;
    let alt_entry = add_agent_node(handler, &spec.id, "AltEntry").await;
    let arbiter = add_agent_node(handler, &spec.id, "Arbiter").await;
    let member = || crate::daemon::params::EnsembleMemberParams {
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    let created = call_graph_action(
        handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Proposers".to_string(),
            kind: None,
            prompt_template: Some("Draft it".to_string()),
            members: Some((0..member_count).map(|_| member()).collect()),
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");
    let mut add_alt = blank_ensemble_update(&ensemble_id);
    add_alt.add_entry_from = Some(alt_entry.clone());
    let added = call_graph_action(handler, "graph_ensemble", "update", params_id(add_alt))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));
    (lp, spec, ensemble_id, entry, alt_entry)
}

// ── sync_* / intelligence_* / get_tools / project_* ────────────

/// RAII guard: sets the real `CANOPY_AGENT_ID` env var, which
/// `resolve_sync_agent_id` falls back to when no request `Parts` header
/// is present (as in these direct-call tests). Safe under `cargo
/// nextest` (one process per test).
struct AgentIdVar {
    prev: Option<std::ffi::OsString>,
}

impl AgentIdVar {
    fn set(id: &str) -> Self {
        let prev = std::env::var_os(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV);
        unsafe {
            std::env::set_var(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV, id);
        }
        AgentIdVar { prev }
    }

    fn clear() -> Self {
        let prev = std::env::var_os(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV);
        unsafe {
            std::env::remove_var(crate::shared::sync_identity::CANOPY_AGENT_ID_ENV);
        }
        AgentIdVar { prev }
    }
}

impl Drop for AgentIdVar {
    fn drop(&mut self) {
        let key = crate::shared::sync_identity::CANOPY_AGENT_ID_ENV;
        match &self.prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

async fn upsert_intel_fact(
    handler: &TaskTriggerHandler,
    id: &str,
    title: &str,
    relations: Option<Vec<IntelligenceRelationParams>>,
) {
    let result = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some(id.to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some(title.to_string()),
                    body: Some("Test body".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
}

// ── CM9: typed project graph traversal ──────────────────────

/// Register parent + child dirs (child nested under parent) with one fact
/// each. Returns (base_tempdir, parent_hash, child_hash).
fn cm9_seed_parent_child(db: &Database) -> (tempfile::TempDir, String, String) {
    let base = tempdir().unwrap();
    let parent = base.path().join("ws-parent");
    let child = parent.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let p = db.register_project_path(&parent).unwrap();
    let c = db.register_project_path(&child).unwrap();
    for (id, hash, title) in [
        (
            "cm9-fact-parent",
            p.hash.clone(),
            "workspace billing rulebook",
        ),
        (
            "cm9-fact-child",
            c.hash.clone(),
            "workspace billing rulebook",
        ),
    ] {
        db.upsert_intelligence_node(crate::db::intelligence::IntelligenceNodeInput {
            id: Some(id.to_string()),
            kind: Some("fact".to_string()),
            status: None,
            title: Some(title.to_string()),
            body: Some(format!("{title} body")),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some(hash)),
            session_id: None,
            relations: None,
        })
        .unwrap();
    }
    (base, p.hash, c.hash)
}

async fn upsert_large_intel_node(
    handler: &TaskTriggerHandler,
    id: &str,
    title: &str,
    body_marker: &str,
    relations: Option<Vec<IntelligenceRelationParams>>,
) {
    let body = format!("{body_marker}-{}", "x".repeat(12_000));
    let result = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some(id.to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some(title.to_string()),
                    body: Some(body),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
}

fn insert_standalone_spec(db: &Database, name: &str) -> GraphSpec {
    let spec = GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: None,
        name: name.to_string(),
        description: Some(valid_spec_description()),
        position: 0,
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    db.insert_graph_spec(&spec).unwrap();
    spec
}

fn spec_list_ids(body: &str) -> Vec<String> {
    serde_json::from_str::<serde_json::Value>(body).unwrap()["specs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_string())
        .collect()
}

/// Write a canopy config at `<home>/.canopy/config.toml` declaring one
/// `mistral` CLI carrying the CM32 registry identity. The binary is a
/// name that cannot exist anywhere, so every probe against it fails at
/// spawn — immediately, without launching a process or spending quota.
fn write_cm32_config(home: &std::path::Path) {
    std::fs::create_dir_all(home.join(".canopy")).unwrap();
    std::fs::write(
            home.join(".canopy/config.toml"),
            "[[clis]]\nname = \"mistral\"\nbinary = \"cm32-no-such-binary-xyz\"\nprovider = \"Mistral AI\"\ntool_name = \"Vibe\"\n",
        )
        .unwrap();
}

/// Write a canopy config at `<home>/.canopy` declaring one platform named
/// `freebuff` marked `headless = false` — the CM35 interactive-only
/// fixture (freebuff v0.1.2 has no print/headless mode). The binary is an
/// executable script that `touch`es a marker file if it is ever spawned,
/// and it carries a `models_list_cmd` so a handler that reaches the
/// enumeration path would run it. The returned marker path lets a test
/// prove a gate fired *before* any process started: the marker must not
/// exist afterwards.
fn write_freebuff_config(home: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let canopy_dir = home.join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let marker = home.join("freebuff-spawned-marker");
    let binary = home.join("freebuff-bin");
    std::fs::write(
        &binary,
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();

    crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "freebuff".to_string(),
            binary: binary.to_string_lossy().to_string(),
            headless: false,
            models_list_cmd: Some("--list".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }
    .save_for_tests(&canopy_dir)
    .unwrap();
    marker
}

fn crew_json_member(model: &str) -> crate::daemon::params::CrewMemberJson {
    crate::daemon::params::CrewMemberJson {
        platform: "openrouter".to_string(),
        model: Some(model.to_string()),
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// Build a graph with a check→ensemble→agent unit whose ensemble is bound
/// to the crew `eager`; returns `(graph_id, ensemble_id)`.
async fn build_crew_ensemble(
    handler: &TaskTriggerHandler,
    workdir: &str,
    graph_name: &str,
) -> (String, String) {
    let created = call_graph_action(
        handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: graph_name.to_string(),
            description: None,
            workdir: workdir.to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");
    let kickoff = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");
    let downstream = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");
    let ensemble = call_graph_action(
        handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Proposers".to_string(),
            prompt_template: Some("draft it".to_string()),
            node: None,
            members: None,
            condition: "always".to_string(),
            from_node: kickoff_id,
            on_pass_to: downstream_id,
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: Some("eager".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&ensemble), "{}", text(&ensemble));
    (graph_id, extract_id(&ensemble, "ensemble_id"))
}

/// Build a graph with a check→ensemble→agent unit whose ensemble has
/// LITERAL members (no crew binding); returns `(graph_id, ensemble_id)`.
async fn build_literal_ensemble(
    handler: &TaskTriggerHandler,
    workdir: &str,
    graph_name: &str,
) -> (String, String) {
    let created = call_graph_action(
        handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: graph_name.to_string(),
            description: None,
            workdir: workdir.to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");
    let kickoff = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");
    let downstream = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");
    let members = vec![
        crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    let ensemble = call_graph_action(
        handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Proposers".to_string(),
            prompt_template: Some("draft it".to_string()),
            node: None,
            members: Some(members),
            condition: "always".to_string(),
            from_node: kickoff_id,
            on_pass_to: downstream_id,
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&ensemble), "{}", text(&ensemble));
    (graph_id, extract_id(&ensemble, "ensemble_id"))
}

fn ensemble_update_params(ensemble_id: &str) -> GraphUpdateEnsembleParams {
    GraphUpdateEnsembleParams {
        mutex: None,
        ensemble_id: ensemble_id.to_string(),
        kind: None,
        prompt_template: None,
        members: None,
        crew: None,
        min_pass: None,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: None,
        on_fail_to: None,
        from_node: None,
        condition: None,
        add_entry_from: None,
        add_entry_condition: None,
        remove_entry_from: None,
        commit_rights: None,
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
