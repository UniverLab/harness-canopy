//! Part 04 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[tokio::test]
async fn graph_run_clean_workdir_unaffected_by_old_failure() {
    let (_dir, db, handler) = endpoint_test_handler();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());

    let failed_lp = insert_test_graph(&db, git_dir.path());
    insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

    let lp = insert_test_graph(&db, git_dir.path());
    insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);

    // No untracked files this time — the workdir is clean.
    let result = call_graph_action(
        &handler,
        "graph_control",
        "run",
        params_id(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: None,
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
}

#[tokio::test]
async fn graph_run_never_refused_for_non_git_workdir() {
    let (_dir, db, handler) = endpoint_test_handler();
    let plain_dir = tempdir().unwrap();

    let failed_lp = insert_test_graph(&db, plain_dir.path());
    insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

    let lp = insert_test_graph(&db, plain_dir.path());
    insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);

    std::fs::write(plain_dir.path().join("a.txt"), "a").unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "run",
        params_id(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: None,
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
}

// ── graph_edit / graph_edit ─────────────────────────

#[tokio::test]
async fn graph_add_spec_happy_path_and_position_conflict() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let added = call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id.clone(),
            name: "First Spec".to_string(),
            description: Some(valid_spec_description()),
            position: 1,
            parallelizable: false,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let conflict = call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id.clone(),
            name: "Second Spec".to_string(),
            description: Some(valid_spec_description()),
            position: 1,
            parallelizable: false,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&conflict));
    assert!(text(&conflict).contains("already has a spec at position"));

    let missing_desc = call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id.clone(),
            name: "Third Spec".to_string(),
            description: None,
            position: 2,
            parallelizable: false,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing_desc));

    let bad_template = call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id,
            name: "Fourth Spec".to_string(),
            description: Some("just a sentence".to_string()),
            position: 3,
            parallelizable: false,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_template));
    assert!(
        text(&bad_template).contains("missing required tag")
            || text(&bad_template).contains("missing")
    );
}

