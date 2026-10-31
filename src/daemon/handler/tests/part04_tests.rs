//! Part 04 of `tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[test]
fn validate_route_edge_target_rejects_undeclared_route() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_standalone_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "router-1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Router".to_string(),
        kind: GraphNodeKind::Router,
        config: router_config(&two_routes_json(), "retry"),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let err = validate_route_edge_target(
        &db,
        "router-1",
        &GraphEdgeCondition::Route("nonexistent".to_string()),
    )
    .unwrap_err();
    assert!(err.contains("undeclared route 'nonexistent'"), "{err}");
}

#[test]
fn validate_route_edge_target_accepts_declared_route() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_standalone_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "router-1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Router".to_string(),
        kind: GraphNodeKind::Router,
        config: router_config(&two_routes_json(), "retry"),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    assert!(validate_route_edge_target(
        &db,
        "router-1",
        &GraphEdgeCondition::Route("escalate".to_string()),
    )
    .is_ok());
}

// ── validate_node_kind ──────────────────────────────────────────

#[test]
fn validate_node_kind_accepts_agent() {
    assert_eq!(validate_node_kind("agent").unwrap(), GraphNodeKind::Agent);
}

#[test]
fn validate_node_kind_accepts_check() {
    assert_eq!(validate_node_kind("check").unwrap(), GraphNodeKind::Check);
}

#[test]
fn validate_node_kind_accepts_gate() {
    assert_eq!(validate_node_kind("gate").unwrap(), GraphNodeKind::Gate);
}

#[test]
fn validate_node_kind_accepts_join() {
    assert_eq!(validate_node_kind("join").unwrap(), GraphNodeKind::Join);
}

#[test]
fn validate_node_kind_trims_whitespace() {
    assert_eq!(
        validate_node_kind("  agent  ").unwrap(),
        GraphNodeKind::Agent
    );
}

#[test]
fn validate_node_kind_rejects_invalid() {
    let err = validate_node_kind("invalid").unwrap_err();
    assert!(err.contains("agent"), "{err}");
    assert!(err.contains("check"), "{err}");
    assert!(err.contains("gate"), "{err}");
}

// ── validate_not_join_kind ──────────────────────────────────────

#[test]
fn validate_not_join_kind_rejects_join() {
    let err = validate_not_join_kind(GraphNodeKind::Join).unwrap_err();
    assert!(err.contains("engine-managed"), "{err}");
    assert!(err.contains("graph_ensemble"), "{err}");
}

#[test]
fn validate_not_join_kind_accepts_agent() {
    assert!(validate_not_join_kind(GraphNodeKind::Agent).is_ok());
}

#[test]
fn validate_not_join_kind_accepts_check() {
    assert!(validate_not_join_kind(GraphNodeKind::Check).is_ok());
}

#[test]
fn validate_not_join_kind_accepts_gate() {
    assert!(validate_not_join_kind(GraphNodeKind::Gate).is_ok());
}

// ── validate_spec_status ────────────────────────────────────────

#[test]
fn validate_spec_status_accepts_all_valid_statuses() {
    assert_eq!(
        validate_spec_status("pending").unwrap(),
        GraphSpecStatus::Pending
    );
    assert_eq!(
        validate_spec_status("running").unwrap(),
        GraphSpecStatus::Running
    );
    assert_eq!(
        validate_spec_status("completed").unwrap(),
        GraphSpecStatus::Completed
    );
    assert_eq!(
        validate_spec_status("failed").unwrap(),
        GraphSpecStatus::Failed
    );
    assert_eq!(
        validate_spec_status("skipped").unwrap(),
        GraphSpecStatus::Skipped
    );
    assert_eq!(
        validate_spec_status("interrupted").unwrap(),
        GraphSpecStatus::Interrupted
    );
}

#[test]
fn validate_spec_status_is_case_insensitive() {
    assert_eq!(
        validate_spec_status("PENDING").unwrap(),
        GraphSpecStatus::Pending
    );
    assert_eq!(
        validate_spec_status("Running").unwrap(),
        GraphSpecStatus::Running
    );
}

#[test]
fn validate_spec_status_trims_whitespace() {
    assert_eq!(
        validate_spec_status("  pending  ").unwrap(),
        GraphSpecStatus::Pending
    );
}

#[test]
fn validate_spec_status_rejects_invalid() {
    let err = validate_spec_status("unknown").unwrap_err();
    assert!(err.contains("pending"), "{err}");
    assert!(err.contains("running"), "{err}");
    assert!(err.contains("completed"), "{err}");
    assert!(err.contains("failed"), "{err}");
    assert!(err.contains("skipped"), "{err}");
}

// ── validate_spec_set_status_target ─────────────────────────────

#[test]
fn validate_spec_set_status_target_accepts_valid() {
    assert_eq!(
        validate_spec_set_status_target("pending").unwrap(),
        GraphSpecStatus::Pending
    );
    assert_eq!(
        validate_spec_set_status_target("completed").unwrap(),
        GraphSpecStatus::Completed
    );
    assert_eq!(
        validate_spec_set_status_target("skipped").unwrap(),
        GraphSpecStatus::Skipped
    );
}

