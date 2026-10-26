//! Part 03 of `tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// Omitting `at` must cancel a pending auto-continue schedule, mirroring
/// `graph_schedule_autorun`'s cancel semantics (B41).
#[tokio::test]
async fn graph_schedule_continue_cancels_pending_schedule() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-paused-continue-cancel";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Paused,
    ))
    .unwrap();
    db.schedule_graph_auto_continue(
        graph_id,
        chrono::Utc::now() + chrono::Duration::hours(1),
        Some("retry_current_node"),
    )
    .unwrap();

    let result = handler
        .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
            graph_id: graph_id.to_string(),
            at: None,
            action: None,
        }))
        .await
        .unwrap();

    let text = result_text(&result);
    assert!(text.contains("cancelled"), "{text}");
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert!(
        lp.auto_continue_at.is_none(),
        "cancel must clear auto_continue_at"
    );
    assert_eq!(
        lp.status,
        GraphStatus::Paused,
        "cancelling must not touch graph status"
    );
}

/// Cancelling when nothing is scheduled must succeed and say so.
#[tokio::test]
async fn graph_schedule_continue_cancel_with_nothing_scheduled_is_not_an_error() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-paused-no-continue";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Paused,
    ))
    .unwrap();

    let result = handler
        .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
            graph_id: graph_id.to_string(),
            at: None,
            action: None,
        }))
        .await
        .unwrap();

    assert!(!result.is_error.unwrap_or(false), "{result:?}");
    let text = result_text(&result);
    assert!(text.contains("no pending auto-continue"), "{text}");
}

/// Scheduling `graph_schedule_continue` must never touch `autorun_at`,
/// and vice versa — the two schedules must stay fully independent so
/// they can never be conflated at fire time.
#[tokio::test]
async fn graph_schedule_continue_and_graph_schedule_autorun_are_independent() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-independent-schedules";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Paused,
    ))
    .unwrap();

    handler
        .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
            graph_id: graph_id.to_string(),
            at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            action: None,
        }))
        .await
        .unwrap();

    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert!(lp.auto_continue_at.is_some());
    assert!(
        lp.autorun_at.is_none(),
        "graph_schedule_continue must not set autorun_at"
    );
}

#[test]
fn graph_copy_node_applies_overrides_and_wiring() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_node(&u10_agent(
            "impl",
            None,
            Some("graph-1"),
            1,
            serde_json::json!({"platform":"claude","prompt_template":"implement it","timeout_minutes":30}),
        ))
        .unwrap();
    db.insert_graph_node(&u10_check("gate", None, Some("graph-1"), 2))
        .unwrap();

    let params: GraphCopyNodeParams = serde_json::from_value(serde_json::json!({
        "source_node_id": "impl",
        "graph_id": "graph-1",
        "name": "review",
        "config_overrides": {"prompt_template": "review it", "platform": "codex"},
        "on_pass_to": "gate"
    }))
    .unwrap();
    let plan = plan_node_copy(&db, &params).unwrap();

    assert_ne!(plan.node.id, "impl", "the copy must have a fresh id");
    assert_eq!(plan.node.name, "review");
    assert_eq!(plan.node.kind, GraphNodeKind::Agent);
    // Overridden keys win; untouched source keys are preserved.
    assert_eq!(plan.node.config["prompt_template"], "review it");
    assert_eq!(plan.node.config["platform"], "codex");
    assert_eq!(plan.node.config["timeout_minutes"], 30);
    assert_eq!(plan.node.graph_id.as_deref(), Some("graph-1"));
    assert!(plan.node.spec_id.is_none());
    // One outgoing pass edge to the wiring target.
    assert_eq!(plan.edges.len(), 1);
    assert_eq!(plan.edges[0].from_node, plan.node.id);
    assert_eq!(plan.edges[0].to_node, "gate");
    assert_eq!(plan.edges[0].condition.as_str(), "pass");
    assert_eq!(plan.wiring.get("on_pass_to").unwrap(), "gate");
}

