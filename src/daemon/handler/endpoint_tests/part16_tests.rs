//! Part 16 of `endpoint_tests`: the refusal contract of the eight
//! second-wave resource tools, plus round trips through the actions no
//! ported test covers (a swapped dispatch arm must fail here, not in
//! production).
use super::*;
use serde_json::json;

/// Call a resource tool with a raw JSON object and return the refusal
/// message, asserting the call was refused with `INVALID_PARAMS` rather
/// than executed.
async fn refusal(
    handler: &TaskTriggerHandler,
    tool: &str,
    action: &str,
    params: serde_json::Value,
) -> String {
    match call_resource_action_value(handler, tool, action, params).await {
        Ok(result) => panic!("expected a refusal, got a result: {}", raw_text(&result)),
        Err(err) => {
            assert_eq!(
                err.code,
                rmcp::model::ErrorCode::INVALID_PARAMS,
                "wrong error code for {tool} refusal"
            );
            err.message.to_string()
        }
    }
}

/// The refusal must name the problem and then list every action of the
/// tool with its required and optional parameters — the whole table, so
/// an agent can correct the call from the refusal alone.
fn assert_full_table(message: &str, actions: &[(&str, &[&str], &[&str])]) {
    for (name, required, optional) in actions {
        assert!(
            message.contains(&format!("`{name}`")),
            "refusal must list action `{name}`: {message}"
        );
        for param in required.iter().chain(optional.iter()) {
            assert!(
                message.contains(param),
                "refusal must list param `{param}` of `{name}`: {message}"
            );
        }
    }
}