#[test]
fn validate_spec_set_status_target_rejects_running() {
    let err = validate_spec_set_status_target("running").unwrap_err();
    assert!(err.contains("pending"), "{err}");
    assert!(err.contains("completed"), "{err}");
    assert!(err.contains("skipped"), "{err}");
}

#[test]
fn validate_spec_set_status_target_rejects_failed() {
    let err = validate_spec_set_status_target("failed").unwrap_err();
    assert!(err.contains("pending"), "{err}");
    assert!(err.contains("completed"), "{err}");
    assert!(err.contains("skipped"), "{err}");
}

#[test]
fn validate_spec_set_status_target_rejects_invalid() {
    assert!(validate_spec_set_status_target("unknown").is_err());
}

// ── json_value_kind_name ────────────────────────────────────────

#[test]
fn json_value_kind_name_null() {
    assert_eq!(json_value_kind_name(&serde_json::Value::Null), "null");
}

#[test]
fn json_value_kind_name_bool() {
    assert_eq!(json_value_kind_name(&serde_json::json!(true)), "a boolean");
}

#[test]
fn json_value_kind_name_number() {
    assert_eq!(json_value_kind_name(&serde_json::json!(42)), "a number");
}

#[test]
fn json_value_kind_name_string() {
    assert_eq!(
        json_value_kind_name(&serde_json::json!("hello")),
        "a JSON-encoded string"
    );
}

#[test]
fn json_value_kind_name_array() {
    assert_eq!(
        json_value_kind_name(&serde_json::json!([1, 2, 3])),
        "an array"
    );
}

#[test]
fn json_value_kind_name_object() {
    assert_eq!(
        json_value_kind_name(&serde_json::json!({"key": "value"})),
        "an object"
    );
}

// ── validate_at_least_one_bool ──────────────────────────────────

#[test]
fn validate_at_least_one_bool_rejects_all_false() {
    let err = validate_at_least_one_bool(&[false, false, false], "fields").unwrap_err();
    assert!(err.contains("fields"), "{err}");
    assert!(err.contains("at least one"), "{err}");
}

#[test]
fn validate_at_least_one_bool_accepts_one_true() {
    assert!(validate_at_least_one_bool(&[false, true, false], "fields").is_ok());
}

#[test]
fn validate_at_least_one_bool_accepts_all_true() {
    assert!(validate_at_least_one_bool(&[true, true, true], "fields").is_ok());
}

// ── validate_queue_reorder ───────────────────────────────────────

#[test]
fn validate_queue_reorder_accepts_valid_permutation() {
    let current = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let reordered = vec!["c".to_string(), "a".to_string(), "b".to_string()];
    assert!(validate_queue_reorder(&current, &reordered).is_ok());
}

#[test]
fn validate_queue_reorder_accepts_same_order() {
    let current = vec!["a".to_string(), "b".to_string()];
    let reordered = vec!["a".to_string(), "b".to_string()];
    assert!(validate_queue_reorder(&current, &reordered).is_ok());
}

#[test]
fn validate_queue_reorder_rejects_length_mismatch() {
    let current = vec!["a".to_string(), "b".to_string()];
    let reordered = vec!["a".to_string()];
    let err = validate_queue_reorder(&current, &reordered).unwrap_err();
    assert!(err.contains("2"), "{err}");
    assert!(err.contains("1"), "{err}");
}

#[test]
fn validate_queue_reorder_rejects_duplicate() {
    let current = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let reordered = vec!["a".to_string(), "a".to_string(), "b".to_string()];
    let err = validate_queue_reorder(&current, &reordered).unwrap_err();
    assert!(err.contains("a"), "{err}");
    assert!(err.contains("more than once"), "{err}");
}

#[test]
fn validate_queue_reorder_rejects_unknown_id() {
    let current = vec!["a".to_string(), "b".to_string()];
    let reordered = vec!["a".to_string(), "x".to_string()];
    let err = validate_queue_reorder(&current, &reordered).unwrap_err();
    assert!(err.contains("x"), "{err}");
    assert!(err.contains("no spec"), "{err}");
}

// ── spec_summary_json ───────────────────────────────────────────