#[test]
fn graph_copy_node_unwired_copy_is_valid_and_reported() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_node(&u10_agent(
        "impl",
        None,
        Some("graph-1"),
        1,
        serde_json::json!({"platform":"claude"}),
    ))
    .unwrap();

    // No wiring, no target override → copy into the source's own graph.
    let params: GraphCopyNodeParams =
        serde_json::from_value(serde_json::json!({"source_node_id": "impl"})).unwrap();
    let plan = plan_node_copy(&db, &params).unwrap();

    assert!(plan.edges.is_empty(), "an unwired copy creates no edges");
    assert!(plan.wiring.is_empty());
    assert_eq!(plan.node.graph_id.as_deref(), Some("graph-1"));
    assert_eq!(plan.node.config["platform"], "claude");
    assert_ne!(plan.node.id, "impl");

    // The response note must flag the unwired state explicitly.
    let note = node_copy_note(&plan.source_id, &plan.node.id, false);
    assert!(note.contains("Unwired copy"), "{note}");
    assert!(note.contains("graph_add_edge"), "{note}");
}

#[test]
fn graph_copy_node_rejects_ensemble_owned_source() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_node(&u10_check("kickoff", None, Some("graph-1"), 1))
        .unwrap();
    db.insert_graph_node(&u10_agent(
        "arbiter",
        None,
        Some("graph-1"),
        2,
        serde_json::json!({"platform":"claude"}),
    ))
    .unwrap();
    let src = u10_source_ensemble(&db, None, Some("graph-1"), "kickoff", "arbiter");

    // A member node can't be copied directly — must go through the ensemble.
    let params: GraphCopyNodeParams = serde_json::from_value(serde_json::json!({
        "source_node_id": src.members[0].node_id
    }))
    .unwrap();
    let err = plan_node_copy(&db, &params).unwrap_err();
    assert!(err.contains("graph_copy_ensemble"), "{err}");
}

/// CM2: copying a node into a graph with `infra_node_id` set must
/// auto-wire a `Error` edge to the infra node, preserving the
/// pre-wiring invariant.
#[test]
fn graph_copy_node_auto_wires_error_edge_when_graph_has_infra_node() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: Some("infra".to_string()),
        id: "graph-1".to_string(),
        name: "graph-1".to_string(),
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
    db.insert_graph_node(&u10_agent(
        "src",
        None,
        Some("graph-1"),
        1,
        serde_json::json!({"platform":"claude","prompt_template":"do it"}),
    ))
    .unwrap();
    db.insert_graph_node(&u10_check("infra", None, Some("graph-1"), 2))
        .unwrap();

    let params: GraphCopyNodeParams = serde_json::from_value(serde_json::json!({
        "source_node_id": "src"
    }))
    .unwrap();
    let plan = plan_node_copy(&db, &params).unwrap();
    db.insert_node_with_edges(&plan.node, &plan.edges).unwrap();

    // Simulate the handler's auto-wiring block.
    if let Some(ref lid) = plan.node.graph_id {
        if let Ok(Some(lp)) = db.get_graph(lid) {
            if let Some(ref infra_node_id) = lp.infra_node_id {
                let already_has_error = plan.edges.iter().any(|e| {
                    e.from_node == plan.node.id && e.condition == GraphEdgeCondition::Error
                });
                if !already_has_error {
                    let edge = GraphEdge {
                        id: uuid::Uuid::new_v4().to_string(),
                        spec_id: plan.node.spec_id.clone(),
                        graph_id: Some(lid.clone()),
                        from_node: plan.node.id.clone(),
                        to_node: infra_node_id.clone(),
                        condition: GraphEdgeCondition::Error,
                    };
                    let _ = db.insert_graph_edge(&edge);
                }
            }
        }
    }

    let edges = db.list_graph_edges_for_graph("graph-1").unwrap();
    let error_edges: Vec<_> = edges
        .iter()
        .filter(|e| e.from_node == plan.node.id && e.condition == GraphEdgeCondition::Error)
        .collect();
    assert_eq!(
        error_edges.len(),
        1,
        "copied node must have exactly one Error edge to infra node"
    );
    assert_eq!(error_edges[0].to_node, "infra");
}

