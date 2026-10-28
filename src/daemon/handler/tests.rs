//! `tests` tests (split from `src/daemon/handler.rs`, pure move).
use super::*;

use super::{
    active_graph_spec_context, build_ensemble_unit, build_get_tools_response,
    build_graph_completion_hook, build_graph_summary_json, build_graph_trigger,
    build_graph_update_response, build_id_result, build_json_result, build_spec_update_response,
    effective_member_prompt, find_in_flight_run, graph_details_json, graph_run_status_guard,
    graph_trigger_json, handle_retry_current_node, handle_skip_next_spec, header_str,
    in_flight_run_error, json_value_kind_name, member_node_config, missing_sync_identity_error,
    node_copy_note, node_name_for_error, perform_graph_reset, plan_ensemble_copy, plan_node_copy,
    queue_running_spec, rag_result_json, resolve_graph_target, resolve_node_kind_and_config,
    resolve_reported_run, spec_summary_json, unknown_config_keys, validate_absolute_dir,
    validate_at_least_one_bool, validate_edge_condition, validate_edge_condition_with_route,
    validate_ensemble_members, validate_node_config, validate_node_kind,
    validate_node_not_ensemble_owned, validate_non_empty, validate_not_join_kind,
    validate_queue_exists, validate_queue_member_removable, validate_queue_not_consumed,
    validate_queue_reorder, validate_queue_reorder_locking, validate_route_edge_target,
    validate_spec_deletable, validate_spec_exists, validate_spec_set_status_target,
    validate_spec_status, validate_spec_workdir, ActiveGraphSpec, BuiltEnsembleUnit,
    EnsembleMemberParams, EnsembleUnitSpec, HandlerDeps, TaskTriggerHandler,
    MISSING_SYNC_IDENTITY_MESSAGE,
};
use crate::daemon::params::{
    GraphCompletionHookParams, GraphCopyEnsembleParams, GraphCopyNodeParams,
    GraphScheduleAutorunParams, GraphScheduleContinueParams, GraphTriggerParams,
};
use crate::db::Database;
use crate::domain::graphs::{
    Ensemble, EnsembleKind, EnsembleMember, Graph, GraphEdge, GraphEdgeCondition, GraphNode,
    GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
};
use crate::domain::models::Trigger;
use crate::domain::queues::Queue;
use crate::shared::sync_identity::CANOPY_AGENT_ID_HEADER;
use tempfile::tempdir;