#[test]
fn spec_summary_json_includes_required_fields() {
    let spec = GraphSpec {
        id: "spec-1".to_string(),
        graph_id: Some("graph-1".to_string()),
        name: "My Spec".to_string(),
        description: Some("does stuff".to_string()),
        position: 3,
        parallelizable: true,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: Some("/tmp/project".to_string()),
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    let json = spec_summary_json(&spec, true);
    assert_eq!(json["id"], "spec-1");
    assert_eq!(json["graph_id"], "graph-1");
    assert_eq!(json["name"], "My Spec");
    assert_eq!(json["description"], "does stuff");
    assert_eq!(json["position"], 3);
    assert_eq!(json["parallelizable"], true);
    assert_eq!(json["status"], "pending");
    assert_eq!(json["workdir"], "/tmp/project");
    assert!(json.get("completed_via").is_none());
}

#[test]
fn spec_summary_json_includes_completed_via_when_set() {
    let spec = GraphSpec {
        id: "spec-2".to_string(),
        graph_id: None,
        name: "Done".to_string(),
        description: None,
        position: 1,
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
        completed_via: Some("graph_control".to_string()),
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    let json = spec_summary_json(&spec, true);
    assert_eq!(json["completed_via"], "graph_control");
}

// ── build_id_result / build_json_result ─────────────────────────

#[test]
fn build_id_result_contains_key_and_id() {
    let result = build_id_result("abc-123", "graph_id");
    let text = format!("{:?}", result.content);
    assert!(text.contains("abc-123"), "{text}");
    assert!(text.contains("graph_id"), "{text}");
    assert!(result.is_error != Some(true));
}

#[test]
fn build_json_result_contains_value() {
    let value = serde_json::json!({"key": "value", "count": 42});
    let result = build_json_result(&value);
    let text = format!("{:?}", result.content);
    assert!(text.contains("key"), "{text}");
    assert!(text.contains("value"), "{text}");
    assert!(text.contains("42"), "{text}");
    assert!(result.is_error != Some(true));
}

#[test]
fn graph_trigger_json_manual() {
    let lp = make_graph_with_trigger(None);
    let json = graph_trigger_json(&lp);
    assert_eq!(json["type"], "manual");
    assert!(json.get("schedule").is_none());
    assert!(json.get("path").is_none());
}

#[test]
fn graph_trigger_json_cron() {
    let lp = make_graph_with_trigger(Some(Trigger::Cron {
        schedule_expr: "0 9 * * *".to_string(),
    }));
    let json = graph_trigger_json(&lp);
    assert_eq!(json["type"], "cron");
    assert_eq!(json["schedule"], "0 9 * * *");
}

#[test]
fn graph_trigger_json_watch() {
    let lp = make_graph_with_trigger(Some(Trigger::Watch {
        path: "/tmp/project".to_string(),
        events: vec![
            crate::domain::models::WatchEvent::Create,
            crate::domain::models::WatchEvent::Modify,
        ],
        debounce_seconds: 3,
        recursive: true,
    }));
    let json = graph_trigger_json(&lp);
    assert_eq!(json["type"], "watch");
    assert_eq!(json["path"], "/tmp/project");
    let events = json["events"].as_array().unwrap();
    assert!(events.contains(&serde_json::json!("create")));
    assert!(events.contains(&serde_json::json!("modify")));
}

// ── build_get_tools_response ────────────────────────────────────

#[test]
fn build_get_tools_response_session_start() {
    let json = build_get_tools_response("session_start");
    assert_eq!(json["scope"], "session_start");
    assert_eq!(json["risk"], "low");
    assert!(json["protocol"].is_array());
    assert!(json["tools"].is_array());
}

#[test]
fn build_get_tools_response_file_write() {
    let json = build_get_tools_response("file_write");
    assert_eq!(json["scope"], "file_write");
    assert_eq!(json["risk"], "high");
}

#[test]
fn build_get_tools_response_test_run() {
    let json = build_get_tools_response("test_run");
    assert_eq!(json["scope"], "test_run");
    assert_eq!(json["risk"], "medium");
}

#[test]
fn build_get_tools_response_close_session() {
    let json = build_get_tools_response("close_session");
    assert_eq!(json["scope"], "close_session");
    assert_eq!(json["risk"], "low");
}

#[test]
fn build_get_tools_response_multi_agent() {
    let json = build_get_tools_response("multi_agent");
    assert_eq!(json["scope"], "multi_agent");
    assert_eq!(json["risk"], "varies");
}

// ── rag_result_json ─────────────────────────────────────────────

#[test]
fn rag_result_json_includes_all_fields() {
    let search_result = crate::rag::vector_store::SearchResult {
        id: "sr-1".to_string(),
        file_path: "/tmp/doc.md".to_string(),
        content: "some content here".to_string(),
        created_at: 0,
        distance: Some(0.42f32),
    };
    let json = rag_result_json(&search_result);
    assert_eq!(json["source"], "/tmp/doc.md");
    assert_eq!(json["content"], "some content here");
    assert!(json["distance"].as_f64().unwrap() > 0.41);
    assert!(json["distance"].as_f64().unwrap() < 0.43);
}

// ── node_copy_note ──────────────────────────────────────────────

#[test]
fn node_copy_note_wired() {
    let note = node_copy_note("src-1", "new-1", true);
    assert!(note.contains("src-1"), "{note}");
    assert!(note.contains("new-1"), "{note}");
    assert!(!note.contains("NO"), "{note}");
}

#[test]
fn node_copy_note_unwired() {
    let note = node_copy_note("src-2", "new-2", false);
    assert!(note.contains("src-2"), "{note}");
    assert!(note.contains("new-2"), "{note}");
    assert!(note.contains("NO"), "{note}");
    assert!(note.contains("graph_edge"), "{note}");
}

// ── build_graph_update_response / build_spec_update_response ──

#[test]
fn build_graph_update_response_contains_graph_id() {
    let result = build_graph_update_response("graph-42", None);
    let text = format!("{:?}", result.content);
    assert!(text.contains("graph-42"), "{text}");
    assert!(text.contains("updated"), "{text}");
    assert!(result.is_error != Some(true));
}

#[test]
fn build_spec_update_response_contains_spec_id() {
    let result = build_spec_update_response("spec-99");
    let text = format!("{:?}", result.content);
    assert!(text.contains("spec-99"), "{text}");
    assert!(text.contains("updated"), "{text}");
    assert!(result.is_error != Some(true));
}

// ── validate_node_config edge cases ─────────────────────────────

#[test]
fn validate_node_config_gate_without_evaluate_defaults_to_output_contains() {
    // When "evaluate" is absent, it defaults to "output_contains", so "value" is required.
    let config = serde_json::json!({});
    let err = validate_node_config(GraphNodeKind::Gate, &config).unwrap_err();
    assert!(err.contains("value"), "{err}");
}

#[test]
fn validate_node_config_gate_with_non_output_contains_evaluate_skips_value_check() {
    let config = serde_json::json!({"evaluate": "exit_code_0"});
    assert!(validate_node_config(GraphNodeKind::Gate, &config).is_ok());
}

#[test]
fn validate_node_config_agent_with_cli_only() {
    let config = serde_json::json!({"cli": "codex"});
    assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
}

#[test]
fn validate_node_config_agent_with_both_platform_and_cli() {
    let config = serde_json::json!({"platform": "claude", "cli": "codex"});
    assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
}

#[test]
fn validate_node_config_rejects_non_object() {
    let cases = [
        serde_json::json!(null),
        serde_json::json!(42),
        serde_json::json!("string"),
        serde_json::json!([1, 2]),
    ];
    for config in &cases {
        let err = validate_node_config(GraphNodeKind::Agent, config).unwrap_err();
        assert!(err.contains("JSON object"), "{err}");
    }
}

#[test]
fn validate_node_config_agent_rejects_empty_platform_and_cli() {
    let config = serde_json::json!({"platform": "", "cli": ""});
    let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(err.contains("platform"), "{err}");
}

#[test]
fn validate_node_config_agent_rejects_whitespace_only_platform() {
    let config = serde_json::json!({"platform": "   "});
    let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(err.contains("platform"), "{err}");
}

// ── missing_sync_identity_error ─────────────────────────────────

#[test]
fn missing_sync_identity_error_message_matches_constant() {
    let err = missing_sync_identity_error();
    assert_eq!(err.message, MISSING_SYNC_IDENTITY_MESSAGE);
}

// ── GraphEdgeCondition::from_str / GraphNodeKind::from_str (domain) ──

#[test]
fn graph_edge_condition_from_str_roundtrip() {
    for cond in [
        GraphEdgeCondition::Pass,
        GraphEdgeCondition::Fail,
        GraphEdgeCondition::Always,
    ] {
        let s = cond.as_str();
        assert_eq!(GraphEdgeCondition::from_str(s), Some(cond));
    }
}

#[test]
fn graph_edge_condition_from_str_invalid() {
    assert_eq!(GraphEdgeCondition::from_str("sometimes"), None);
    assert_eq!(GraphEdgeCondition::from_str(""), None);
}

#[test]
fn validate_edge_condition_rejects_legacy_break() {
    let err = validate_edge_condition("break").unwrap_err();
    assert!(err.contains("pass, fail, always, error"), "{err}");
}

#[test]
fn graph_node_kind_from_str_roundtrip() {
    for kind in [
        GraphNodeKind::Agent,
        GraphNodeKind::Check,
        GraphNodeKind::Gate,
        GraphNodeKind::Join,
    ] {
        let s = kind.as_str();
        assert_eq!(GraphNodeKind::from_str(s), Some(kind));
    }
}

#[test]
fn graph_node_kind_from_str_invalid() {
    assert_eq!(GraphNodeKind::from_str("invalid"), None);
    assert_eq!(GraphNodeKind::from_str(""), None);
}

#[test]
fn graph_spec_status_from_str_roundtrip() {
    for status in [
        GraphSpecStatus::Pending,
        GraphSpecStatus::Running,
        GraphSpecStatus::Completed,
        GraphSpecStatus::Failed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Interrupted,
    ] {
        let s = status.as_str();
        assert_eq!(GraphSpecStatus::from_str(s), status);
    }
}

#[test]
fn graph_spec_status_from_str_defaults_to_pending() {
    assert_eq!(
        GraphSpecStatus::from_str("unknown"),
        GraphSpecStatus::Pending
    );
    assert_eq!(GraphSpecStatus::from_str(""), GraphSpecStatus::Pending);
}

// ── build_graph_trigger: cron with invalid expression ────────────

#[test]
fn build_graph_trigger_cron_invalid_expression() {
    let params = GraphTriggerParams {
        kind: "cron".to_string(),
        schedule: Some("not-a-cron".to_string()),
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("Invalid cron expression"), "{err}");
}

#[test]
fn build_graph_trigger_cron_valid_expression() {
    let params = GraphTriggerParams {
        kind: "cron".to_string(),
        schedule: Some("*/5 * * * *".to_string()),
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    let trigger = build_graph_trigger(&Some(params)).unwrap().unwrap();
    match trigger {
        Trigger::Cron { schedule_expr } => {
            assert_eq!(schedule_expr, "*/5 * * * *");
        }
        other => panic!("expected Trigger::Cron, got {other:?}"),
    }
}

// ── FR1/FR5: typed resource-tool input schemas ──────────────────────

use crate::daemon::actions::ToolSpec;
use crate::daemon::handler::{
    call_graph_action, call_graph_action_value, call_resource_action, call_resource_action_value,
};
use rmcp::model::ErrorCode;

/// Every resource tool dispatched through a `ToolSpec`, with its action
/// table (all 24 — the schema-coverage test walks this list, so a
/// resource tool missing from it, or an action parameter missing from the
/// published schema, fails there).
const RESOURCE_TOOL_SPECS: &[(&str, &ToolSpec)] = &[
    ("agent", &crate::daemon::handler_resource_tools::AGENT),
    (
        "agent_edit",
        &crate::daemon::handler_resource_tools::AGENT_EDIT,
    ),
    ("config", &crate::daemon::handler_resource_tools::CONFIG),
    ("crew", &crate::daemon::handler_resource_tools::CREW),
    ("graph", &crate::daemon::handler_graph_tools::GRAPH),
    (
        "graph_control",
        &crate::daemon::handler_graph_tools::GRAPH_CONTROL,
    ),
    (
        "graph_edge",
        &crate::daemon::handler_graph_tools::GRAPH_EDGE,
    ),
    (
        "graph_edit",
        &crate::daemon::handler_graph_tools::GRAPH_EDIT,
    ),
    (
        "graph_ensemble",
        &crate::daemon::handler_graph_tools::GRAPH_ENSEMBLE,
    ),
    (
        "graph_node",
        &crate::daemon::handler_graph_tools::GRAPH_NODE,
    ),
    ("harness", &crate::daemon::handler_resource_tools::HARNESS),
    (
        "identity",
        &crate::daemon::handler_resource_tools_final::IDENTITY,
    ),
    (
        "intelligence_admin",
        &crate::daemon::handler_resource_tools_final::INTELLIGENCE_ADMIN,
    ),
    ("node", &crate::daemon::handler_resource_tools_final::NODE),
    (
        "project",
        &crate::daemon::handler_resource_tools_final::PROJECT,
    ),
    ("queue", &crate::daemon::handler_resource_tools::QUEUE),
    ("rag", &crate::daemon::handler_resource_tools_final::RAG),
    ("remote", &crate::daemon::handler_remote::REMOTE),
    ("sandbox", &crate::daemon::handler_sandbox::SANDBOX),
    (
        "scheduled_prompts",
        &crate::daemon::handler_resource_tools_final::SCHEDULED_PROMPTS,
    ),
    ("skill", &crate::daemon::handler_resource_tools_final::SKILL),
    ("spec", &crate::daemon::handler_resource_tools::SPEC),
    (
        "spec_edit",
        &crate::daemon::handler_resource_tools::SPEC_EDIT,
    ),
    (
        "subagent",
        &crate::daemon::handler_resource_tools_final::SUBAGENT,
    ),
];

/// Every JSON type name a (possibly union) schema declares, collected
/// shallowly; empty when the schema declares no type at all.
fn declared_type_names(shape: &serde_json::Value) -> Vec<String> {
    use serde_json::Value;
    let mut names = Vec::new();
    match shape.get("type") {
        Some(Value::String(single)) => names.push(single.clone()),
        Some(Value::Array(candidates)) => names.extend(
            candidates
                .iter()
                .filter_map(|candidate| candidate.as_str().map(str::to_string)),
        ),
        _ => {}
    }
    if names.is_empty() {
        for combinator in ["anyOf", "oneOf"] {
            if let Some(variants) = shape.get(combinator).and_then(Value::as_array) {
                for variant in variants {
                    names.extend(declared_type_names(variant));
                }
            }
        }
    }
    if names.is_empty() {
        if shape.get("properties").is_some() {
            names.push("object".to_string());
        } else if shape.get("items").is_some() {
            names.push("array".to_string());
        }
    }
    names
}

#[test]
fn schema_covers_every_action_param_of_every_resource_tool() {
    let tools = TaskTriggerHandler::tool_router().list_all();
    assert_eq!(
        RESOURCE_TOOL_SPECS.len(),
        24,
        "all 24 resource tools listed"
    );
    for (name, spec) in RESOURCE_TOOL_SPECS {
        let tool = tools
            .iter()
            .find(|tool| tool.name.as_ref() == *name)
            .unwrap_or_else(|| panic!("resource tool `{name}` missing from tools/list"));
        let schema = tool.input_schema.as_ref();
        assert_eq!(
            schema.get("type").and_then(serde_json::Value::as_str),
            Some("object"),
            "`{name}` root schema must be an object"
        );
        assert_eq!(
            schema.get("required"),
            Some(&serde_json::json!(["action"])),
            "`{name}` root required must be exactly [\"action\"]"
        );
        let properties = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .unwrap_or_else(|| panic!("`{name}` schema must declare properties"));

        // `action` is a string enum listing every action of the tool,
        // in table order.
        let action = &properties["action"];
        assert_eq!(
            action.get("type").and_then(serde_json::Value::as_str),
            Some("string"),
            "`{name}` action must be a string"
        );
        let enum_names: Vec<&str> = action
            .get("enum")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect()
            })
            .unwrap_or_default();
        let table_names: Vec<&str> = spec.actions.iter().map(|action| action.name).collect();
        assert_eq!(
            enum_names, table_names,
            "`{name}` action enum must list every action of its table"
        );

        // Every parameter any action declares appears in `properties`
        // with a non-empty declared type — a future action shipping an
        // undeclared parameter fails here.
        for action_spec in spec.actions {
            for param in action_spec.required.iter().chain(action_spec.optional) {
                let shape = properties.get(*param).unwrap_or_else(|| {
                    panic!(
                        "`{name}` action `{}` does not declare parameter `{param}` in properties",
                        action_spec.name
                    )
                });
                assert!(
                    !declared_type_names(shape).is_empty(),
                    "`{name}` parameter `{param}` must declare a non-empty type: {shape}"
                );
            }
        }
    }
}

// ── FR5: typed calls through the resource-tool dispatch ────────────

#[tokio::test]
async fn typed_spec_list_with_integer_limit_succeeds() {
    let (_dir, _db, handler) = queue_test_handler();
    let result = call_resource_action(&handler, "spec", "list", serde_json::json!({ "limit": 5 }))
        .await
        .expect("a typed integer limit must deserialize");
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
}

#[tokio::test]
async fn typed_graph_node_update_with_object_config_succeeds() {
    let (_dir, db, handler) = queue_test_handler();
    insert_test_graph(&db, "graph-1");
    insert_standalone_spec(&db, "spec-1");
    insert_test_node(&db, "node-1", "spec-1");
    let result = call_graph_action(
        &handler,
        "graph_node",
        "update",
        serde_json::json!({ "node_id": "node-1", "config": { "command": "echo hi" } }),
    )
    .await
    .expect("a typed object config must deserialize");
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
}

#[tokio::test]
async fn typed_graph_edit_update_with_object_hooks_succeeds() {
    let (_dir, db, handler) = queue_test_handler();
    insert_test_graph(&db, "graph-1");
    let result = call_graph_action(
        &handler,
        "graph_edit",
        "update",
        serde_json::json!({
            "graph_id": "graph-1",
            "hooks": { "on_completed": [{ "command": "echo done" }] },
        }),
    )
    .await
    .expect("a typed object hooks map must deserialize");
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
}

// ── FR3: stringified clients are coerced, not refused ───────────────

#[tokio::test]
async fn stringified_integer_limit_is_coerced_to_integer() {
    let (_dir, _db, handler) = queue_test_handler();
    let result = call_resource_action_value(
        &handler,
        "spec",
        "list",
        serde_json::json!({ "limit": "5" }),
    )
    .await
    .expect("the string \"5\" must be coerced to the integer 5");
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
}

#[tokio::test]
async fn stringified_object_config_is_coerced_to_object() {
    let (_dir, db, handler) = queue_test_handler();
    insert_test_graph(&db, "graph-1");
    insert_standalone_spec(&db, "spec-1");
    insert_test_node(&db, "node-1", "spec-1");
    let result = call_graph_action_value(
        &handler,
        "graph_node",
        "update",
        serde_json::json!({
            "node_id": "node-1",
            "config": "{\"command\":\"echo hi\"}",
        }),
    )
    .await
    .expect("the stringified config must be coerced to an object");
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
}

#[tokio::test]
async fn unparseable_stringified_integer_is_refused_as_invalid_params() {
    let (_dir, _db, handler) = queue_test_handler();
    let err = call_resource_action_value(
        &handler,
        "spec",
        "list",
        serde_json::json!({ "limit": "abc" }),
    )
    .await
    .expect_err("a non-JSON string for an integer parameter must be refused");
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS, "{err}");
}