#[test]
fn graph_copy_ensemble_overrides_prompt_and_rewires() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_node(&u10_check("kickoff", None, Some("graph-1"), 1))
        .unwrap();
    db.insert_graph_node(&u10_agent(
        "arbiter",
        None,
        Some("graph-1"),
        2,
        serde_json::json!({"platform":"claude"}),
    ))
    .unwrap();
    db.insert_graph_node(&u10_check("review_from", None, Some("graph-1"), 3))
        .unwrap();
    db.insert_graph_node(&u10_check("review_next", None, Some("graph-1"), 4))
        .unwrap();
    let src = u10_source_ensemble(&db, None, Some("graph-1"), "kickoff", "arbiter");

    // Canonical use: copy the proposer ensemble, swap in a review prompt,
    // rewire it after the gates.
    let params: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
        "source_ensemble_id": src.ensemble.id,
        "prompt_template": "review the proposal",
        "from_node": "review_from",
        "condition": "pass",
        "on_pass_to": "review_next"
    }))
    .unwrap();
    let plan = plan_ensemble_copy(&db, &params).unwrap();

    assert_ne!(plan.built.ensemble.id, src.ensemble.id);
    assert_eq!(plan.built.ensemble.prompt_template, "review the proposal");
    assert_eq!(plan.entry_from_node, "review_from");
    assert_eq!(plan.entry_condition.as_str(), "pass");
    assert_eq!(plan.on_pass_to, "review_next");
    assert!(!plan.members_replaced);
    assert_eq!(plan.built.member_nodes.len(), 2);
    for node in &plan.built.member_nodes {
        // The shared prompt propagates to every member node's config.
        assert_eq!(node.config["prompt_template"], "review the proposal");
        assert!(
            !src.member_nodes.iter().any(|m| m.id == node.id),
            "member node ids must be fresh"
        );
    }

    // Persisting the plan yields a resolvable ensemble with the same members.
    db.insert_ensemble_unit(
        &plan.built.ensemble,
        &plan.built.members,
        &plan.built.member_nodes,
        &plan.built.join_node,
        &plan.built.edges,
    )
    .unwrap();
    let details = db
        .get_ensemble_details(&plan.built.ensemble.id)
        .unwrap()
        .unwrap();
    assert_eq!(details.members.len(), 2);
    assert_eq!(details.members[0].platform, "claude");
    assert_eq!(details.members[1].platform, "codex");
}

#[test]
fn graph_copy_ensemble_cross_graph_requires_and_uses_target_wiring() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_node(&u10_check("kickoff", None, Some("graph-1"), 1))
        .unwrap();
    db.insert_graph_node(&u10_agent(
        "arbiter",
        None,
        Some("graph-1"),
        2,
        serde_json::json!({"platform":"claude"}),
    ))
    .unwrap();
    let src = u10_source_ensemble(&db, None, Some("graph-1"), "kickoff", "arbiter");

    insert_test_graph(&db, "graph-2");
    db.insert_graph_node(&u10_check("k2", None, Some("graph-2"), 1))
        .unwrap();
    db.insert_graph_node(&u10_agent(
        "a2",
        None,
        Some("graph-2"),
        2,
        serde_json::json!({"platform":"claude"}),
    ))
    .unwrap();

    // Cross-graph copy without wiring: the source's entry/exit nodes don't
    // exist in graph-2, so it must be refused with an actionable message.
    let bad: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
        "source_ensemble_id": src.ensemble.id,
        "graph_id": "graph-2"
    }))
    .unwrap();
    let err = plan_ensemble_copy(&db, &bad).unwrap_err();
    assert!(err.contains("not found in the target graph"), "{err}");

    // With target wiring, the whole unit lands in graph-2's graph.
    let ok: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
        "source_ensemble_id": src.ensemble.id,
        "graph_id": "graph-2",
        "from_node": "k2",
        "on_pass_to": "a2"
    }))
    .unwrap();
    let plan = plan_ensemble_copy(&db, &ok).unwrap();
    assert_eq!(plan.built.ensemble.graph_id.as_deref(), Some("graph-2"));
    assert!(plan.built.ensemble.spec_id.is_none());
    assert_eq!(plan.built.join_node.graph_id.as_deref(), Some("graph-2"));
    for node in &plan.built.member_nodes {
        assert_eq!(node.graph_id.as_deref(), Some("graph-2"));
    }
    assert_eq!(plan.entry_from_node, "k2");
    assert_eq!(plan.on_pass_to, "a2");
}