fn ensemble_member_params(platform: &str) -> EnsembleMemberParams {
    EnsembleMemberParams {
        platform: platform.to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

fn ensemble_member_params_with_prompt(
    platform: &str,
    prompt_override: &str,
) -> EnsembleMemberParams {
    EnsembleMemberParams {
        platform: platform.to_string(),
        model: None,
        prompt_override: Some(prompt_override.to_string()),
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

/// B7 A1 frozen inventory: sorted MCP tool names. Any added, removed, or
/// renamed tool fails `tool_inventory_is_byte_identical`.
const EXPECTED_TOOL_NAMES: &[&str] = &[
    "agent",
    "agent_edit",
    "agent_report",
    "config",
    "crew",
    "get_tools",
    "graph",
    "graph_complete_node",
    "graph_control",
    "graph_edge",
    "graph_edit",
    "graph_ensemble",
    "graph_node",
    "graph_report_blocker",
    "harness",
    "identity",
    "intelligence_admin",
    "intelligence_get_context",
    "intelligence_search",
    "intelligence_upsert",
    "loop_complete_node",
    "loop_report_blocker",
    "node",
    "project",
    "queue",
    "rag",
    "remote",
    "sandbox",
    "scheduled_send",
    "scheduled_send_create",
    "session_list",
    "session_send_file",
    "skill",
    "spec",
    "spec_edit",
    "subagent",
];

/// B7 A1 frozen snapshot: per-tool `name|sha256(description)|sha256(input_schema JSON)`.
/// Any byte drift in a description or schema fails `tool_inventory_is_byte_identical`.
/// The schema hashes were re-pinned for rmcp 2.x, whose `validate_and_strip` drops the
/// root `title`/`description` of each input schema (rmcp 1.6 kept them). Properties,
/// required fields and tool descriptions are unchanged by that.
const EXPECTED_TOOL_ROWS: &[&str] = &[
    "agent|d8f9c6d2e27d78721e0e68301793b430eb079a49d68e003c0a36062ade4326c2|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "agent_edit|9f9d97739be8934e8023b86d6ed35d9acf9209d18d42a8cafd8ffc99154965cf|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "agent_report|998eb6d649fe53809656f0a4db81248aa0feb0b045f957686d917fbcfe289224|f40f986d373435cf862cf0e260812d92f78eaf4bfcfbe98ffce5d724a1685d54",
    "config|9f0c345de8207c8c734ceec7e64bcc92f46e237d6e7ac57ac80d6b90a00622fc|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "crew|886ff7c084a09bd0b3b7927447619bde9e91ee322e31e828a10ca6b14630d563|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "get_tools|1e7a235797cc4043ef72fbe9821989c7fdf78c19dfb4afa2f97dec22b28ad2b9|8d1085667f9f7b280bc979a52911ce87934bee573c8dd9a35175afad22365c8f",
    "graph|d430e133b8f3fb2f69fa901969a339a14191f3cd2da796bfccd5887a44709d82|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_complete_node|cd65852e0bb702902c8fc61858fb514f0f75d749de39d9014f2ae037d39a5a24|174f4383731a376d4ffd07080c26cbc61bd66839093939ea3f6defaebd64cd5e",
    "graph_control|6ea48b9f5a7efdcb8091015b809f91686f69870f05e7f83b077744334a521603|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_edge|dc837b1ca327d911a1f60b27d4546747dcb70411b35c7a51d4954256e73987de|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_edit|d5c0c9ad02ee3f28ccea179274e9a3410f8458a9ddb43f87123119ce9e92f71f|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_ensemble|de9b6ae3f76602a7e5b1f8dfc1dbd36a127265dba6149340b3fe1710ee052ec7|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_node|7fce2b505b0d7e9dc1256d68c5ded89516a89bbbbcd5ea128cc35d360793141c|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "graph_report_blocker|0af53e5791486ff4aacdfd49ff7e2a12171ff71ba4825fe03147683c8bea8d95|55366b14da2aae0cf6af2fb6f92900d8db343c7051e7d01dc025f8196b8bf599",
    "harness|059ab2d487f4740c696909f05886053ef5816f6c49458ce59f8d0362fff7e278|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "identity|a180db61cb53b2cc3122327c72750ea7c43b9933ea1890d49cba8a1e07418915|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "intelligence_admin|530a0447f22ba50dc5945f1c5db22db37ec16b2377b3fccf2ec6da346b8dfd0e|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "intelligence_get_context|669fe1f581e27a2bdf49f09765a42bc8f1f0879bf49c0466ff072ed315e73e33|21815b7ab3af1404c86b99df1bd52c8d73a656d5ffa2229c6ba6e7b95402b58f",
    "intelligence_search|a91c69fe51924face0f4a2a6c4777b9dde708606f68cc44c7c46d16719d3f67c|fa7481fcecc2528cae994223e58076d4818ac3f55cb99577e71a45e5d85c395e",
    "intelligence_upsert|164de5427fff75a57508a5da581435374ebd7176ced490cf2e21577294d1aa01|7f55a980873729de4e6ae987d684496da34ac296de5d6d75603bf72dc039db1d",
    "loop_complete_node|a3a48975f0dafdc50569ac00a150cb091479281e7339f112708580a1764f95e8|174f4383731a376d4ffd07080c26cbc61bd66839093939ea3f6defaebd64cd5e",
    "loop_report_blocker|9fb9038c5d224015aa25b157730b99339dd91372deb3c1460abb1e5e4fad2081|55366b14da2aae0cf6af2fb6f92900d8db343c7051e7d01dc025f8196b8bf599",
    "node|b3eb6b349c7f9a09be75c5537612e2fd087fc80d9dbb406a6f2bb9f8d213e097|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "project|c65b957f5281b52bc552e5d1b03c7647d3e9f4761341e52f06c61238c8f6a4bb|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "queue|c9468c5708f09f8830ada49db64c145bc9dce653035ebc56499df454fc82143f|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "rag|9e6c40757677f90c354176984896506760fe8b81b8102b4f277c9228e4add4a8|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "remote|01ec2dbcbf307f08b6af5b2c5ea15ace1bd65d6ca04de00bcc81ed5c81d87b66|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "sandbox|aebc31a471c120da0dc64bcb6b8787f6161a8d28bbe7b956deca5121033bba5a|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "scheduled_send|ed00bebe0c040774e21acbddb045f13871c4d7192366307e5a7312c80b4c588c|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "scheduled_send_create|e6defc41d6d01fe133056562efc3aba3b50b6d1e2a06fe278894cbbbcb65a165|959aa8850eee97391ca1529377085f07115c926742f575ceac2d01e143b80880",
    "session_list|f983db33c96519c95584055f90c807dda350a17e9b12d402c0551590e772532b|adae59573f649012ccb3856be07ea7740581587cfeb6e9caba07ab66ca88ec84",
    "session_send_file|93da0ac3f05d1272b9d2655352a1b6dccdf20724d589476b704396682b0bc914|164c349fdc53ee0b35afdfe6d750e6c23e8622f1426bc8c3aa84ff6e93f9c613",
    "skill|0f77b55bf9e5bff59383d9d96a628615e4ce85cb0f780ddff691be95175a2943|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "spec|3a76fcb9ac04bc4e33bf9e92b34c5a16ab77d4f9fd73e8411eba177341ab04ea|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "spec_edit|ad4f5ead3b9a869f25be22ec633cd75f6d09d7d13d54379040e108fe19c046b6|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
    "subagent|5e0c38144198037deab293244b2839d8b17dad4cd1afc1f9ea76952712e95657|deecb0fe13d120970845f9ad8cacb3c7a8c98df822f46e4f606a017553b7bc37",
];

fn spec_with_status(graph_id: &str, id: &str, position: i64, status: GraphSpecStatus) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: Some(graph_id.to_string()),
        name: id.to_string(),
        description: None,
        position,
        parallelizable: false,
        status,
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
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
    }
}

fn graph_reset_fixture(status: GraphStatus) -> (tempfile::TempDir, Database, String) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-reset-test".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
    (dir, db, graph_id)
}

fn router_config(routes: &serde_json::Value, fallback: &str) -> serde_json::Value {
    serde_json::json!({ "routes": routes, "fallback": fallback })
}

fn two_routes_json() -> serde_json::Value {
    serde_json::json!([
        { "label": "retry", "description": "Retry the current step." },
        { "label": "escalate", "description": "Hand off to a human." },
    ])
}

fn standalone_spec(id: &str) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: None,
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
    }
}

