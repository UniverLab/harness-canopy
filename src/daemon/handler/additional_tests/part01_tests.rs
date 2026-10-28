//! Part 01 of `additional_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

// ── validate_position_conflict ────────────────────────────────

#[test]
fn validate_position_conflict_no_graph_id_returns_ok() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    assert!(validate_position_conflict(&db, None, "spec-x", 1).is_ok());
}

#[test]
fn validate_position_conflict_no_conflict() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    let mut spec = standalone_spec("spec-a");
    spec.graph_id = Some("graph-1".to_string());
    db.insert_graph_spec(&spec).unwrap();
    // spec-a is at position 0, checking position 1 — no conflict.
    assert!(validate_position_conflict(&db, Some("graph-1"), "spec-x", 1).is_ok());
}

#[test]
fn validate_position_conflict_detects_conflict() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    let mut spec = standalone_spec("spec-a");
    spec.graph_id = Some("graph-1".to_string());
    db.insert_graph_spec(&spec).unwrap();
    let error = validate_position_conflict(&db, Some("graph-1"), "spec-x", 0).unwrap_err();
    assert!(error.contains("position 0"), "{error}");
    assert!(error.contains("graph-1"), "{error}");
}

#[test]
fn validate_position_conflict_excludes_self() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    let mut spec = standalone_spec("spec-a");
    spec.graph_id = Some("graph-1".to_string());
    db.insert_graph_spec(&spec).unwrap();
    // spec-a at position 0, checking spec-a itself at position 0 — no conflict.
    assert!(validate_position_conflict(&db, Some("graph-1"), "spec-a", 0).is_ok());
}

// ── validate_node_position_conflict ───────────────────────────

#[test]
fn validate_node_position_conflict_no_owner_returns_ok() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let node = GraphNode {
        id: "n1".to_string(),
        spec_id: None,
        graph_id: None,
        name: "n1".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    assert!(validate_node_position_conflict(&db, &node, "n1", 1).is_ok());
}

#[test]
fn validate_node_position_conflict_spec_owner() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-1")).unwrap();
    let now = chrono::Utc::now();
    db.insert_graph_node(&GraphNode {
        id: "n1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "n1".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: now,
    })
    .unwrap();
    let error = validate_node_position_conflict(
        &db,
        &GraphNode {
            id: "n1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "n1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 5,
            created_at: now,
        },
        "other-node",
        1,
    )
    .unwrap_err();
    assert!(error.contains("Spec 'spec-1'"), "{error}");
    assert!(error.contains("position 1"), "{error}");
}

#[test]
fn validate_node_position_conflict_graph_owner() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    let now = chrono::Utc::now();
    db.insert_graph_node(&GraphNode {
        id: "n1".to_string(),
        spec_id: None,
        graph_id: Some("graph-1".to_string()),
        name: "n1".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: now,
    })
    .unwrap();
    let error = validate_node_position_conflict(
        &db,
        &GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            name: "n1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 5,
            created_at: now,
        },
        "other-node",
        1,
    )
    .unwrap_err();
    assert!(error.contains("Graph 'graph-1'"), "{error}");
}

// ── validate_spec_workdir edge cases ──────────────────────────

#[test]
fn validate_spec_workdir_whitespace_is_absolute() {
    // Whitespace-only relative path is still relative.
    assert!(validate_spec_workdir("  relative  ").is_err());
}

#[test]
fn validate_spec_workdir_root_path() {
    assert!(validate_spec_workdir("/").is_ok());
}

// ── validate_not_join_kind edge cases ─────────────────────────

#[test]
fn validate_not_join_kind_join_returns_error_with_actionable_message() {
    let err = validate_not_join_kind(GraphNodeKind::Join).unwrap_err();
    assert!(err.contains("engine-managed"), "{err}");
    assert!(err.contains("graph_add_ensemble"), "{err}");
}

// ── validate_edge_condition edge cases ────────────────────────

#[test]
fn validate_edge_condition_empty_string() {
    let err = validate_edge_condition("").unwrap_err();
    assert!(err.contains("pass"), "{err}");
}

#[test]
fn validate_edge_condition_case_sensitive() {
    // "Pass" (capital P) should not match — only lowercase.
    assert!(validate_edge_condition("Pass").is_err());
}

// ── validate_node_kind edge cases ─────────────────────────────