#[tokio::test]
async fn graph_update_spec_renames_and_rejects_no_op() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let updated = call_graph_action(
        &handler,
        "graph_edit",
        "update_spec",
        params_id(GraphUpdateSpecParams {
            spec_id: spec.id.clone(),
            name: Some("New Name".to_string()),
            description: None,
            position: None,
            parallelizable: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph_spec(&spec.id).unwrap().unwrap().name,
        "New Name"
    );

    let no_op = call_graph_action(
        &handler,
        "graph_edit",
        "update_spec",
        params_id(GraphUpdateSpecParams {
            spec_id: spec.id.clone(),
            name: None,
            description: None,
            position: None,
            parallelizable: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&no_op));

    let missing = call_graph_action(
        &handler,
        "graph_edit",
        "update_spec",
        params_id(GraphUpdateSpecParams {
            spec_id: "ghost".to_string(),
            name: Some("x".to_string()),
            description: None,
            position: None,
            parallelizable: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing));
}

// ── spec_create / spec_list / spec_update / spec_set_status / spec_delete

#[tokio::test]
async fn spec_create_and_list_and_update() {
    let (_dir, db, handler) = endpoint_test_handler();

    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Backlog item".to_string(),
            description: valid_spec_description(),
            workdir: Some("/tmp".to_string()),
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let bad = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Backlog item 2".to_string(),
            description: "too short".to_string(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad));

    let listed = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: Some(true),
            include_descriptions: None,
            limit: None,
            offset: None,
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(text(&listed).contains("Backlog item"));

    let bad_status = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: Some("sideways".to_string()),
            unassigned_only: None,
            include_descriptions: None,
            limit: None,
            offset: None,
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_status));

    let all_specs = db.list_specs(None, None, false, None).unwrap();
    let spec_id = all_specs
        .iter()
        .find(|s| s.name == "Backlog item")
        .unwrap()
        .id
        .clone();

    let updated = call_resource_action(
        &handler,
        "spec_edit",
        "update",
        params_id(SpecUpdateParams {
            spec_id: spec_id.clone(),
            name: Some("Renamed backlog item".to_string()),
            description: None,
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().name,
        "Renamed backlog item"
    );
}

#[tokio::test]
async fn spec_create_with_conventional_title_stores_subject_type_scope_and_code() {
    let (dir, db, handler) = endpoint_test_handler();

    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "fix(tui): open F1 from inside a session".to_string(),
            description: valid_spec_description(),
            workdir: Some(dir.path().to_string_lossy().to_string()),
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    // A conventional title is parsed and stored as subject + type + scope,
    // never as the raw "fix(tui): ..." title.
    let spec = db
        .list_specs(None, None, false, None)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "open F1 from inside a session")
        .expect("conventional title stored as the subject");
    assert_eq!(
        spec.spec_type
            .as_ref()
            .map(crate::domain::spec_type::SpecType::as_str),
        Some("fix")
    );
    assert_eq!(spec.spec_scope, Some("tui".to_string()));
    assert_eq!(spec.spec_code, Some("FIX-1".to_string()));
    assert_eq!(
        crate::domain::spec_type::commit_message_opt(
            spec.spec_type,
            spec.spec_scope.as_deref(),
            &spec.name,
        ),
        Some("fix(tui): open F1 from inside a session".to_string())
    );
}

/// A name-only `spec_update` adopting a conventional title on an untyped
/// spec stores the subject alone — never the raw title, which would make
/// the commit message repeat the type (`fix(tui): fix(tui): ...`).
#[tokio::test]
async fn spec_update_name_only_conventional_title_stores_subject_and_code() {
    let (dir, db, handler) = endpoint_test_handler();
    let _ = &dir;

    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Vague bug report".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let spec_id = db
        .list_specs(None, None, false, None)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "Vague bug report")
        .unwrap()
        .id;

    let updated = call_resource_action(
        &handler,
        "spec_edit",
        "update",
        params_id(SpecUpdateParams {
            spec_id: spec_id.clone(),
            name: Some("fix(tui): open F1 from inside a session".to_string()),
            description: None,
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.name, "open F1 from inside a session");
    assert_eq!(
        spec.spec_type
            .as_ref()
            .map(crate::domain::spec_type::SpecType::as_str),
        Some("fix")
    );
    assert_eq!(spec.spec_scope, Some("tui".to_string()));
    assert_eq!(
        crate::domain::spec_type::commit_message_opt(
            spec.spec_type,
            spec.spec_scope.as_deref(),
            &spec.name,
        ),
        Some("fix(tui): open F1 from inside a session".to_string())
    );
}

#[tokio::test]
async fn spec_set_status_transitions_and_rejects_bound_spec() {
    let (dir, db, handler) = endpoint_test_handler();

    let standalone = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Standalone".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&standalone));
    let spec_id = db
        .list_specs(None, None, false, None)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "Standalone")
        .unwrap()
        .id;

    let completed = call_resource_action(
        &handler,
        "spec_edit",
        "set_status",
        params_id(SpecSetStatusParams {
            spec_id: spec_id.clone(),
            status: "completed".to_string(),
            reason: "manually closed".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&completed), "{}", text(&completed));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().status,
        GraphSpecStatus::Completed
    );

    let empty_reason = call_resource_action(
        &handler,
        "spec_edit",
        "set_status",
        params_id(SpecSetStatusParams {
            spec_id: spec_id.clone(),
            status: "pending".to_string(),
            reason: "  ".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&empty_reason));

    // A graph-bound spec must be rejected as "not standalone".
    let lp = insert_test_graph(&db, dir.path());
    let bound_spec = insert_test_spec(&db, &lp.id, 1);
    let not_standalone = call_resource_action(
        &handler,
        "spec_edit",
        "set_status",
        params_id(SpecSetStatusParams {
            spec_id: bound_spec.id.clone(),
            status: "completed".to_string(),
            reason: "trying anyway".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&not_standalone));
    assert!(text(&not_standalone).contains("bound to graph"));

    let unknown_status = call_resource_action(
        &handler,
        "spec_edit",
        "set_status",
        params_id(SpecSetStatusParams {
            spec_id: spec_id.clone(),
            status: "sideways".to_string(),
            reason: "x".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&unknown_status));
}

#[tokio::test]
async fn spec_delete_removes_standalone_but_refuses_bound_spec() {
    let (dir, db, handler) = endpoint_test_handler();
    call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Deletable".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    let spec_id = db
        .list_specs(None, None, false, None)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "Deletable")
        .unwrap()
        .id;

    let deleted = call_resource_action(
        &handler,
        "spec_edit",
        "delete",
        params_id(SpecDeleteParams {
            spec_id: spec_id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    assert!(db.get_graph_spec(&spec_id).unwrap().is_none());

    let missing = call_resource_action(
        &handler,
        "spec_edit",
        "delete",
        params_id(SpecDeleteParams {
            spec_id: "ghost".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing));

    let lp = insert_test_graph(&db, dir.path());
    let bound_spec = insert_test_spec(&db, &lp.id, 1);
    let refused = call_resource_action(
        &handler,
        "spec_edit",
        "delete",
        params_id(SpecDeleteParams {
            spec_id: bound_spec.id,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused));
}

#[tokio::test]
async fn graph_remove_spec_unbinds_bound_spec() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let removed = call_graph_action(
        &handler,
        "graph_edit",
        "detach_spec",
        params_id(GraphRemoveSpecParams {
            graph_id: lp.id.clone(),
            spec_id: spec.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&removed), "{}", text(&removed));
    assert!(text(&removed).contains("unbound"));

    let after = db.get_graph_spec(&spec.id).unwrap().unwrap();
    assert!(after.graph_id.is_none());

    let unassigned = db.list_specs(None, None, true, None).unwrap();
    assert!(unassigned.iter().any(|s| s.id == spec.id));

    assert!(db
        .list_graph_specs(&lp.id)
        .unwrap()
        .iter()
        .all(|s| s.id != spec.id));
}

#[tokio::test]
async fn graph_remove_spec_rejects_running_spec() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    db.update_graph_spec_status(&spec.id, GraphSpecStatus::Running, None, None)
        .unwrap();

    let refused = call_graph_action(
        &handler,
        "graph_edit",
        "detach_spec",
        params_id(GraphRemoveSpecParams {
            graph_id: lp.id.clone(),
            spec_id: spec.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused));
    assert!(text(&refused).contains("running"));
}

#[tokio::test]
async fn graph_remove_spec_rejects_wrong_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp_a = insert_test_graph(&db, dir.path());
    let lp_b = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp_a.id, 1);

    let refused = call_graph_action(
        &handler,
        "graph_edit",
        "detach_spec",
        params_id(GraphRemoveSpecParams {
            graph_id: lp_b.id.clone(),
            spec_id: spec.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused));
    assert!(text(&refused).contains(&lp_a.id));
}

#[tokio::test]
async fn graph_remove_spec_rejects_standalone_spec() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Standalone".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let standalone_id = db
        .list_specs(None, None, false, None)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "Standalone")
        .unwrap()
        .id;

    let refused = call_graph_action(
        &handler,
        "graph_edit",
        "detach_spec",
        params_id(GraphRemoveSpecParams {
            graph_id: lp.id.clone(),
            spec_id: standalone_id,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused));
    assert!(text(&refused).contains("not bound to any graph"));
}

#[tokio::test]
async fn spec_delete_error_message_names_graph_remove_spec() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let refused = call_resource_action(
        &handler,
        "spec_edit",
        "delete",
        params_id(SpecDeleteParams {
            spec_id: spec.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused));
    assert!(text(&refused).contains("graph_edit"));
}

#[tokio::test]
async fn graph_remove_spec_preserves_execution_history() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node = insert_named_node(&db, &spec.id, "build", 1);
    insert_finalized_node_run(
        &db,
        &lp.id,
        &spec.id,
        &node.id,
        GraphRunStatus::Pass,
        None,
        chrono::Utc::now(),
    );

    let removed = call_graph_action(
        &handler,
        "graph_edit",
        "detach_spec",
        params_id(GraphRemoveSpecParams {
            graph_id: lp.id.clone(),
            spec_id: spec.id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&removed), "{}", text(&removed));

    let runs = db.list_graph_runs_for_spec(&spec.id).unwrap();
    assert_eq!(runs.len(), 1);
}

// ── blueprint_list / blueprint_create / blueprint_delete ─────

#[tokio::test]
async fn blueprint_create_list_delete_round_trip() {
    let (_dir, db, handler) = endpoint_test_handler();

    let created = handler
        .blueprint_create(Parameters(BlueprintCreateParams {
            name: "my-agent-bp".to_string(),
            kind: "agent".to_string(),
            config: agent_node_config("claude"),
        }))
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let duplicate = handler
        .blueprint_create(Parameters(BlueprintCreateParams {
            name: "my-agent-bp".to_string(),
            kind: "agent".to_string(),
            config: agent_node_config("claude"),
        }))
        .await
        .unwrap();
    assert!(is_err(&duplicate));
    assert!(text(&duplicate).contains("already exists"));

    let bad_kind = handler
        .blueprint_create(Parameters(BlueprintCreateParams {
            name: "other-bp".to_string(),
            kind: "not-a-kind".to_string(),
            config: agent_node_config("claude"),
        }))
        .await
        .unwrap();
    assert!(is_err(&bad_kind));

    let bad_config = handler
        .blueprint_create(Parameters(BlueprintCreateParams {
            name: "no-platform-bp".to_string(),
            kind: "agent".to_string(),
            config: serde_json::Map::new(),
        }))
        .await
        .unwrap();
    assert!(is_err(&bad_config));

    let listed = handler.blueprint_list().await.unwrap();
    assert!(text(&listed).contains("my-agent-bp"));
    assert!(db
        .list_blueprints()
        .unwrap()
        .iter()
        .any(|bp| bp.name == "my-agent-bp"));

    let deleted = handler
        .blueprint_delete(Parameters(BlueprintDeleteParams {
            name: "my-agent-bp".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    assert!(db.get_blueprint_by_name("my-agent-bp").unwrap().is_none());

    let missing = handler
        .blueprint_delete(Parameters(BlueprintDeleteParams {
            name: "ghost-bp".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));
}

// ── graph_node / graph_node ──────────────────────────

#[tokio::test]
async fn graph_add_node_happy_path_and_invalid_config() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Kickoff".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&added), "{}", text(&added));
    assert_eq!(db.list_graph_nodes(&spec.id).unwrap().len(), 1);

    let missing_both_targets = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: None,
            name: "Orphan".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing_both_targets));

    let bad_config = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "No Platform".to_string(),
            kind: Some("agent".to_string()),
            config: Some(serde_json::Map::new()),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_config));

    let join_rejected = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id),
            graph_id: None,
            name: "Quorum".to_string(),
            kind: Some("join".to_string()),
            config: Some(serde_json::Map::new()),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&join_rejected));
    assert!(text(&join_rejected).contains("engine-managed"));
}

/// The exact incident shape, through the real `graph_node` tool
/// call: a `prompt` key (instead of `prompt_template`) must be rejected
/// at write time, naming the correct key, and must never reach the DB.
#[tokio::test]
async fn graph_add_node_rejects_prompt_key_naming_prompt_template() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut config = agent_node_config("claude");
    config.insert(
        "prompt".to_string(),
        serde_json::Value::String("implement the spec".to_string()),
    );

    let rejected = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Implementer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&rejected), "{}", text(&rejected));
    assert!(text(&rejected).contains("prompt_template"));
    assert!(db.list_graph_nodes(&spec.id).unwrap().is_empty());
}