// ── `remote` revoke/disconnect actions ──────────────────────────

/// Call the `remote` tool with one action object.
async fn call_remote(
    handler: &TaskTriggerHandler,
    action: &str,
    device_id: &str,
) -> rmcp::model::CallToolResult {
    use crate::daemon::actions::ActionObject;
    use crate::daemon::handler_remote::RemoteObject;
    use crate::daemon::params_extract::Parameters;

    let object = serde_json::json!({"action": action, "device_id": device_id})
        .as_object()
        .expect("object params")
        .clone();
    handler
        .remote(Parameters(RemoteObject(ActionObject(object))))
        .await
        .expect("the tool call succeeds")
}

fn pair_remote_device(db: &Database, label: &str) -> String {
    crate::remote::devices::RemoteDevices::new(db.clone())
        .pair(
            format!("test spki for {label}").into_bytes(),
            label,
            "octocat",
            "2026-01-01T00:00:00Z",
        )
        .expect("pair the device")
        .id
}

#[tokio::test]
async fn remote_revoke_deletes_the_row_and_answers_revoked() {
    let (_dir, db, handler) = queue_test_handler();
    let id = pair_remote_device(&db, "laptop");

    let result = call_remote(&handler, "revoke", &id).await;
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
    let body: serde_json::Value =
        serde_json::from_str(&queue_raw_text(&result)).expect("json body");
    assert_eq!(body, serde_json::json!({ "revoked": id }));
    assert!(
        crate::remote::devices::RemoteDevices::new(db.as_ref().clone())
            .list()
            .expect("list")
            .is_empty(),
        "the row is gone"
    );

    let result = call_remote(&handler, "revoke", "missingdevice0000").await;
    assert!(result.is_error.unwrap_or(false), "unknown id errors");
    assert!(
        queue_raw_text(&result).contains("no paired device"),
        "the error names the problem: {}",
        queue_raw_text(&result)
    );
}

