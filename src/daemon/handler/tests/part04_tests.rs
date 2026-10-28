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

// ── blueprint_json ──────────────────────────────────────────────

#[test]
fn blueprint_json_includes_all_fields() {
    let bp = Blueprint {
        id: "bp-1".to_string(),
        name: "my-blueprint".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        builtin: true,
        created_at: chrono::Utc::now(),
    };
    let json = super::blueprint_json(&bp);
    assert_eq!(json["id"], "bp-1");
    assert_eq!(json["name"], "my-blueprint");
    assert_eq!(json["kind"], "agent");
    assert_eq!(json["builtin"], true);
    assert_eq!(json["config"]["platform"], "claude");
}

#[test]
fn blueprint_json_non_builtin() {
    let bp = Blueprint {
        id: "bp-2".to_string(),
        name: "custom".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        builtin: false,
        created_at: chrono::Utc::now(),
    };
    let json = super::blueprint_json(&bp);
    assert_eq!(json["builtin"], false);
    assert_eq!(json["kind"], "check");
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