#[test]
fn graph_copy_ensemble_copies_config_only_no_runtime_state() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    insert_test_graph(&db, "graph-1");
    db.insert_graph_spec(&standalone_spec("spec-src")).unwrap();
    db.insert_graph_node(&u10_check("kickoff", Some("spec-src"), None, 1))
        .unwrap();
    db.insert_graph_node(&u10_agent(
        "arbiter",
        Some("spec-src"),
        None,
        2,
        serde_json::json!({"platform":"claude"}),
    ))
    .unwrap();
    let src = u10_source_ensemble(&db, Some("spec-src"), None, "kickoff", "arbiter");

    // A completed run attached to a SOURCE member node: runtime state that
    // must never be carried into the copy.
    let source_member = src.members[0].node_id.clone();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-src".to_string(),
        graph_id: "graph-1".to_string(),
        spec_id: "spec-src".to_string(),
        node_id: source_member,
        status: GraphRunStatus::Pass,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    let params: GraphCopyEnsembleParams = serde_json::from_value(serde_json::json!({
        "source_ensemble_id": src.ensemble.id
    }))
    .unwrap();
    let plan = plan_ensemble_copy(&db, &params).unwrap();
    db.insert_ensemble_unit(
        &plan.built.ensemble,
        &plan.built.members,
        &plan.built.member_nodes,
        &plan.built.join_node,
        &plan.built.edges,
    )
    .unwrap();

    // Every copied node has a fresh identity...
    for node in &plan.built.member_nodes {
        assert!(!src.member_nodes.iter().any(|m| m.id == node.id));
    }
    // ...and NO run was copied: the only run is still the original, and
    // none reference the new member nodes.
    let runs = db.list_graph_runs_for_spec("spec-src").unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, "run-src");
    let new_ids: Vec<&str> = plan
        .built
        .member_nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    assert!(!runs.iter().any(|r| new_ids.contains(&r.node_id.as_str())));
}

// ── validate_non_empty ──────────────────────────────────────────

#[test]
fn validate_non_empty_rejects_empty_string() {
    assert!(validate_non_empty("", "name").is_err());
}

#[test]
fn validate_non_empty_rejects_whitespace_only() {
    assert!(validate_non_empty("   ", "name").is_err());
    assert!(validate_non_empty("\t\n", "name").is_err());
}

#[test]
fn validate_non_empty_accepts_valid_value() {
    assert!(validate_non_empty("hello", "name").is_ok());
    assert!(validate_non_empty("  hello  ", "name").is_ok());
}

#[test]
fn validate_non_empty_error_includes_field_name() {
    let err = validate_non_empty("", "workdir").unwrap_err();
    assert!(err.contains("workdir"), "{err}");
    assert!(err.contains("must not be empty"), "{err}");
}

// ── validate_absolute_dir ───────────────────────────────────────

#[test]
fn validate_absolute_dir_rejects_relative_path() {
    assert!(validate_absolute_dir("relative/path").is_err());
}

#[test]
fn validate_absolute_dir_rejects_nonexistent_directory() {
    let err = validate_absolute_dir("/nonexistent/dir/path").unwrap_err();
    assert!(err.contains("existing directory"), "{err}");
}

#[test]
fn validate_absolute_dir_accepts_existing_directory() {
    let dir = tempdir().unwrap();
    assert!(validate_absolute_dir(dir.path().to_str().unwrap()).is_ok());
}

// ── validate_spec_workdir ───────────────────────────────────────

#[test]
fn validate_spec_workdir_rejects_relative_path() {
    let err = validate_spec_workdir("relative/path").unwrap_err();
    assert!(err.contains("absolute path"), "{err}");
}

#[test]
fn validate_spec_workdir_accepts_absolute_path() {
    assert!(validate_spec_workdir("/some/absolute/path").is_ok());
}

#[test]
fn validate_spec_workdir_does_not_require_existing_directory() {
    assert!(validate_spec_workdir("/nonexistent/dir/path").is_ok());
}

// ── build_graph_trigger ──────────────────────────────────────────

#[test]
fn build_graph_trigger_none_returns_none() {
    assert!(build_graph_trigger(&None).unwrap().is_none());
}