#[tokio::test]
async fn remote_disconnect_keeps_the_row_and_answers_disconnected() {
    use crate::application::ports::StateRepository as _;

    let (_dir, db, handler) = queue_test_handler();
    let id = pair_remote_device(&db, "phone");

    let result = call_remote(&handler, "disconnect", &id).await;
    assert!(!result.is_error.unwrap_or(false), "{:?}", result.content);
    let body: serde_json::Value =
        serde_json::from_str(&queue_raw_text(&result)).expect("json body");
    assert_eq!(body, serde_json::json!({ "disconnected": id }));

    let devices = crate::remote::devices::RemoteDevices::new(db.as_ref().clone());
    assert_eq!(devices.list().expect("list").len(), 1, "the row stays");
    let queue = db
        .get_state(crate::remote::relay::REMOTE_DISCONNECT_KEY)
        .expect("queue state");
    assert_eq!(queue.as_deref(), Some(id.as_str()));

    let result = call_remote(&handler, "disconnect", "missingdevice0000").await;
    assert!(result.is_error.unwrap_or(false), "unknown id errors");
    assert!(
        queue_raw_text(&result).contains("no paired device"),
        "the error names the problem: {}",
        queue_raw_text(&result)
    );
}

// ── run workdir: continue + get/list ───────────────────────────────