fn queue_test_db() -> (tempfile::TempDir, Database) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    (dir, db)
}

fn insert_queue(db: &Database, id: &str) {
    db.insert_queue(&Queue {
        id: id.to_string(),
        name: format!("{id}-name"),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

/// Build a fully-wired `TaskTriggerHandler` over an in-memory-ish temp DB
/// so the queue `#[tool]` methods (and their shared `do_queue_*` helpers)
/// can be exercised end-to-end.
fn queue_test_handler() -> (
    tempfile::TempDir,
    std::sync::Arc<Database>,
    TaskTriggerHandler,
) {
    use crate::application::notification_service::{
        DefaultNotificationService, NotificationService,
    };
    use crate::executor::Executor;
    use crate::graph_engine::GraphEngine;
    use crate::rag::ingestion::IngestionManager;
    use crate::watchers::WatcherEngine;
    use std::sync::Arc;
    use tokio::sync::Notify;

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
        shelf: crate::sessions::shelf::ShelfStore::for_test(&db),
        start_time: std::time::Instant::now(),
        port: 0,
    });
    (dir, db, handler)
}

fn result_text(result: &rmcp::model::CallToolResult) -> String {
    format!("{:?}", result.content)
}

fn queue_raw_text(result: &rmcp::model::CallToolResult) -> String {
    result.content[0].as_text().unwrap().text.clone()
}

fn queue_is_err(result: &rmcp::model::CallToolResult) -> bool {
    result.is_error == Some(true)
}

fn running_spec(id: &str) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.status = GraphSpecStatus::Running;
    spec
}

fn interrupted_spec(id: &str) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.status = GraphSpecStatus::Interrupted;
    spec
}

/// A minimal graph row, needed only to satisfy `graph_runs.graph_id`'s FK.
fn insert_test_graph(db: &Database, id: &str) {
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
}