#[test]
fn validate_node_kind_empty_string() {
    assert!(validate_node_kind("").is_err());
}

#[test]
fn validate_node_kind_uppercase() {
    assert!(validate_node_kind("AGENT").is_err());
}

// ── validate_spec_status edge cases ───────────────────────────

#[test]
fn validate_spec_status_empty_string() {
    let err = validate_spec_status("").unwrap_err();
    assert!(err.contains("pending"), "{err}");
}

#[test]
fn validate_spec_set_status_target_empty_string() {
    assert!(validate_spec_set_status_target("").is_err());
}

// ── validate_spec_set_status_target edge cases ────────────────

#[test]
fn validate_spec_set_status_target_case_insensitive() {
    assert_eq!(
        validate_spec_set_status_target("COMPLETED").unwrap(),
        GraphSpecStatus::Completed
    );
    assert_eq!(
        validate_spec_set_status_target("  Skipped  ").unwrap(),
        GraphSpecStatus::Skipped
    );
}

// ── json_value_kind_name edge cases ───────────────────────────

#[test]
fn json_value_kind_name_nested_object() {
    let v = serde_json::json!({"a": {"b": 1}});
    assert_eq!(json_value_kind_name(&v), "an object");
}

#[test]
fn json_value_kind_name_empty_array() {
    let v = serde_json::json!([]);
    assert_eq!(json_value_kind_name(&v), "an array");
}

// ── validate_at_least_one_bool edge cases ─────────────────────

#[test]
fn validate_at_least_one_bool_single_true() {
    assert!(validate_at_least_one_bool(&[true], "f").is_ok());
}

#[test]
fn validate_at_least_one_bool_single_false() {
    assert!(validate_at_least_one_bool(&[false], "f").is_err());
}

#[test]
fn validate_at_least_one_bool_empty_slice() {
    // All false in an empty slice.
    assert!(validate_at_least_one_bool(&[], "f").is_err());
}

// ── validate_queue_reorder edge cases ──────────────────────────

#[test]
fn validate_queue_reorder_empty_current_and_ids() {
    assert!(validate_queue_reorder(&[], &[]).is_ok());
}

#[test]
fn validate_queue_reorder_single_element() {
    let current = vec!["a".to_string()];
    let reordered = vec!["a".to_string()];
    assert!(validate_queue_reorder(&current, &reordered).is_ok());
}

// ── validate_non_empty edge cases ─────────────────────────────

#[test]
fn validate_non_empty_single_space() {
    assert!(validate_non_empty(" ", "field").is_err());
}

#[test]
fn validate_non_empty_tabs_and_newlines() {
    assert!(validate_non_empty("\t\n\r", "field").is_err());
}

// ── validate_absolute_dir edge cases ──────────────────────────

#[test]
fn validate_absolute_dir_root() {
    assert!(validate_absolute_dir("/").is_ok());
}

#[test]
fn validate_absolute_dir_home() {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    // Home dir should exist and be absolute.
    if std::path::Path::new(&home).is_dir() {
        assert!(validate_absolute_dir(&home).is_ok());
    }
}

// ── node_copy_note edge cases ─────────────────────────────────

#[test]
fn node_copy_note_wired_mentions_both_ids() {
    let note = node_copy_note("source-abc", "target-xyz", true);
    assert!(note.contains("source-abc"));
    assert!(note.contains("target-xyz"));
    assert!(!note.contains("Unwired"));
}

#[test]
fn node_copy_note_unwired_suggests_wiring_tool() {
    let note = node_copy_note("src", "dst", false);
    assert!(note.contains("graph_add_edge"));
    assert!(note.contains("entry_from_node") || note.contains("on_pass_to"));
}

// ── build_graph_trigger: watch with debounce_seconds and recursive ──

#[test]
fn build_graph_trigger_watch_defaults() {
    let params = GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: Some("/tmp".to_string()),
        events: Some(vec!["modify".to_string()]),
        debounce_seconds: None,
        recursive: None,
    };
    let trigger = build_graph_trigger(&Some(params)).unwrap().unwrap();
    match trigger {
        Trigger::Watch {
            debounce_seconds,
            recursive,
            ..
        } => {
            assert_eq!(debounce_seconds, 2, "default debounce should be 2");
            assert!(!recursive, "default recursive should be false");
        }
        other => panic!("expected Watch, got {other:?}"),
    }
}