#[tokio::test]
async fn spec_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        (
            "list",
            &[],
            &[
                "workdir",
                "status",
                "unassigned_only",
                "spec_type",
                "include_descriptions",
                "limit",
                "offset",
            ],
        ),
        ("get", &["spec_id"], &[]),
        ("section", &["spec_id", "section"], &[]),
        ("notes", &["spec_id"], &[]),
    ];

    let message = refusal(&handler, "spec", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(&handler, "spec", "section", json!({"spec_id": "s1"})).await;
    assert!(
        message.contains("missing required parameter(s): section"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "spec",
        "get",
        json!({"spec_id": "s1", "frobnicate": 1}),
    )
    .await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn spec_edit_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        (
            "create",
            &["name", "description"],
            &["workdir", "spec_type", "spec_scope"],
        ),
        (
            "update",
            &["spec_id"],
            &["name", "description", "workdir", "spec_type", "spec_scope"],
        ),
        ("delete", &["spec_id"], &[]),
        ("set_status", &["spec_id", "status", "reason"], &[]),
        ("note_write", &["body"], &["run_id", "spec_id"]),
    ];

    let message = refusal(&handler, "spec_edit", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "spec_edit",
        "set_status",
        json!({"spec_id": "s1", "status": "completed"}),
    )
    .await;
    assert!(
        message.contains("missing required parameter(s): reason"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "spec_edit",
        "delete",
        json!({"spec_id": "s1", "frobnicate": 1}),
    )
    .await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn queue_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        ("create", &["name"], &[]),
        ("list", &[], &["queue_id", "include_descriptions"]),
        ("add_spec", &["queue_id", "spec_id"], &["group"]),
        ("remove_spec", &["queue_id", "spec_id"], &[]),
        ("reorder", &["queue_id", "spec_ids"], &[]),
        ("set_group", &["queue_id", "spec_id"], &["group"]),
    ];

    let message = refusal(&handler, "queue", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(&handler, "queue", "add_spec", json!({"queue_id": "q1"})).await;
    assert!(
        message.contains("missing required parameter(s): spec_id"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "queue",
        "create",
        json!({"name": "q", "frobnicate": 1}),
    )
    .await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn agent_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        ("list", &[], &[]),
        ("get", &["id"], &[]),
        ("logs", &["id"], &["lines", "since"]),
        ("status", &[], &[]),
    ];

    let message = refusal(&handler, "agent", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(&handler, "agent", "logs", json!({})).await;
    assert!(
        message.contains("missing required parameter(s): id"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(&handler, "agent", "status", json!({"frobnicate": 1})).await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn agent_edit_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        (
            "add",
            &["id", "prompt", "schedule"],
            &[
                "cli",
                "model",
                "effort",
                "duration_minutes",
                "working_dir",
                "timeout_minutes",
                "delivery_action",
                "target_session_name",
                "target_session_id",
            ],
        ),
        (
            "watch",
            &["id", "path", "events", "prompt"],
            &[
                "cli",
                "model",
                "effort",
                "debounce_seconds",
                "recursive",
                "timeout_minutes",
                "delivery_action",
                "target_session_name",
                "target_session_id",
            ],
        ),
        (
            "update",
            &["id"],
            &[
                "new_id",
                "prompt",
                "cli",
                "model",
                "effort",
                "schedule",
                "working_dir",
                "duration_minutes",
                "path",
                "events",
                "debounce_seconds",
                "recursive",
                "enabled",
                "notify_on_success",
                "delivery_action",
                "target_session_name",
                "target_session_id",
            ],
        ),
        ("remove", &["id"], &[]),
        ("enable", &["id"], &[]),
        ("disable", &["id"], &[]),
        ("schedule", &["id", "at"], &[]),
        ("run", &["id"], &[]),
    ];

    let message = refusal(&handler, "agent_edit", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(&handler, "agent_edit", "schedule", json!({"id": "a1"})).await;
    assert!(
        message.contains("missing required parameter(s): at"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "agent_edit",
        "run",
        json!({"id": "a1", "frobnicate": 1}),
    )
    .await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn harness_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        ("models", &[], &["platform", "refresh", "full"]),
        ("probe", &[], &["platform", "model", "timeout_seconds"]),
        ("probe_recent", &[], &["timeout_seconds", "max_pairs"]),
        ("refresh_registry", &[], &[]),
        ("roster_list", &[], &["platform"]),
        (
            "roster_set",
            &["platform", "state"],
            &["model_pattern", "reason", "until"],
        ),
        ("roster_clear", &["platform"], &["model_pattern"]),
        ("roster_check", &["platform"], &["model"]),
    ];

    let message = refusal(&handler, "harness", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "harness",
        "roster_set",
        json!({"platform": "claude"}),
    )
    .await;
    assert!(
        message.contains("missing required parameter(s): state"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "harness",
        "roster_check",
        json!({"platform": "claude", "frobnicate": 1}),
    )
    .await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn config_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        ("get", &["key"], &[]),
        ("list", &[], &["group"]),
        ("set", &["key", "value"], &[]),
        ("reset", &["key"], &[]),
    ];

    let message = refusal(&handler, "config", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(&handler, "config", "set", json!({"key": "theme"})).await;
    assert!(
        message.contains("missing required parameter(s): value"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(
        &handler,
        "config",
        "get",
        json!({"key": "theme", "frobnicate": 1}),
    )
    .await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

#[tokio::test]
async fn crew_refusals_list_the_full_action_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let table: &[(&str, &[&str], &[&str])] = &[
        ("list", &[], &[]),
        ("get", &["name"], &[]),
        (
            "create",
            &["name"],
            &["description", "members", "from_ensemble"],
        ),
        ("update", &["name"], &["members", "description", "rename"]),
        ("delete", &["name"], &[]),
        (
            "assignments",
            &[],
            &[
                "graph_id",
                "platform",
                "model",
                "include_archived",
                "group_by",
            ],
        ),
    ];

    let message = refusal(&handler, "crew", "frobnicate", json!({})).await;
    assert!(message.contains("Unknown action 'frobnicate'"), "{message}");
    assert_full_table(&message, table);

    let message = refusal(&handler, "crew", "get", json!({})).await;
    assert!(
        message.contains("missing required parameter(s): name"),
        "{message}"
    );
    assert_full_table(&message, table);

    let message = refusal(&handler, "crew", "assignments", json!({"frobnicate": 1})).await;
    assert!(
        message.contains("does not declare parameter(s): frobnicate"),
        "{message}"
    );
    assert_full_table(&message, table);
}

// ── Round trips through actions no ported test covers ───────────────

#[tokio::test]
async fn spec_get_returns_the_list_row_with_its_description() {
    let (_dir, db, handler) = endpoint_test_handler();

    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Gettable".to_string(),
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
        .find(|s| s.name == "Gettable")
        .expect("created spec stored")
        .id;

    let got = call_resource_action(
        &handler,
        "spec",
        "get",
        params_id(SpecGetParams {
            spec_id: spec_id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert_eq!(body["id"], spec_id);
    assert_eq!(body["name"], "Gettable");
    assert_eq!(body["description"], valid_spec_description());

    // An id prefix resolves, like every other spec_id-taking tool.
    let prefixed = call_resource_action(
        &handler,
        "spec",
        "get",
        params_id(SpecGetParams {
            spec_id: spec_id[..8].to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&prefixed), "{}", text(&prefixed));

    let missing = call_resource_action(
        &handler,
        "spec",
        "get",
        params_id(SpecGetParams {
            spec_id: "no-such-spec".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing));
}

#[tokio::test]
async fn spec_operator_note_round_trip() {
    let (_dir, db, handler) = endpoint_test_handler();

    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Noted".to_string(),
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
        .find(|s| s.name == "Noted")
        .expect("created spec stored")
        .id;

    let written = call_resource_action(
        &handler,
        "spec_edit",
        "note_write",
        params_id(SpecNoteWriteParams {
            run_id: None,
            spec_id: Some(spec_id.clone()),
            body: "operator context".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&written), "{}", text(&written));

    let notes = call_resource_action(
        &handler,
        "spec",
        "notes",
        params_id(SpecNotesGetParams {
            spec_id: spec_id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&notes), "{}", text(&notes));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&notes)).unwrap();
    assert_eq!(body.as_array().expect("notes array").len(), 1);
    assert_eq!(body[0]["body"], "operator context");
    assert_eq!(body[0]["owner_kind"], "operator");
}

#[tokio::test]
async fn queue_round_trip() {
    let (_dir, db, handler) = endpoint_test_handler();

    for name in ["First", "Second"] {
        let created = call_resource_action(
            &handler,
            "spec_edit",
            "create",
            params_id(SpecCreateParams {
                name: name.to_string(),
                description: valid_spec_description(),
                workdir: None,
                spec_type: None,
                spec_scope: None,
            }),
        )
        .await
        .unwrap();
        assert!(!is_err(&created), "{}", text(&created));
    }
    let specs = db.list_specs(None, None, false, None).unwrap();
    let first = specs.iter().find(|s| s.name == "First").unwrap().id.clone();
    let second = specs
        .iter()
        .find(|s| s.name == "Second")
        .unwrap()
        .id
        .clone();

    let created = call_resource_action(
        &handler,
        "queue",
        "create",
        params_id(QueueCreateParams {
            name: "q".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let queue_id = db.list_queues().unwrap().into_iter().next().unwrap().id;

    for spec_id in [&first, &second] {
        let added = call_resource_action(
            &handler,
            "queue",
            "add_spec",
            params_id(QueueAddSpecParams {
                queue_id: queue_id.clone(),
                spec_id: spec_id.clone(),
                group: None,
            }),
        )
        .await
        .unwrap();
        assert!(!is_err(&added), "{}", text(&added));
    }

    let listed = call_resource_action(
        &handler,
        "queue",
        "list",
        params_id(QueueListParams {
            queue_id: Some(queue_id.clone()),
            include_descriptions: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    let members = body["queue"]["members"].as_array().expect("members array");
    assert_eq!(members.len(), 2);
    assert_eq!(members[0]["id"], first);

    // Reorder swaps the pair; remove drops one; the DB agrees throughout.
    let reordered = call_resource_action(
        &handler,
        "queue",
        "reorder",
        params_id(QueueReorderParams {
            queue_id: queue_id.clone(),
            spec_ids: vec![second.clone(), first.clone()],
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&reordered), "{}", text(&reordered));

    let grouped = call_resource_action(
        &handler,
        "queue",
        "set_group",
        params_id(QueueSetGroupParams {
            queue_id: queue_id.clone(),
            spec_id: second.clone(),
            group: Some("g1".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&grouped), "{}", text(&grouped));
    assert_eq!(
        db.queue_member_group(&queue_id, &second).unwrap(),
        Some("g1".to_string())
    );

    let removed = call_resource_action(
        &handler,
        "queue",
        "remove_spec",
        params_id(QueueRemoveSpecParams {
            queue_id: queue_id.clone(),
            spec_id: first.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&removed), "{}", text(&removed));
    assert!(!db.queue_has_member(&queue_id, &first).unwrap());
    assert!(db.queue_has_member(&queue_id, &second).unwrap());
}

#[tokio::test]
async fn harness_roster_round_trip() {
    let (_dir, _db, handler) = endpoint_test_handler();

    let set = call_resource_action(
        &handler,
        "harness",
        "roster_set",
        params_id(RosterSetParams {
            platform: "claude".to_string(),
            model_pattern: None,
            state: "unavailable".to_string(),
            reason: Some("quota".to_string()),
            until: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&set), "{}", text(&set));

    let listed = call_resource_action(
        &handler,
        "harness",
        "roster_list",
        params_id(RosterListParams { platform: None }),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    assert_eq!(body.as_array().expect("roster array").len(), 1);

    let checked = call_resource_action(
        &handler,
        "harness",
        "roster_check",
        params_id(RosterCheckParams {
            platform: "claude".to_string(),
            model: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&checked), "{}", text(&checked));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&checked)).unwrap();
    assert_eq!(body["available"], false);

    let cleared = call_resource_action(
        &handler,
        "harness",
        "roster_clear",
        params_id(RosterClearParams {
            platform: "claude".to_string(),
            model_pattern: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&cleared), "{}", text(&cleared));

    let relisted = call_resource_action(
        &handler,
        "harness",
        "roster_list",
        params_id(RosterListParams { platform: None }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&relisted)).unwrap();
    assert_eq!(body.as_array().expect("roster array").len(), 0);
}

#[tokio::test]
async fn config_round_trip() {
    let home = tempdir().unwrap();
    let _home_guard = HomeVar::set(home.path());
    let (_dir, _db, handler) = endpoint_test_handler();

    let set = call_resource_action(
        &handler,
        "config",
        "set",
        params_id(ConfigSetParams {
            key: "theme".to_string(),
            value: "modern".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&set), "{}", text(&set));

    let got = call_resource_action(
        &handler,
        "config",
        "get",
        params_id(ConfigGetParams {
            key: "theme".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    assert!(raw_text(&got).contains("modern"), "{}", raw_text(&got));

    let listed = call_resource_action(
        &handler,
        "config",
        "list",
        params_id(ConfigListParams { group: None }),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    assert!(raw_text(&listed).contains("theme"), "{}", raw_text(&listed));

    let reset = call_resource_action(
        &handler,
        "config",
        "reset",
        params_id(ConfigResetParams {
            key: "theme".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&reset), "{}", text(&reset));
    assert!(raw_text(&reset).contains("classic"), "{}", raw_text(&reset));
}

#[tokio::test]
async fn crew_round_trip() {
    let (_dir, _db, handler) = endpoint_test_handler();

    let created = call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(CrewCreateParams {
            name: "impl".to_string(),
            description: None,
            members: Some(vec![crew_json_member("a"), crew_json_member("b")]),
            from_ensemble: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let got = call_resource_action(
        &handler,
        "crew",
        "get",
        params_id(CrewGetParams {
            name: "impl".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert_eq!(body["members"].as_array().expect("members").len(), 2);

    let listed = call_resource_action(&handler, "crew", "list", ())
        .await
        .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    assert!(raw_text(&listed).contains("impl"), "{}", raw_text(&listed));

    let updated = call_resource_action(
        &handler,
        "crew",
        "update",
        params_id(CrewUpdateParams {
            name: "impl".to_string(),
            members: Some(vec![crew_json_member("a")]),
            description: Some("solo".to_string()),
            rename: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let assignments = call_resource_action(
        &handler,
        "crew",
        "assignments",
        params_id(GraphCrewParams {
            graph_id: None,
            platform: None,
            model: None,
            include_archived: None,
            group_by: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&assignments), "{}", text(&assignments));

    let deleted = call_resource_action(
        &handler,
        "crew",
        "delete",
        params_id(CrewDeleteParams {
            name: "impl".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
}
