//! Part 24 of `endpoint_tests`: live ensemble kind/crew changes on a
//! running graph (the change applies at the next dispatch) and the
//! topology that stays refused while running.
use super::*;
use crate::domain::crews::CrewMember;

fn crew_member(platform: &str) -> CrewMember {
    CrewMember {
        position: 0,
        platform: platform.to_string(),
        model: None,
        model_ref: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

async fn running_ensemble_graph() -> (
    tempfile::TempDir,
    std::sync::Arc<Database>,
    TaskTriggerHandler,
    Graph,
    GraphSpec,
    String,
    String,
    String,
) {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ens = add_two_member_ensemble(&handler, &spec.id, "Ens", &entry, &arbiter).await;
    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();
    (dir, db, handler, lp, spec, ens, entry, arbiter)
}

fn ensemble_changed_events(
    db: &Database,
    graph_id: &str,
) -> Vec<crate::db::graphs::events::GraphEventRow> {
    db.list_graph_events(
        graph_id,
        None,
        None,
        Some(&["ensemble_changed".to_string()]),
        200,
    )
    .unwrap()
}

#[tokio::test]
async fn live_kind_change_writes_ensemble_changed_with_four_fields() {
    let (_dir, db, handler, lp, _spec, ens, _entry, _arbiter) = running_ensemble_graph().await;
    let old_kind = db
        .get_ensemble_details(&ens)
        .unwrap()
        .unwrap()
        .ensemble
        .kind
        .as_str()
        .to_string();

    let res = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            kind: Some("cascade".to_string()),
            ..blank_ensemble_update(&ens)
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&res), "{}", text(&res));

    let events = ensemble_changed_events(&db, &lp.id);
    assert_eq!(
        events.len(),
        1,
        "one ensemble_changed event, got {events:?}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(events[0].payload.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(payload["old_kind"], serde_json::json!(old_kind));
    assert_eq!(payload["new_kind"], serde_json::json!("cascade"));
    assert!(payload["old_crew"].is_null(), "{payload}");
    assert!(payload["new_crew"].is_null(), "{payload}");

    let activity = db.list_activity_log_entries(&lp.workdir, 10).unwrap();
    assert!(
        activity.iter().any(|entry| entry.message.contains("Ens")
            && (entry.message.contains("cascade") || entry.message.contains("kind"))),
        "a live kind change narrates one activity line: {:?}",
        activity
            .iter()
            .map(|entry| &entry.message)
            .collect::<Vec<_>>(),
    );
}

#[tokio::test]
async fn live_crew_bind_names_old_and_new_crew() {
    let (_dir, db, handler, lp, _spec, ens, _entry, _arbiter) = running_ensemble_graph().await;
    db.crew_create(
        "live-crew",
        None,
        &[crew_member("claude"), crew_member("opencode")],
    )
    .unwrap();

    let res = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            crew: Some(Some("live-crew".to_string())),
            ..blank_ensemble_update(&ens)
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&res), "{}", text(&res));

    let events = ensemble_changed_events(&db, &lp.id);
    assert_eq!(
        events.len(),
        1,
        "one ensemble_changed event, got {events:?}"
    );
    let payload: serde_json::Value =
        serde_json::from_str(events[0].payload.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(payload["old_kind"], payload["new_kind"]);
    assert!(payload["old_crew"].is_null(), "{payload}");
    assert_eq!(payload["new_crew"], serde_json::json!("live-crew"));
    assert_eq!(
        db.get_ensemble_details(&ens)
            .unwrap()
            .unwrap()
            .ensemble
            .crew_name
            .as_deref(),
        Some("live-crew"),
    );
}

#[tokio::test]
async fn round_robin_cursor_survives_kind_changes() {
    let (_dir, db, handler, lp, _spec, ens, _entry, _arbiter) = running_ensemble_graph().await;

    // Move to round_robin first (accepted live), then park a nonzero cursor.
    let res = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            kind: Some("round_robin".to_string()),
            ..blank_ensemble_update(&ens)
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&res), "{}", text(&res));
    db.update_ensemble_kind(&ens, None, Some(Some(1))).unwrap();

    for next in ["random", "round_robin"] {
        let res = call_graph_action(
            &handler,
            "graph_ensemble",
            "update",
            params_id(GraphUpdateEnsembleParams {
                kind: Some(next.to_string()),
                ..blank_ensemble_update(&ens)
            }),
        )
        .await
        .unwrap();
        assert!(!is_err(&res), "{next}: {}", text(&res));
        assert_eq!(
            db.get_ensemble_details(&ens)
                .unwrap()
                .unwrap()
                .ensemble
                .round_robin_index,
            Some(1),
            "cursor kept across the switch to {next}",
        );
    }
    let _ = lp;
}