fn insert_paused_graph(db: &Database, id: &str, workdir: &str) {
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: id.to_string(),
        name: id.to_string(),
        description: None,
        workdir: workdir.to_string(),
        status: GraphStatus::Paused,
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
    // The paused-mid-spec state: a spec left `running`, no active run.
    let mut spec = standalone_spec(&format!("{id}-spec"));
    spec.graph_id = Some(id.to_string());
    spec.status = GraphSpecStatus::Running;
    db.insert_graph_spec(&spec).unwrap();
}

/// `graph_control continue` with an explicit workdir replaces the run's
/// workdir; without one the run keeps the workdir it was launched with
/// (never the stored one); an invalid workdir is a tool error with no
/// resume. Asserts the synchronous DB state only — never the background
/// dispatch either accepted call spawns.
#[tokio::test]
async fn continue_workdir_replaces_and_is_kept_without_override() {
    use crate::daemon::params::GraphContinueParams;

    let (dir, db, handler) = queue_test_handler();
    let stored = dir.path().to_string_lossy().to_string();
    insert_paused_graph(&db, "graph-1", &stored);
    let replacement = tempdir().unwrap();
    let replacement_path = replacement.path().to_string_lossy().to_string();

    let continued = handler
        .graph_continue_impl(GraphContinueParams {
            graph_id: "graph-1".to_string(),
            mode: "retry_current_node".to_string(),
            node_id: None,
            spec_id: None,
            workdir: Some(replacement_path.clone()),
        })
        .await
        .unwrap();
    assert!(
        !continued.is_error.unwrap_or(false),
        "{:?}",
        continued.content
    );
    assert_eq!(
        db.graph_active_run_workdir("graph-1").unwrap().as_deref(),
        Some(replacement_path.as_str()),
        "an explicit workdir replaces the run's workdir"
    );

    // Without an override the run keeps the workdir it was launched with —
    // not the graph's stored workdir. The call's own result is background
    // dependent (the accepted call above already spawned a dispatch); the
    // persisted run workdir is not — every writer agrees on it.
    db.update_graph_status("graph-1", GraphStatus::Paused, None, None)
        .unwrap();
    let _ = handler
        .graph_continue_impl(GraphContinueParams {
            graph_id: "graph-1".to_string(),
            mode: "retry_current_node".to_string(),
            node_id: None,
            spec_id: None,
            workdir: None,
        })
        .await;
    assert_eq!(
        db.graph_active_run_workdir("graph-1").unwrap().as_deref(),
        Some(replacement_path.as_str()),
        "without an override the run workdir is kept, never reset to the stored one"
    );

    // An invalid workdir is a tool error: nothing is persisted and the
    // graph is not resumed.
    insert_paused_graph(&db, "graph-2", &stored);
    let invalid = handler
        .graph_continue_impl(GraphContinueParams {
            graph_id: "graph-2".to_string(),
            mode: "retry_current_node".to_string(),
            node_id: None,
            spec_id: None,
            workdir: Some("relative/path".to_string()),
        })
        .await
        .unwrap();
    assert!(
        invalid.is_error.unwrap_or(false),
        "an invalid workdir must be a tool error: {:?}",
        invalid.content
    );
    assert!(
        queue_raw_text(&invalid).contains("absolute path"),
        "the error names the problem: {}",
        queue_raw_text(&invalid)
    );
    assert_eq!(
        db.graph_active_run_workdir("graph-2").unwrap(),
        None,
        "an invalid workdir persists nothing"
    );
    assert_eq!(
        db.get_graph("graph-2").unwrap().unwrap().status,
        GraphStatus::Paused,
        "an invalid workdir resumes nothing"
    );
}