// ── build_graph_completion_hook edge cases ─────────────────────

#[test]
fn build_graph_completion_hook_none_model_becomes_none() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: None,
        effort: None,
        prompt: Some("test".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert!(hook.model.is_none());
}

#[test]
fn build_graph_completion_hook_timeout_passthrough() {
    let params = GraphCompletionHookParams {
        platform: Some("mimo".to_string()),
        model: None,
        effort: None,
        prompt: Some("do stuff".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(45),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert_eq!(hook.timeout_minutes, Some(45));
}

// ── graph_run_status_guard: Draft and Paused are accepted ──────

#[test]
fn graph_run_status_guard_draft_accepted() {
    assert!(graph_run_status_guard("graph-d", GraphStatus::Draft).is_ok());
}

#[test]
fn graph_run_status_guard_paused_accepted() {
    assert!(graph_run_status_guard("graph-p", GraphStatus::Paused).is_ok());
}

// ── validate_node_config: Join kind ───────────────────────────

#[test]
fn validate_node_config_join_always_passes() {
    let config = serde_json::json!({});
    assert!(validate_node_config(GraphNodeKind::Join, &config).is_ok());
}

// ── build_graph_trigger: watch with empty events ───────────────

#[test]
fn build_graph_trigger_watch_empty_events_rejected() {
    let params = GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: Some("/tmp".to_string()),
        events: Some(vec![]),
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("event"), "{err}");
}

// ── validate_queue_reorder_locking: pending spec is movable ────

#[test]
fn validate_queue_reorder_locking_allows_moving_pending_members() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-c")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.append_queue_member("queue-1", id, None).unwrap();
    }
    let current = db.list_queue_member_spec_ids("queue-1").unwrap();

    // All pending — any permutation is allowed.
    let order = vec![
        "spec-c".to_string(),
        "spec-a".to_string(),
        "spec-b".to_string(),
    ];
    assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
}

// ── validate_queue_member_removable: non-running spec ──────────

#[test]
fn validate_queue_member_removable_pending_spec_ok() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    assert!(validate_queue_member_removable(&db, "queue-1", "spec-a").is_ok());
}

// ── resolve_reported_run: run not found for node_id ───────────

#[test]
fn resolve_reported_run_rejects_node_id_mismatch_even_if_running() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
    insert_test_node(&db, "node-1", "spec-a");
    db.insert_graph_run(&graph_run_row(
        "run-1",
        "graph-1",
        "spec-a",
        GraphRunStatus::Running,
    ))
    .unwrap();

    let result = resolve_reported_run(&db, Some("run-1"), Some("wrong-node"), None).unwrap();
    let err = result.expect_err("mismatched node_id must be rejected");
    assert!(err.is_error.unwrap_or(false));
}

// ── validate_queue_not_consumed: empty queue ────────────────────

#[test]
fn validate_queue_not_consumed_empty_queue() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-empty".to_string(),
        name: "empty".to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    assert!(validate_queue_not_consumed(&db, "queue-empty", "graph-1").is_ok());
}

// ── graph_run_status_guard: error messages name the graph ───────

#[test]
fn graph_run_status_guard_running_error_names_graph() {
    let err = graph_run_status_guard("my-graph", GraphStatus::Running).unwrap_err();
    assert!(err.contains("my-graph"), "{err}");
    assert!(err.contains("already running"), "{err}");
}

#[test]
fn graph_run_status_guard_completed_error_names_reset() {
    let err = graph_run_status_guard("l", GraphStatus::Completed).unwrap_err();
    assert!(err.contains("graph_reset"), "{err}");
}

// ── validate_spec_deletable: no graph_id ───────────────────────

#[test]
fn validate_spec_deletable_standalone_spec_ok() {
    let spec = GraphSpec {
        id: "spec-1".to_string(),
        graph_id: None,
        name: "test".to_string(),
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
    };
    assert!(validate_spec_deletable(&spec).is_ok());
}

// ── validate_ensemble_members: whitespace-only platform ───────