#[tokio::test]
async fn undersized_crew_bind_is_refused_and_changes_nothing() {
    let (_dir, db, handler, lp, _spec, ens, _entry, _arbiter) = running_ensemble_graph().await;
    db.crew_create("tiny-crew", None, &[crew_member("claude")])
        .unwrap();

    let res = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            crew: Some(Some("tiny-crew".to_string())),
            ..blank_ensemble_update(&ens)
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&res), "{}", text(&res));
    assert!(
        text(&res).contains("must have 2"),
        "usual member-count message, got {}",
        text(&res),
    );
    let after = db.get_ensemble_details(&ens).unwrap().unwrap();
    assert_eq!(after.ensemble.crew_name, None);
    assert!(
        ensemble_changed_events(&db, &lp.id).is_empty(),
        "a refused bind narrates nothing",
    );
}

#[tokio::test]
async fn adds_and_entry_wiring_stay_refused_while_running() {
    let (_dir, db, handler, lp, spec, ens, entry, arbiter) = running_ensemble_graph().await;

    // Adding a node is topology: refused with the current message.
    let node_res = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Late".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&node_res), "{}", text(&node_res));
    assert!(text(&node_res).contains("running"), "{}", text(&node_res));

    // Adding an edge is topology too.
    let edge_res = call_graph_action(
        &handler,
        "graph_edge",
        "add",
        params_id(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: entry.clone(),
            to_node: arbiter.clone(),
            condition: "always".to_string(),
            route: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&edge_res), "{}", text(&edge_res));
    assert!(text(&edge_res).contains("running"), "{}", text(&edge_res));

    // Entry wiring stays topology-pinned.
    let entry_res = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            from_node: Some(arbiter.clone()),
            ..blank_ensemble_update(&ens)
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&entry_res), "{}", text(&entry_res));
    assert!(text(&entry_res).contains("running"), "{}", text(&entry_res));

    // None of the refusals wrote anything.
    assert!(
        db.list_graph_nodes(&spec.id)
            .unwrap()
            .iter()
            .all(|node| node.name != "Late"),
        "refused node add wrote nothing",
    );
    let _ = lp;
}

/// Spec: `graph get` without the new parameters returns byte-identical JSON to
/// `graph_details_json` (the default output must not change).
#[tokio::test]
async fn graph_get_default_output_is_byte_identical() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let got = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: lp.id.clone(),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));

    let details = db.get_graph_details(&lp.id).unwrap().unwrap();
    let expected = serde_json::to_string_pretty(
        &crate::daemon::handler_read::graph_details_json(&db, &details).unwrap(),
    )
    .unwrap();
    assert_eq!(raw_text(&got), expected);
}

/// Spec: compact mode and selector errors/dispatch.
#[tokio::test]
async fn graph_get_compact_and_selector_errors() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    // compact + node_id -> error
    let err = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: lp.id.clone(),
            compact: Some(true),
            node_id: Some("n1".to_string()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&err), "{}", text(&err));
    assert!(
        raw_text(&err).contains("compact cannot be combined with node_id or ensemble"),
        "{}",
        raw_text(&err)
    );

    // node_id + ensemble -> error
    let err = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: lp.id.clone(),
            node_id: Some("n1".to_string()),
            ensemble: Some("e1".to_string()),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&err), "{}", text(&err));
    assert!(
        raw_text(&err).contains("pass node_id or ensemble, not both"),
        "{}",
        raw_text(&err)
    );

    // compact returns summary shape
    let compact = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: lp.id.clone(),
            compact: Some(true),
            ..Default::default()
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&compact), "{}", text(&compact));
    let json: serde_json::Value = serde_json::from_str(&raw_text(&compact)).unwrap();
    assert_eq!(json["compact"], true);
    assert!(json.get("counts").is_some());
    assert!(json.get("hint").is_some());
    assert!(json.get("graph").is_none());
}