/// CM35 FR2: a node naming a platform whose registry entry is
/// `headless = false` (freebuff) is rejected by `graph_node` with the
/// exact spec wording, and nothing reaches the DB. A platform left at the
/// serde default (`headless` absent) is unaffected — the happy-path test
/// above still adds its `claude` node with this check in place.
#[tokio::test]
async fn graph_add_node_rejects_interactive_only_platform() {
    let (dir, db, handler) = endpoint_test_handler();
    let _home = HomeVar::set(dir.path());
    crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "freebuff".to_string(),
            binary: "freebuff".to_string(),
            headless: false,
            ..Default::default()
        }],
        ..Default::default()
    }
    .save_for_tests(&dir.path().join(".canopy"))
    .unwrap();

    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let rejected = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Live Only".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("freebuff")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&rejected), "{}", text(&rejected));
    assert!(
        raw_text(&rejected).contains("platform 'freebuff' is interactive-only (no headless mode)"),
        "{}",
        raw_text(&rejected)
    );
    assert!(db.list_graph_nodes(&spec.id).unwrap().is_empty());
}

/// CM35 FR2: `graph_node` rejects the same platform with the same
/// wording, and a rejected update leaves the stored node untouched. The
/// node is created first and only then repointed — the ordering an
/// operator actually hits when "fixing" a node onto an interactive-only
/// harness.
#[tokio::test]
async fn graph_update_node_rejects_interactive_only_platform() {
    let home = tempdir().unwrap();
    write_freebuff_config(home.path());
    let _home_guard = HomeVar::set(home.path());

    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Implementer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    let rejected = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some(agent_node_config("freebuff")),
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&rejected), "{}", text(&rejected));
    assert!(
        raw_text(&rejected).contains("platform 'freebuff' is interactive-only (no headless mode)"),
        "{}",
        raw_text(&rejected)
    );
    let stored = db.get_graph_node(&node_id).unwrap().unwrap();
    assert_eq!(
        stored.config["platform"].as_str(),
        Some("claude"),
        "a rejected update must not reach the DB"
    );
}