/// The `graph_control` action table must declare `workdir` for `continue`:
/// without it a real MCP call carrying the documented parameter is refused
/// as undeclared before the handler ever runs. Direct `graph_continue_impl`
/// tests bypass the table, so they cannot catch the drift — this one pins
/// the dispatch layer itself.
#[test]
fn graph_control_continue_declares_workdir() {
    let table = &crate::daemon::handler_graph_tools::GRAPH_CONTROL;
    let spec = table.action("continue").expect("continue action declared");
    assert!(
        spec.optional.contains(&"workdir"),
        "continue must declare optional workdir"
    );
    let params = serde_json::json!({
        "action": "continue",
        "graph_id": "graph-1",
        "mode": "retry_current_node",
        "workdir": "/tmp",
    });
    assert!(
        table.validate(&params).is_ok(),
        "continue carrying workdir must validate"
    );
}

/// A refused `continue` persists nothing: neither a bad mode nor a
/// run_from_node refusal (unknown node) may replace the run's workdir —
/// otherwise the next continue without a workdir would silently resume at
/// the refused call's checkout.
#[tokio::test]
async fn refused_continue_persists_no_workdir() {
    use crate::daemon::params::GraphContinueParams;

    let (dir, db, handler) = queue_test_handler();
    let stored = dir.path().to_string_lossy().to_string();
    let candidate = tempdir().unwrap();
    let candidate_path = candidate.path().to_string_lossy().to_string();

    insert_paused_graph(&db, "graph-bad-mode", &stored);
    let bad_mode = handler
        .graph_continue_impl(GraphContinueParams {
            graph_id: "graph-bad-mode".to_string(),
            mode: "sideways".to_string(),
            node_id: None,
            spec_id: None,
            workdir: Some(candidate_path.clone()),
        })
        .await
        .unwrap();
    assert!(
        bad_mode.is_error.unwrap_or(false),
        "a bad mode must be a tool error: {:?}",
        bad_mode.content
    );
    assert_eq!(
        db.graph_active_run_workdir("graph-bad-mode").unwrap(),
        None,
        "a bad mode must persist no workdir"
    );

    insert_paused_graph(&db, "graph-bad-node", &stored);
    let bad_node = handler
        .graph_continue_impl(GraphContinueParams {
            graph_id: "graph-bad-node".to_string(),
            mode: "run_from_node".to_string(),
            node_id: Some("Nope".to_string()),
            spec_id: None,
            workdir: Some(candidate_path.clone()),
        })
        .await
        .unwrap();
    assert!(
        bad_node.is_error.unwrap_or(false),
        "an unknown node must be a tool error: {:?}",
        bad_node.content
    );
    assert_eq!(
        db.graph_active_run_workdir("graph-bad-node").unwrap(),
        None,
        "a refused run_from_node must persist no workdir"
    );
}

