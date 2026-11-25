//! Part 22 of `endpoint_tests`: `trigger_prompt` validation on
//! `graph_edit` create/update (FR1/FR2), its return via `graph get`, and
//! its exclusion from export/import (FR3).
use super::*;

fn cron_trigger() -> GraphTriggerParams {
    GraphTriggerParams {
        kind: "cron".to_string(),
        schedule: Some("* * * * *".to_string()),
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    }
}

fn watch_trigger(workdir: &str) -> GraphTriggerParams {
    GraphTriggerParams {
        kind: "watch".to_string(),
        schedule: None,
        path: Some(workdir.to_string()),
        events: Some(vec!["all".to_string()]),
        debounce_seconds: None,
        recursive: None,
    }
}

fn blank_update(graph_id: &str) -> GraphUpdateParams {
    GraphUpdateParams {
        graph_id: graph_id.to_string(),
        name: None,
        description: None,
        workdir: None,
        trigger: None,
        trigger_prompt: None,
        on_completed: None,
        hooks: None,
        infra_node_id: None,
        allow_dirty_start: None,
        require_spec_type: None,
        enabled: None,
        enable_at: None,
        expires_at: None,
    }
}

/// A `trigger_prompt` without a cron trigger is refused on create.
#[tokio::test]
async fn graph_create_prompt_without_trigger_is_rejected() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Prompt Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            trigger_prompt: Some("sweep the inbox".to_string()),
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&created), "{}", text(&created));
    assert!(
        text(&created).contains("set a cron trigger, or clear trigger_prompt"),
        "{}",
        text(&created)
    );
}

/// A `trigger_prompt` on a watch graph is refused on create.
#[tokio::test]
async fn graph_create_prompt_with_watch_trigger_is_rejected() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Prompt Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: Some(watch_trigger(&workdir)),
            trigger_prompt: Some("sweep the inbox".to_string()),
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&created), "{}", text(&created));
    assert!(
        text(&created).contains("set a cron trigger, or clear trigger_prompt"),
        "{}",
        text(&created)
    );
}

/// A cron graph with a `trigger_prompt` creates fine and `graph get`
/// returns the prompt.
#[tokio::test]
async fn graph_create_cron_prompt_round_trips_through_get() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Prompt Graph".to_string(),
            description: None,
            workdir,
            trigger: Some(cron_trigger()),
            trigger_prompt: Some("sweep the inbox".to_string()),
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let graph_id = extract_id(&created, "graph_id");

    let got = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: graph_id.clone(),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert_eq!(body["trigger_prompt"], "sweep the inbox");
    assert_eq!(body["trigger"]["type"], "cron");
}

/// Setting a prompt on a manual graph is refused on update.
#[tokio::test]
async fn graph_update_prompt_on_manual_graph_is_rejected() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Manual Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            trigger_prompt: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let mut params = blank_update(&graph_id);
    params.trigger_prompt = Some(Some("sweep the inbox".to_string()));
    let updated = call_graph_action(&handler, "graph_edit", "update", params_id(params))
        .await
        .unwrap();
    assert!(is_err(&updated), "{}", text(&updated));
    assert!(
        text(&updated).contains("set a cron trigger, or clear trigger_prompt"),
        "{}",
        text(&updated)
    );
}

/// Setting a prompt on a watch graph is refused on update.
#[tokio::test]
async fn graph_update_prompt_on_watch_graph_is_rejected() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Watch Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: Some(watch_trigger(&workdir)),
            trigger_prompt: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let mut params = blank_update(&graph_id);
    params.trigger_prompt = Some(Some("sweep the inbox".to_string()));
    let updated = call_graph_action(&handler, "graph_edit", "update", params_id(params))
        .await
        .unwrap();
    assert!(is_err(&updated), "{}", text(&updated));
    assert!(
        text(&updated).contains("set a cron trigger, or clear trigger_prompt"),
        "{}",
        text(&updated)
    );
}

/// Switching a prompt-carrying cron graph to a watch trigger is refused
/// on update while the prompt is kept.
#[tokio::test]
async fn graph_update_cannot_switch_to_watch_while_prompt_set() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Prompt Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: Some(cron_trigger()),
            trigger_prompt: Some("sweep the inbox".to_string()),
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let mut params = blank_update(&graph_id);
    params.trigger = Some(watch_trigger(&workdir));
    let updated = call_graph_action(&handler, "graph_edit", "update", params_id(params))
        .await
        .unwrap();
    assert!(is_err(&updated), "{}", text(&updated));
    assert!(
        text(&updated).contains("set a cron trigger, or clear trigger_prompt"),
        "{}",
        text(&updated)
    );
}

/// Removing the cron trigger of a graph that keeps a `trigger_prompt`
/// is refused; clearing both together is allowed.
#[tokio::test]
async fn graph_update_cannot_drop_cron_while_prompt_set() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Prompt Graph".to_string(),
            description: None,
            workdir,
            trigger: Some(cron_trigger()),
            trigger_prompt: Some("sweep the inbox".to_string()),
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let mut drop_trigger = blank_update(&graph_id);
    drop_trigger.trigger = Some(GraphTriggerParams {
        kind: "manual".to_string(),
        schedule: None,
        path: None,
        events: None,
        debounce_seconds: None,
        recursive: None,
    });
    let refused = call_graph_action(&handler, "graph_edit", "update", params_id(drop_trigger))
        .await
        .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    assert!(
        text(&refused).contains("set a cron trigger, or clear trigger_prompt"),
        "{}",
        text(&refused)
    );

    // Clearing both together is allowed — but a typed `None` cannot carry
    // the explicit-null "clear it" signal through `call_graph_action`
    // (it strips nulls), so this goes through the raw-JSON seam instead.
    let cleared = call_graph_action_value(
        &handler,
        "graph_edit",
        "update",
        serde_json::json!({
            "graph_id": graph_id,
            "trigger": { "kind": "manual" },
            "trigger_prompt": null,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&cleared), "{}", text(&cleared));
    let got = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: graph_id.clone(),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert!(body["trigger_prompt"].is_null(), "{body}");
}

/// Export omits the prompt (like the trigger itself) and import never
/// sets it: a re-imported cron+prompt graph is manual with no prompt.
#[tokio::test]
async fn graph_import_leaves_trigger_prompt_unset() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = call_graph_action(
        &handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "Prompt Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: Some(cron_trigger()),
            trigger_prompt: Some("sweep the inbox".to_string()),
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }),
    )
    .await
    .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let exported = call_graph_action(
        &handler,
        "graph",
        "export",
        params_id(GraphExportParams {
            graph_id: graph_id.clone(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&exported), "{}", text(&exported));
    let raw = raw_text(&exported);
    assert!(!raw.contains("trigger_prompt"), "{raw}");
    let document: serde_json::Value = serde_json::from_str(&raw).unwrap();

    let imported = call_graph_action(
        &handler,
        "graph_edit",
        "import",
        params_id(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: Some("Reimported".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    let new_id = body["graph_id"].as_str().expect("import names graph_id");
    let lp = db.get_graph(new_id).unwrap().unwrap();
    assert!(lp.trigger.is_none(), "an import never carries a trigger");
    assert!(
        lp.trigger_prompt.is_none(),
        "an import never sets a trigger_prompt"
    );
}