#[test]
fn build_graph_trigger_manual_returns_none() {
    let params = GraphTriggerParams {
        kind: "manual".to_string(),
        schedule: None,
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    assert!(build_graph_trigger(&Some(params)).unwrap().is_none());
}

#[test]
fn build_graph_trigger_empty_kind_returns_none() {
    let params = GraphTriggerParams {
        kind: "".to_string(),
        schedule: None,
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    assert!(build_graph_trigger(&Some(params)).unwrap().is_none());
}

#[test]
fn build_graph_trigger_cron_requires_schedule() {
    let params = GraphTriggerParams {
        kind: "cron".to_string(),
        schedule: None,
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("schedule"), "{err}");
}

#[test]
fn build_graph_trigger_cron_rejects_empty_schedule() {
    let params = GraphTriggerParams {
        kind: "cron".to_string(),
        schedule: Some("".to_string()),
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("schedule"), "{err}");
}

#[test]
fn build_graph_trigger_watch_requires_path() {
    let params = GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: None,
        events: Some(vec!["modify".to_string()]),
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("path"), "{err}");
}

#[test]
fn build_graph_trigger_watch_rejects_relative_path() {
    let params = GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: Some("relative/path".to_string()),
        events: Some(vec!["modify".to_string()]),
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("absolute"), "{err}");
}

#[test]
fn build_graph_trigger_watch_requires_events() {
    let params = GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: Some("/tmp/test".to_string()),
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("event"), "{err}");
}

#[test]
fn build_graph_trigger_watch_accepts_valid_config() {
    let params = GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: Some("/tmp/test".to_string()),
        events: Some(vec!["create".to_string(), "modify".to_string()]),
        debounce_seconds: Some(5),
        recursive: Some(true),
    };
    let trigger = build_graph_trigger(&Some(params)).unwrap().unwrap();
    match trigger {
        Trigger::Watch {
            path,
            events,
            debounce_seconds,
            recursive,
        } => {
            assert_eq!(path, "/tmp/test");
            assert_eq!(events.len(), 2);
            assert_eq!(debounce_seconds, 5);
            assert!(recursive);
        }
        other => panic!("expected Trigger::Watch, got {other:?}"),
    }
}

#[test]
fn build_graph_trigger_unknown_kind_returns_error() {
    let params = GraphTriggerParams {
        kind: "unknown".to_string(),
        schedule: None,
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    };
    let err = build_graph_trigger(&Some(params)).unwrap_err();
    assert!(err.contains("Unknown trigger kind"), "{err}");
    assert!(err.contains("unknown"), "{err}");
}

// ── build_graph_completion_hook ──────────────────────────────────