/// A minimal node row, needed only to satisfy `graph_runs.node_id`'s FK.
fn insert_test_node(db: &Database, id: &str, spec_id: &str) {
    db.insert_graph_node(&GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

fn graph_run_row(id: &str, graph_id: &str, spec_id: &str, status: GraphRunStatus) -> GraphNodeRun {
    GraphNodeRun {
        id: id.to_string(),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: "node-1".to_string(),
        status,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: (status != GraphRunStatus::Running).then(chrono::Utc::now),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }
}

// ── B12: stale graph_complete_node/graph_report_blocker reports ─────

fn reported_run_fixture() -> (tempfile::TempDir, Database) {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
    insert_test_node(&db, "node-1", "spec-a");
    (dir, db)
}

/// CB30 test helpers: a blank-name placeholder bound to a graph (engine
/// bookkeeping for spec-less dispatches) and a named spec bound to a
/// graph or standing alone as a queue member.
fn cb30_test_graph(id: &str, status: GraphStatus) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

fn placeholder_spec(id: &str, graph_id: &str, status: GraphSpecStatus) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.graph_id = Some(graph_id.to_string());
    spec.name = String::new();
    spec.status = status;
    spec
}

fn named_spec(
    id: &str,
    graph_id: Option<&str>,
    name: &str,
    position: i64,
    status: GraphSpecStatus,
) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.graph_id = graph_id.map(str::to_string);
    spec.name = name.to_string();
    spec.position = position;
    spec.status = status;
    spec
}

fn queue_driven_test_db() -> (tempfile::TempDir, Database) {
    // A running graph drawing from a queue with two named members, plus
    // the blank-name placeholder the engine binds to the graph itself.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut lp = cb30_test_graph("graph-q", GraphStatus::Running);
    lp.active_run_queue_id = Some("q1".to_string());
    db.insert_graph(&lp).unwrap();
    insert_queue(&db, "q1");
    db.insert_graph_spec(&placeholder_spec("ph", "graph-q", GraphSpecStatus::Running))
        .unwrap();
    db.insert_graph_spec(&named_spec(
        "spec-a",
        None,
        "CM4 — queue work",
        0,
        GraphSpecStatus::Running,
    ))
    .unwrap();
    db.append_queue_member("q1", "spec-a", None).unwrap();
    db.insert_graph_spec(&named_spec(
        "spec-b",
        None,
        "CM6 — next work",
        0,
        GraphSpecStatus::Pending,
    ))
    .unwrap();
    db.append_queue_member("q1", "spec-b", None).unwrap();
    (dir, db)
}

// ── B41: cancel a scheduled autorun via graph_control(at=None) ──

fn autorun_test_graph(graph_id: &str, workdir: &str, status: GraphStatus) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.to_string(),
        name: graph_id.to_string(),
        description: None,
        workdir: workdir.to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

// ── U10: copy nodes and ensembles ────────────────────────────────

fn u10_check(id: &str, spec_id: Option<&str>, graph_id: Option<&str>, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position,
        created_at: chrono::Utc::now(),
    }
}

fn u10_agent(
    id: &str,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    position: i64,
    config: serde_json::Value,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: id.to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position,
        created_at: chrono::Utc::now(),
    }
}

/// A persisted source ensemble ("proposers"): two members (claude, codex/o1),
/// wired `from` → members → join → `to`.
fn u10_source_ensemble(
    db: &Database,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    from: &str,
    to: &str,
) -> BuiltEnsembleUnit {
    let members = [
        ("claude".to_string(), None, None, None, None, None, None),
        (
            "codex".to_string(),
            Some("o1".to_string()),
            None,
            None,
            None,
            None,
            None,
        ),
    ];
    let built = build_ensemble_unit(&EnsembleUnitSpec {
        mutex: Vec::new(),
        spec_id: spec_id.map(str::to_string),
        graph_id: graph_id.map(str::to_string),
        name: "proposers",
        prompt_template: "propose a solution",
        members: &members,
        entry_from_node: from,
        entry_condition: GraphEdgeCondition::Always,
        on_pass_to: to,
        on_pass_fan_out: None,
        on_fail_to: None,
        on_fail_fan_out: None,
        min_pass: 2,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        start_position: 10,
        kind: EnsembleKind::Parallel,
        commit_rights: false,
    });
    db.insert_ensemble_unit(
        &built.ensemble,
        &built.members,
        &built.member_nodes,
        &built.join_node,
        &built.edges,
    )
    .unwrap();
    built
}

/// Insert a standalone spec (no owning graph) so a test can hang nodes
/// off it without needing a full `Graph` row too.
fn insert_standalone_spec(db: &Database, id: &str) {
    db.insert_graph_spec(&GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();
}

// ── graph_trigger_json ───────────────────────────────────────────

fn make_graph_with_trigger(trigger: Option<Trigger>) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: "graph-1".to_string(),
        name: "Test Graph".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

mod part01_tests;
mod part02_tests;
mod part03_tests;
mod part04_tests;