/// CM46 req 7: `graph_node` records a `config_changed` event for a
/// RUNNING graph and none for an idle one.
#[tokio::test]
async fn graph_update_node_records_config_changed_only_for_running_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Implementer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    // Idle (Draft) graph: an edit is recorded nowhere.
    call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some(agent_node_config("claude")),
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        db.list_graph_events(&lp.id, None, None, None, 200)
            .unwrap()
            .is_empty(),
        "an idle graph's edit must not produce a config_changed event"
    );

    // Running graph: the same edit now records exactly one event.
    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();
    call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: Some("Renamed".to_string()),
            kind: None,
            config: None,
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();

    let events = db.list_graph_events(&lp.id, None, None, None, 200).unwrap();
    assert_eq!(events.len(), 1, "got: {events:?}");
    assert_eq!(events[0].kind, "config_changed");
    assert_eq!(events[0].node_id.as_deref(), Some(node_id.as_str()));
    assert_eq!(events[0].spec_id.as_deref(), Some(spec.id.as_str()));
    let payload: serde_json::Value =
        serde_json::from_str(events[0].payload.as_deref().unwrap()).unwrap();
    assert_eq!(payload["change"], "node");
}

#[tokio::test]
async fn graph_update_node_renames_and_validates() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node_id = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Node A".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&node_id, "node_id");

    let renamed = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: Some("Node A Renamed".to_string()),
            kind: None,
            config: None,
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&renamed), "{}", text(&renamed));
    assert_eq!(
        db.get_graph_node(&node_id).unwrap().unwrap().name,
        "Node A Renamed"
    );

    let no_op = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: None,
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&no_op));

    let missing = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: "ghost".to_string(),
            name: Some("x".to_string()),
            kind: None,
            config: None,
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing));
}