#[test]
fn validate_ensemble_members_rejects_whitespace_platform() {
    let members = vec![
        EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMemberParams {
            platform: "\t\n".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    let err = validate_ensemble_members(
        &members,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap_err();
    assert!(err.contains("platform"), "{err}");
}

// ── validate_ensemble_members: model trimming ─────────────────

#[test]
fn validate_ensemble_members_trims_model() {
    let members = vec![
        EnsembleMemberParams {
            platform: "claude".to_string(),
            model: Some("  opus-4  ".to_string()),
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMemberParams {
            platform: "mimo".to_string(),
            model: Some("   ".to_string()),
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    let result = validate_ensemble_members(
        &members,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap();
    assert_eq!(result[0].1.as_deref(), Some("opus-4"));
    assert_eq!(result[1].1, None); // whitespace-only model becomes None
}

// ── validate_position_conflict: no conflict with same position on different spec ──

#[test]
fn validate_position_conflict_same_position_different_graph() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    insert_test_graph(&db, "graph-2");
    let mut spec = standalone_spec("spec-a");
    spec.graph_id = Some("graph-1".to_string());
    db.insert_graph_spec(&spec).unwrap();
    // spec-a at position 0 in graph-1; check position 0 in graph-2 — no conflict.
    assert!(validate_position_conflict(&db, Some("graph-2"), "spec-x", 0).is_ok());
}

// ── build_graph_update_response ────────────────────────────────

#[test]
fn build_graph_update_response_text() {
    let result = build_graph_update_response("graph-xyz", None);
    let text = format!("{:?}", result.content);
    assert!(text.contains("graph-xyz"));
    assert!(text.contains("updated"));
}

// ── build_spec_update_response ────────────────────────────────

#[test]
fn build_spec_update_response_text() {
    let result = build_spec_update_response("spec-abc");
    let text = format!("{:?}", result.content);
    assert!(text.contains("spec-abc"));
    assert!(text.contains("updated"));
}

// ── blueprint_json ────────────────────────────────────────────

#[test]
fn blueprint_json_with_agent_kind() {
    let bp = Blueprint {
        id: "bp-a".to_string(),
        name: "agent-bp".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "mimo"}),
        builtin: false,
        created_at: chrono::Utc::now(),
    };
    let json = blueprint_json(&bp);
    assert_eq!(json["kind"], "agent");
    assert_eq!(json["builtin"], false);
}

// ── spec_summary_json edge cases ──────────────────────────────

#[test]
fn spec_summary_json_with_null_fields() {
    let spec = GraphSpec {
        id: "s1".to_string(),
        graph_id: None,
        name: "spec".to_string(),
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
    };
    let json = spec_summary_json(&spec, true);
    assert!(json["graph_id"].is_null());
    assert!(json["description"].is_null());
    assert!(json["workdir"].is_null());
}

// ── build_get_tools_response: all scopes return objects ────────

#[test]
fn build_get_tools_response_all_scopes_are_objects() {
    for scope in [
        "session_start",
        "file_write",
        "test_run",
        "close_session",
        "multi_agent",
    ] {
        let json = build_get_tools_response(scope);
        assert!(json.is_object(), "scope '{scope}' should be an object");
        assert!(json["scope"].as_str().is_some());
        assert!(json["protocol"].is_array());
        assert!(json["tools"].is_array());
    }
}

// ── rag_result_json edge cases ────────────────────────────────

#[test]
fn rag_result_json_with_none_distance() {
    let result = crate::rag::vector_store::SearchResult {
        id: "sr-2".to_string(),
        file_path: "/doc.md".to_string(),
        content: "text".to_string(),
        created_at: 0,
        distance: None,
    };
    let json = rag_result_json(&result);
    assert!(json["distance"].is_null());
}

// ── validate_node_config: gate with custom evaluate ───────────

#[test]
fn validate_node_config_gate_custom_evaluate_no_value_required() {
    let config = serde_json::json!({"evaluate": "exit_code_0"});
    assert!(validate_node_config(GraphNodeKind::Gate, &config).is_ok());
}

// ── validate_queue_reorder_locking: skipped spec is locked ─────

#[test]
fn validate_queue_reorder_locking_refuses_moving_skipped_spec() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut skipped = standalone_spec("spec-a");
    skipped.status = GraphSpecStatus::Skipped;
    db.insert_graph_spec(&skipped).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();
    let current = db.list_queue_member_spec_ids("queue-1").unwrap();

    let order = vec!["spec-b".to_string(), "spec-a".to_string()];
    let error = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
    assert!(error.contains("spec-a"), "{error}");
    assert!(error.contains("skipped"), "{error}");
}

// ── node_copy_note ────────────────────────────────────────────

#[test]
fn node_copy_note_wired() {
    let note = node_copy_note("src", "dst", true);
    assert!(note.contains("src"));
    assert!(note.contains("dst"));
    assert!(!note.contains("Unwired"));
}

#[test]
fn node_copy_note_unwired() {
    let note = node_copy_note("src", "dst", false);
    assert!(note.contains("Unwired"));
    assert!(note.contains("src"));
    assert!(note.contains("dst"));
    assert!(note.contains("NO incoming or outgoing edges"));
}

// ── json_value_kind_name ──────────────────────────────────────

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
    assert_eq!(json_value_kind_name(&serde_json::json!([1, 2])), "an array");
}

#[test]
fn json_value_kind_name_object() {
    assert_eq!(
        json_value_kind_name(&serde_json::json!({"a": 1})),
        "an object"
    );
}

// ── validate_node_config ──────────────────────────────────────

#[test]
fn validate_node_config_agent_needs_platform() {
    let config = serde_json::json!({"command": "test"});
    let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(err.contains("platform"), "{err}");
}

#[test]
fn validate_node_config_agent_with_platform() {
    let config = serde_json::json!({"platform": "claude"});
    assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
}

#[test]
fn validate_node_config_agent_with_cli() {
    let config = serde_json::json!({"cli": "opencode"});
    assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
}

#[test]
fn validate_node_config_check_needs_command() {
    let config = serde_json::json!({"platform": "claude"});
    let err = validate_node_config(GraphNodeKind::Check, &config).unwrap_err();
    assert!(err.contains("command"), "{err}");
}

#[test]
fn validate_node_config_check_with_command() {
    let config = serde_json::json!({"command": "cargo test"});
    assert!(validate_node_config(GraphNodeKind::Check, &config).is_ok());
}

#[test]
fn validate_node_config_gate_needs_value_when_output_contains() {
    let config = serde_json::json!({});
    let err = validate_node_config(GraphNodeKind::Gate, &config).unwrap_err();
    assert!(err.contains("value"), "{err}");
}

#[test]
fn validate_node_config_gate_with_value() {
    let config = serde_json::json!({"value": "success"});
    assert!(validate_node_config(GraphNodeKind::Gate, &config).is_ok());
}

#[test]
fn validate_node_config_not_object() {
    let config = serde_json::json!("not an object");
    let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(err.contains("JSON object"), "{err}");
}

#[test]
fn validate_node_config_join_always_ok() {
    let config = serde_json::json!({});
    assert!(validate_node_config(GraphNodeKind::Join, &config).is_ok());
}

#[test]
fn validate_node_config_agent_empty_platform() {
    let config = serde_json::json!({"platform": "  "});
    let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(err.contains("platform"), "{err}");
}

// ── validate_edge_condition ───────────────────────────────────

#[test]
fn validate_edge_condition_pass() {
    assert!(matches!(
        validate_edge_condition("pass").unwrap(),
        GraphEdgeCondition::Pass
    ));
}

#[test]
fn validate_edge_condition_fail() {
    assert!(matches!(
        validate_edge_condition("fail").unwrap(),
        GraphEdgeCondition::Fail
    ));
}

#[test]
fn validate_edge_condition_always() {
    assert!(matches!(
        validate_edge_condition("always").unwrap(),
        GraphEdgeCondition::Always
    ));
}

#[test]
fn validate_edge_condition_invalid() {
    assert!(validate_edge_condition("sometimes").is_err());
}

#[test]
fn validate_edge_condition_with_whitespace() {
    assert!(matches!(
        validate_edge_condition("  pass  ").unwrap(),
        GraphEdgeCondition::Pass
    ));
}

// ── validate_node_kind ────────────────────────────────────────

#[test]
fn validate_node_kind_agent() {
    assert!(matches!(
        validate_node_kind("agent").unwrap(),
        GraphNodeKind::Agent
    ));
}

#[test]
fn validate_node_kind_check() {
    assert!(matches!(
        validate_node_kind("check").unwrap(),
        GraphNodeKind::Check
    ));
}

#[test]
fn validate_node_kind_gate() {
    assert!(matches!(
        validate_node_kind("gate").unwrap(),
        GraphNodeKind::Gate
    ));
}

#[test]
fn validate_node_kind_invalid() {
    assert!(validate_node_kind("invalid").is_err());
}

#[test]
fn validate_node_kind_with_whitespace() {
    assert!(matches!(
        validate_node_kind("  agent  ").unwrap(),
        GraphNodeKind::Agent
    ));
}

// ── validate_not_join_kind ────────────────────────────────────

#[test]
fn validate_not_join_kind_rejects_join() {
    assert!(validate_not_join_kind(GraphNodeKind::Join).is_err());
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

// ── validate_spec_status ──────────────────────────────────────

#[test]
fn validate_spec_status_all_valid() {
    assert!(matches!(
        validate_spec_status("pending").unwrap(),
        GraphSpecStatus::Pending
    ));
    assert!(matches!(
        validate_spec_status("running").unwrap(),
        GraphSpecStatus::Running
    ));
    assert!(matches!(
        validate_spec_status("completed").unwrap(),
        GraphSpecStatus::Completed
    ));
    assert!(matches!(
        validate_spec_status("failed").unwrap(),
        GraphSpecStatus::Failed
    ));
    assert!(matches!(
        validate_spec_status("skipped").unwrap(),
        GraphSpecStatus::Skipped
    ));
    assert!(matches!(
        validate_spec_status("interrupted").unwrap(),
        GraphSpecStatus::Interrupted
    ));
}

#[test]
fn validate_spec_status_case_insensitive() {
    assert!(validate_spec_status("PENDING").is_ok());
    assert!(validate_spec_status("Running").is_ok());
}

#[test]
fn validate_spec_status_with_whitespace() {
    assert!(validate_spec_status("  pending  ").is_ok());
}

#[test]
fn validate_spec_status_invalid() {
    assert!(validate_spec_status("unknown").is_err());
}

// ── validate_spec_set_status_target ───────────────────────────

#[test]
fn validate_spec_set_status_target_valid() {
    assert!(validate_spec_set_status_target("pending").is_ok());
    assert!(validate_spec_set_status_target("completed").is_ok());
    assert!(validate_spec_set_status_target("skipped").is_ok());
}

#[test]
fn validate_spec_set_status_target_running_invalid() {
    assert!(validate_spec_set_status_target("running").is_err());
}

#[test]
fn validate_spec_set_status_target_failed_invalid() {
    assert!(validate_spec_set_status_target("failed").is_err());
}

// ── validate_at_least_one_bool ────────────────────────────────

#[test]
fn validate_at_least_one_bool_all_false() {
    assert!(validate_at_least_one_bool(&[false, false, false], "test").is_err());
}

#[test]
fn validate_at_least_one_bool_one_true() {
    assert!(validate_at_least_one_bool(&[false, true, false], "test").is_ok());
}

#[test]
fn validate_at_least_one_bool_all_true() {
    assert!(validate_at_least_one_bool(&[true, true], "test").is_ok());
}

#[test]
fn validate_at_least_one_bool_empty() {
    assert!(validate_at_least_one_bool(&[], "test").is_err());
}

// ── missing_sync_identity_error ───────────────────────────────

#[test]
fn missing_sync_identity_error_includes_canopy_identity() {
    let err = missing_sync_identity_error();
    assert!(err.message.contains("Canopy session identity"));
}

// ── validate_spec_workdir ─────────────────────────────────────

#[test]
fn validate_spec_workdir_absolute() {
    assert!(validate_spec_workdir("/tmp").is_ok());
}

#[test]
fn validate_spec_workdir_relative() {
    assert!(validate_spec_workdir("relative/path").is_err());
}

// ── validate_non_empty ────────────────────────────────────────

#[test]
fn validate_non_empty_valid() {
    assert!(validate_non_empty("hello", "field").is_ok());
}

#[test]
fn validate_non_empty_empty() {
    assert!(validate_non_empty("", "field").is_err());
}

#[test]
fn validate_non_empty_whitespace() {
    assert!(validate_non_empty("   ", "field").is_err());
}

#[test]
fn validate_non_empty_error_message() {
    let err = validate_non_empty("", "my_field").unwrap_err();
    assert!(err.contains("my_field"));
}

// ── spec_summary_json ─────────────────────────────────────────

#[test]
fn spec_summary_json_basic() {
    let spec = standalone_spec("spec-1");
    let json = spec_summary_json(&spec, true);
    assert_eq!(json["id"], "spec-1");
    assert_eq!(json["name"], "spec-1");
    assert_eq!(json["status"], "pending");
    assert_eq!(json["parallelizable"], false);
}

#[test]
fn spec_summary_json_with_completed_via() {
    let mut spec = standalone_spec("spec-1");
    spec.completed_via = Some("test".to_string());
    let json = spec_summary_json(&spec, true);
    assert_eq!(json["completed_via"], "test");
}

#[test]
fn spec_summary_json_with_workdir() {
    let mut spec = standalone_spec("spec-1");
    spec.workdir = Some("/tmp/project".to_string());
    let json = spec_summary_json(&spec, true);
    assert_eq!(json["workdir"], "/tmp/project");
}

// ── build_get_tools_response ──────────────────────────────────

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

// ── graph_run_status_guard ─────────────────────────────────────

#[test]
fn graph_run_status_guard_running() {
    let err = graph_run_status_guard("graph-1", GraphStatus::Running).unwrap_err();
    assert!(err.contains("already running"));
}

#[test]
fn graph_run_status_guard_completed() {
    let err = graph_run_status_guard("graph-1", GraphStatus::Completed).unwrap_err();
    assert!(err.contains("cannot be resumed directly"), "{err}");
}

#[test]
fn graph_run_status_guard_failed() {
    let err = graph_run_status_guard("graph-1", GraphStatus::Failed).unwrap_err();
    assert!(err.contains("cannot be resumed directly"), "{err}");
}

#[test]
fn graph_run_status_guard_draft() {
    assert!(graph_run_status_guard("graph-1", GraphStatus::Draft).is_ok());
}

// ── graph_trigger_json ─────────────────────────────────────────

#[test]
fn graph_trigger_json_manual() {
    let lp = Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: "l1".to_string(),
        name: "l1".to_string(),
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
    };
    let json = graph_trigger_json(&lp);
    assert_eq!(json["type"], "manual");
    assert!(json.get("schedule").is_none());
}

#[test]
fn graph_trigger_json_cron() {
    let lp = Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: "l1".to_string(),
        name: "l1".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Draft,
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    };
    let json = graph_trigger_json(&lp);
    assert_eq!(json["type"], "cron");
    assert_eq!(json["schedule"], "0 9 * * *");
}

// ── validate_queue_reorder ─────────────────────────────────────

#[test]
fn validate_queue_reorder_wrong_count() {
    let current = vec!["a".to_string(), "b".to_string()];
    let spec_ids = vec!["a".to_string()];
    assert!(validate_queue_reorder(&current, &spec_ids).is_err());
}

#[test]
fn validate_queue_reorder_duplicate() {
    let current = vec!["a".to_string(), "b".to_string()];
    let spec_ids = vec!["a".to_string(), "a".to_string()];
    assert!(validate_queue_reorder(&current, &spec_ids).is_err());
}

#[test]
fn validate_queue_reorder_unknown_spec() {
    let current = vec!["a".to_string(), "b".to_string()];
    let spec_ids = vec!["a".to_string(), "c".to_string()];
    assert!(validate_queue_reorder(&current, &spec_ids).is_err());
}

#[test]
fn validate_queue_reorder_valid() {
    let current = vec!["a".to_string(), "b".to_string()];
    let spec_ids = vec!["b".to_string(), "a".to_string()];
    assert!(validate_queue_reorder(&current, &spec_ids).is_ok());
}

// ── blueprint_json ────────────────────────────────────────────

#[test]
fn blueprint_json_basic() {
    let bp = Blueprint {
        id: "bp-1".to_string(),
        name: "ensemble-proposers".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"members": []}),
        builtin: true,
        created_at: chrono::Utc::now(),
    };
    let json = blueprint_json(&bp);
    assert_eq!(json["id"], "bp-1");
    assert_eq!(json["name"], "ensemble-proposers");
    assert_eq!(json["builtin"], true);
}

// ── validate_absolute_dir ─────────────────────────────────────

#[test]
fn validate_absolute_dir_valid() {
    assert!(validate_absolute_dir("/tmp").is_ok());
}

#[test]
fn validate_absolute_dir_relative() {
    assert!(validate_absolute_dir("relative").is_err());
}