/// `graph get` and `graph list` show the active run's workdir next to the
/// stored one — but only when they differ.
#[test]
fn get_list_show_run_workdir_only_when_it_differs() {
    let (dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-1");
    // `insert_test_graph` stores `/tmp` as the graph's workdir.
    let run_workdir_of = |db: &Database| {
        let lp = db.get_graph("graph-1").unwrap().unwrap();
        let summary = build_graph_summary_json(db, &lp).unwrap();
        let details = db.get_graph_details("graph-1").unwrap().unwrap();
        let details_json = graph_details_json(db, &details).unwrap();
        (
            summary.get("run_workdir").cloned(),
            details_json.get("run_workdir").cloned(),
        )
    };

    // No persisted run workdir: absent on both surfaces.
    assert_eq!(run_workdir_of(&db), (None, None));

    // Persisted but equal to the stored workdir: still absent.
    db.set_graph_active_run_workdir("graph-1", Some("/tmp"))
        .unwrap();
    assert_eq!(run_workdir_of(&db), (None, None));

    // Persisted and different: present on both surfaces.
    let other = dir.path().to_string_lossy().to_string();
    db.set_graph_active_run_workdir("graph-1", Some(&other))
        .unwrap();
    assert_eq!(
        run_workdir_of(&db),
        (
            Some(serde_json::json!(other)),
            Some(serde_json::json!(other))
        )
    );
}