#[tokio::test]
async fn graph_add_node_accepts_skills_on_agent_and_preserves_order() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut config = agent_node_config("claude");
    config.insert(
        "skills".to_string(),
        serde_json::json!(["alpha", "beta", "gamma"]),
    );

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Implementer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&added), "{}", text(&added));
    let node_id = extract_id(&added, "node_id");

    let stored = db.get_graph_node(&node_id).unwrap().unwrap();
    let skills = stored.config["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 3);
    assert_eq!(skills[0].as_str().unwrap(), "alpha");
    assert_eq!(skills[1].as_str().unwrap(), "beta");
    assert_eq!(skills[2].as_str().unwrap(), "gamma");

    // Spec FR4: the names must reach the engine's resolution path in
    // listed order. `node_pinned_skills` (graph_engine.rs) is private, so
    // replicate its exact extraction from the persisted config here.
    let resolved: Vec<&str> = stored.config["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(resolved, ["alpha", "beta", "gamma"]);
}

#[tokio::test]
async fn graph_update_node_accepts_skills_on_agent_and_preserves_order() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Implementer".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    let mut new_config = agent_node_config("claude");
    new_config.insert(
        "skills".to_string(),
        serde_json::json!(["delta", "epsilon"]),
    );

    let updated = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some(new_config),
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let stored = db.get_graph_node(&node_id).unwrap().unwrap();
    let skills = stored.config["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 2);
    assert_eq!(skills[0].as_str().unwrap(), "delta");
    assert_eq!(skills[1].as_str().unwrap(), "epsilon");
}

#[tokio::test]
async fn graph_update_node_partial_config_merge_preserves_prompt() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let long_prompt = "A very long prompt that should not be touched by a model change.\
            Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor.\
            Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.";
    let mut config = agent_node_config("old-platform");
    config.insert(
        "prompt_template".to_string(),
        serde_json::json!(long_prompt),
    );
    config.insert("model".to_string(), serde_json::json!("old-model"));

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Agent".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    let updated = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some({
                let mut m = serde_json::Map::new();
                m.insert("model".to_string(), serde_json::json!("new-model"));
                m
            }),
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let stored = db.get_graph_node(&node_id).unwrap().unwrap();
    assert_eq!(
        stored.config["prompt_template"].as_str().unwrap(),
        long_prompt
    );
    assert_eq!(stored.config["model"].as_str().unwrap(), "new-model");
    assert_eq!(stored.config["platform"].as_str().unwrap(), "old-platform");

    let response_text = text(&updated);
    assert!(
        response_text.contains("model"),
        "response should name changed key: {response_text}"
    );
}