#[test]
fn build_graph_completion_hook_rejects_empty_platform() {
    let params = GraphCompletionHookParams {
        platform: Some("".to_string()),
        model: None,
        effort: None,
        prompt: Some("run tests".to_string()),
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("must have either"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_whitespace_platform() {
    let params = GraphCompletionHookParams {
        platform: Some("   ".to_string()),
        model: None,
        effort: None,
        prompt: Some("run tests".to_string()),
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("must have either"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_empty_prompt() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: None,
        effort: None,
        prompt: Some("".to_string()),
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("prompt"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_whitespace_prompt() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: None,
        effort: None,
        prompt: Some("  \t  ".to_string()),
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("prompt"), "{err}");
}

#[test]
fn build_graph_completion_hook_accepts_valid_config() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: Some("opus-4".to_string()),
        effort: None,
        prompt: Some("{{graph_name}} completed".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(10),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert_eq!(hook.platform.as_deref(), Some("claude"));
    assert_eq!(hook.model, Some("opus-4".to_string()));
    assert_eq!(hook.prompt.as_deref(), Some("{{graph_name}} completed"));
    assert_eq!(hook.timeout_minutes, Some(10));
}

/// CM49: an agent hook names a crew instead of a platform — accepted
/// alone, refused beside a literal platform.
#[test]
fn build_graph_completion_hook_crew_is_the_platform_alternative() {
    let params = GraphCompletionHookParams {
        platform: None,
        crew: Some("reviewers".to_string()),
        model: None,
        effort: None,
        prompt: Some("review the diff".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert!(hook.platform.is_none(), "a crew hook names no platform");
    assert_eq!(hook.crew.as_deref(), Some("reviewers"));
    assert_eq!(hook.prompt.as_deref(), Some("review the diff"));

    let both = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        crew: Some("reviewers".to_string()),
        model: None,
        effort: None,
        prompt: Some("review the diff".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
    };
    let err = build_graph_completion_hook(&both).unwrap_err();
    assert!(err.contains("'platform' or 'crew', never both"), "{err}");

    let no_prompt = GraphCompletionHookParams {
        platform: None,
        crew: Some("reviewers".to_string()),
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,
    };
    let err = build_graph_completion_hook(&no_prompt).unwrap_err();
    assert!(err.contains("prompt"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_unknown_placeholder() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: None,
        effort: None,
        prompt: Some("failure at {{nodee}}".to_string()),
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("nodee"), "{err}");
    for marker in crate::graph_engine::GRAPH_HOOK_BINDINGS {
        assert!(err.contains(marker), "{err}");
    }
}

#[test]
fn build_graph_completion_hook_strips_whitespace() {
    let params = GraphCompletionHookParams {
        platform: Some("  claude  ".to_string()),
        model: Some("  opus  ".to_string()),
        effort: None,
        prompt: Some("  test  ".to_string()),
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
    assert_eq!(hook.platform.as_deref(), Some("claude"));
    assert_eq!(hook.model, Some("opus".to_string()));
    assert_eq!(hook.prompt.as_deref(), Some("test"));
}

#[test]
fn build_graph_completion_hook_empty_model_becomes_none() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: Some("  ".to_string()),
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

// ── member_node_config ──────────────────────────────────────────

#[test]
fn build_graph_completion_hook_rejects_both_command_and_agent() {
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: None,
        effort: None,
        prompt: Some("test".to_string()),
        command: Some("echo hi".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("command"), "{err}");
    assert!(err.contains("platform"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_neither_command_nor_agent() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("either"), "{err}");
}

#[test]
fn build_graph_completion_hook_accepts_command_only() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("echo hello".to_string()),
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(5),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert_eq!(hook.command.as_deref(), Some("echo hello"));
    assert!(hook.platform.is_none());
    assert!(hook.prompt.is_none());
    assert_eq!(hook.timeout_minutes, Some(5));
}

#[test]
fn build_graph_completion_hook_command_strips_whitespace() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: Some("  echo hello  ".to_string()),
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
    assert_eq!(hook.command.as_deref(), Some("echo hello"));
}

#[test]
fn build_graph_completion_hook_accepts_interactive_config() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: Some("  Graph {{graph_name}} failed  ".to_string()),
        command: None,
        target_session_id: Some("  session-7  ".to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert!(hook.is_interactive());
    assert!(!hook.is_command());
    assert!(!hook.is_agent());
    assert_eq!(hook.target_session_id.as_deref(), Some("session-7"));
    assert_eq!(hook.prompt.as_deref(), Some("Graph {{graph_name}} failed"));
}

#[test]
fn build_graph_completion_hook_rejects_mixed_interactive_modes() {
    // target plus platform is rejected.
    let params = GraphCompletionHookParams {
        platform: Some("claude".to_string()),
        model: None,
        effort: None,
        prompt: Some("hi".to_string()),
        command: None,
        target_session_id: Some("session-1".to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    assert!(build_graph_completion_hook(&params).is_err());

    // target plus command is rejected.
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: Some("hi".to_string()),
        command: Some("echo hi".to_string()),
        target_session_id: Some("session-1".to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    assert!(build_graph_completion_hook(&params).is_err());

    // target plus model is rejected.
    let params = GraphCompletionHookParams {
        platform: None,
        model: Some("opus".to_string()),
        effort: None,
        prompt: Some("hi".to_string()),
        command: None,
        target_session_id: Some("session-1".to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    assert!(build_graph_completion_hook(&params).is_err());

    // target plus effort is rejected.
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: Some("high".to_string()),
        prompt: Some("hi".to_string()),
        command: None,
        target_session_id: Some("session-1".to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    assert!(build_graph_completion_hook(&params).is_err());

    // target without a prompt is rejected.
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: Some("session-1".to_string()),
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("prompt"), "{err}");
}

#[test]
fn build_graph_completion_hook_accepts_name_based_interactive_config() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: Some("hi".to_string()),
        command: None,
        target_session_id: None,
        target_session_name: Some("  marasmius  ".to_string()),
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let hook = build_graph_completion_hook(&params).unwrap();
    assert!(hook.is_interactive());
    assert_eq!(hook.target_session_name.as_deref(), Some("marasmius"));
    assert!(hook.target_session_id.is_none());
}

#[test]
fn build_graph_completion_hook_rejects_both_target_id_and_name() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: Some("hi".to_string()),
        command: None,
        target_session_id: Some("session-1".to_string()),
        target_session_name: Some("session-2".to_string()),
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("target_session_id"), "{err}");
    assert!(err.contains("target_session_name"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_bare_prompt_with_no_target() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: Some("hi".to_string()),
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
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("target_session_id"), "{err}");
    assert!(err.contains("target_session_name"), "{err}");
}

#[test]
fn build_graph_completion_hook_rejects_target_graph_id_with_target_session_name() {
    let params = GraphCompletionHookParams {
        platform: None,
        model: None,
        effort: None,
        prompt: None,
        command: None,
        target_session_id: None,
        target_session_name: Some("marasmius".to_string()),
        timeout_minutes: None,
        target_graph_id: Some("graph-1".to_string()),
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let err = build_graph_completion_hook(&params).unwrap_err();
    assert!(err.contains("target_session_name"), "{err}");
}

#[test]
fn member_node_config_produces_correct_shape() {
    let config = member_node_config(
        "claude",
        Some("opus-4"),
        "do the thing",
        30,
        None,
        None,
        None,
    );
    assert_eq!(config["platform"], "claude");
    assert_eq!(config["model"], "opus-4");
    assert_eq!(config["prompt_template"], "do the thing");
    assert_eq!(config["timeout_minutes"], 30);
}

#[test]
fn member_node_config_null_model_when_none() {
    let config = member_node_config("claude", None, "prompt", 15, None, None, None);
    assert_eq!(config["platform"], "claude");
    assert!(config["model"].is_null());
}

// ── validate_edge_condition ─────────────────────────────────────

#[test]
fn validate_edge_condition_accepts_pass() {
    assert_eq!(
        validate_edge_condition("pass").unwrap(),
        GraphEdgeCondition::Pass
    );
}

#[test]
fn validate_edge_condition_accepts_fail() {
    assert_eq!(
        validate_edge_condition("fail").unwrap(),
        GraphEdgeCondition::Fail
    );
}

#[test]
fn validate_edge_condition_accepts_always() {
    assert_eq!(
        validate_edge_condition("always").unwrap(),
        GraphEdgeCondition::Always
    );
}

#[test]
fn validate_edge_condition_trims_whitespace() {
    assert_eq!(
        validate_edge_condition("  pass  ").unwrap(),
        GraphEdgeCondition::Pass
    );
}

#[test]
fn validate_edge_condition_rejects_invalid() {
    let err = validate_edge_condition("sometimes").unwrap_err();
    assert!(err.contains("pass"), "{err}");
    assert!(err.contains("fail"), "{err}");
    assert!(err.contains("always"), "{err}");
}

// ── validate_edge_condition_with_route ───────────────────────────

#[test]
fn validate_edge_condition_with_route_builds_route_condition() {
    let c = validate_edge_condition_with_route("route", Some("escalate")).unwrap();
    assert_eq!(c, GraphEdgeCondition::Route("escalate".to_string()));
}

#[test]
fn validate_edge_condition_with_route_requires_non_empty_label() {
    let err = validate_edge_condition_with_route("route", None).unwrap_err();
    assert!(err.contains("non-empty 'route' label"), "{err}");

    let err = validate_edge_condition_with_route("route", Some("  ")).unwrap_err();
    assert!(err.contains("non-empty 'route' label"), "{err}");
}

#[test]
fn validate_edge_condition_with_route_leaves_simple_conditions_untouched() {
    assert_eq!(
        validate_edge_condition_with_route("pass", None).unwrap(),
        GraphEdgeCondition::Pass
    );
    assert_eq!(
        validate_edge_condition_with_route("fail", None).unwrap(),
        GraphEdgeCondition::Fail
    );
    assert_eq!(
        validate_edge_condition_with_route("always", None).unwrap(),
        GraphEdgeCondition::Always
    );
    assert!(validate_edge_condition_with_route("sideways", None).is_err());
}

// ── validate_route_edge_target ────────────────────────────────────

#[test]
fn validate_route_edge_target_is_noop_for_non_route_conditions() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    // No node inserted at all — a non-route condition must never look it
    // up, let alone fail over a missing node.
    assert!(validate_route_edge_target(&db, "missing-node", &GraphEdgeCondition::Pass).is_ok());
}

#[test]
fn validate_route_edge_target_rejects_non_router_from_node() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_standalone_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "agent-1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Agent".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "claude" }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let err = validate_route_edge_target(
        &db,
        "agent-1",
        &GraphEdgeCondition::Route("retry".to_string()),
    )
    .unwrap_err();
    assert!(err.contains("router node"), "{err}");
}