#[tokio::test]
async fn graph_update_node_partial_config_merge_timeout_leaves_others() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut config = agent_node_config("claude");
    config.insert("model".to_string(), serde_json::json!("some-model"));
    config.insert("commit_rights".to_string(), serde_json::json!(true));

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Agent".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    let updated = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some({
                let mut m = serde_json::Map::new();
                m.insert("timeout_minutes".to_string(), serde_json::json!(30));
                m
            }),
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let stored = db.get_graph_node(&node_id).unwrap().unwrap();
    assert_eq!(stored.config["platform"].as_str().unwrap(), "claude");
    assert_eq!(stored.config["model"].as_str().unwrap(), "some-model");
    assert_eq!(stored.config["commit_rights"], serde_json::json!(true));
    assert_eq!(stored.config["timeout_minutes"], serde_json::json!(30));
}

#[tokio::test]
async fn graph_update_node_full_replacement_drops_keys() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut config = agent_node_config("claude");
    config.insert("model".to_string(), serde_json::json!("some-model"));
    config.insert("timeout_minutes".to_string(), serde_json::json!(10));

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Agent".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    let updated = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some({
                let mut m = serde_json::Map::new();
                m.insert("platform".to_string(), serde_json::json!("new-platform"));
                m
            }),
            config_replace: Some(true),
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let stored = db.get_graph_node(&node_id).unwrap().unwrap();
    assert_eq!(stored.config["platform"].as_str().unwrap(), "new-platform");
    assert!(
        stored.config.get("model").is_none(),
        "model should be dropped by full replacement"
    );
    assert!(
        stored.config.get("timeout_minutes").is_none(),
        "timeout_minutes should be dropped by full replacement"
    );
}

#[tokio::test]
async fn graph_update_node_unknown_key_rejected_in_both_modes() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let added = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Agent".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    let node_id = extract_id(&added, "node_id");

    let merge_result = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some({
                let mut m = serde_json::Map::new();
                m.insert("totally_bogus_key".to_string(), serde_json::json!("x"));
                m
            }),
            config_replace: None,
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&merge_result));
    assert!(
        text(&merge_result).contains("totally_bogus_key"),
        "error should name the bad key: {}",
        text(&merge_result)
    );
    assert!(
        text(&merge_result).contains("Accepted keys"),
        "error should name accepted keys: {}",
        text(&merge_result)
    );

    let replace_result = call_graph_action(
        &handler,
        "graph_node",
        "update",
        params_id(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some({
                let mut m = serde_json::Map::new();
                m.insert("another_bogus".to_string(), serde_json::json!("y"));
                m
            }),
            config_replace: Some(true),
            position: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&replace_result));
    assert!(
        text(&replace_result).contains("another_bogus"),
        "error should name the bad key: {}",
        text(&replace_result)
    );
}
