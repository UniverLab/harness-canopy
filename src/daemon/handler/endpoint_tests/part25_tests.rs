//! Part 25 of `endpoint_tests`: named model entries (`crew model_set` /
//! `model_list` / `model_delete`) and `model_ref` crew members — one edit
//! reaches every tracking crew and every bound ensemble.
use super::*;
use crate::daemon::params::{
    CrewCreateParams, CrewGetParams, CrewMemberJson, CrewModelDeleteParams, CrewModelSetParams,
    CrewUpdateParams, GraphCrewParams,
};
use crate::domain::graphs::{GraphCompletionHook, GraphHookEvent};

fn ref_crew_params(name: &str) -> CrewCreateParams {
    CrewCreateParams {
        name: name.to_string(),
        description: None,
        members: Some(vec![
            crew_json_ref_member("agy-main"),
            crew_json_ref_member("agy-main"),
        ]),
        from_ensemble: None,
    }
}

async fn model_test_setup() -> (
    tempfile::TempDir,
    std::sync::Arc<Database>,
    TaskTriggerHandler,
    String,
    String,
    String,
) {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let set = call_resource_action(
        &handler,
        "crew",
        "model_set",
        params_id(CrewModelSetParams {
            name: "agy-main".to_string(),
            platform: "antigravity".to_string(),
            model: Some("gen-medium".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&set), "{}", text(&set));
    let created = call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(ref_crew_params("eager")),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let (running_graph, running_ens) = build_crew_ensemble(&handler, &workdir, "Running G").await;
    let (_idle_graph, idle_ens) = build_crew_ensemble(&handler, &workdir, "Idle G").await;
    db.update_graph_status(&running_graph, GraphStatus::Running, None, None)
        .unwrap();
    (dir, db, handler, running_graph, running_ens, idle_ens)
}

#[tokio::test]
async fn model_set_rewrites_crews_and_syncs_idle_but_defers_running() {
    let (_dir, db, handler, running_graph, running_ens, idle_ens) = model_test_setup().await;

    let got = call_resource_action(
        &handler,
        "crew",
        "get",
        params_id(CrewGetParams {
            name: "eager".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert_eq!(body["version"], 1);
    assert!(
        body["members"].as_array().unwrap().iter().all(|member| {
            member["platform"] == "antigravity"
                && member["model"] == "gen-medium"
                && member["model_ref"] == "agy-main"
        }),
        "{body}"
    );

    let changed = call_resource_action(
        &handler,
        "crew",
        "model_set",
        params_id(CrewModelSetParams {
            name: "agy-main".to_string(),
            platform: "antigravity".to_string(),
            model: Some("gen-high".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&changed), "{}", text(&changed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&changed)).unwrap();
    assert_eq!(
        body["crews_updated"],
        serde_json::json!(["eager"]),
        "{body}"
    );
    let sync = body["sync"].as_array().expect("sync array in {body}");
    let running_entry = sync
        .iter()
        .find(|entry| entry["ensemble_id"] == running_ens)
        .expect("a sync entry for the running graph's ensemble");
    assert_eq!(running_entry["outcome"], "DeferredRunningGraph");
    assert_eq!(running_entry["note"], "applies at next dispatch");
    let idle_entry = sync
        .iter()
        .find(|entry| entry["ensemble_id"] == idle_ens)
        .expect("a sync entry for the idle graph's ensemble");
    assert!(
        idle_entry["outcome"]
            .as_str()
            .is_some_and(|outcome| outcome.contains("Synced")),
        "{idle_entry}"
    );

    let (crew, members) = db.crew_get("eager").unwrap().unwrap();
    assert_eq!(
        crew.version, 2,
        "one model change bumps the crew exactly once"
    );
    assert!(
        members.iter().all(|member| member.platform == "antigravity"
            && member.model.as_deref() == Some("gen-high")
            && member.model_ref.as_deref() == Some("agy-main")),
        "every tracking member is rewritten"
    );
    let running = db.get_ensemble_details(&running_ens).unwrap().unwrap();
    assert_eq!(running.ensemble.crew_version_applied, Some(1));
    assert!(
        running
            .members
            .iter()
            .all(|member| member.model.as_deref() == Some("gen-medium")),
        "a running graph's member rows are left behind"
    );
    let idle = db.get_ensemble_details(&idle_ens).unwrap().unwrap();
    assert_eq!(idle.ensemble.crew_version_applied, Some(2));
    assert!(
        idle.members
            .iter()
            .all(|member| member.model.as_deref() == Some("gen-high")),
        "an idle graph picks the new pair up at once"
    );

    // The deferred ensemble applies the newer version at its next sync.
    db.update_graph_status(&running_graph, GraphStatus::Draft, None, None)
        .unwrap();
    let outcome = db.sync_ensemble_with_crew(&running_ens).unwrap();
    assert!(
        matches!(outcome, crate::db::crews::SyncOutcome::Synced { .. }),
        "{outcome:?}"
    );
    let running = db.get_ensemble_details(&running_ens).unwrap().unwrap();
    assert_eq!(running.ensemble.crew_version_applied, Some(2));
    assert!(
        running
            .members
            .iter()
            .all(|member| member.model.as_deref() == Some("gen-high")),
        "the deferred rows land on the next sync"
    );
}

#[tokio::test]
async fn model_set_same_pair_is_a_no_op() {
    let (_dir, db, handler, _running_graph, _running_ens, _idle_ens) = model_test_setup().await;

    let same = call_resource_action(
        &handler,
        "crew",
        "model_set",
        params_id(CrewModelSetParams {
            name: "agy-main".to_string(),
            platform: "antigravity".to_string(),
            model: Some("gen-medium".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&same), "{}", text(&same));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&same)).unwrap();
    assert_eq!(body["crews_updated"], serde_json::json!([]), "{body}");
    assert_eq!(body["sync"], serde_json::json!([]), "{body}");
    let (crew, _) = db.crew_get("eager").unwrap().unwrap();
    assert_eq!(crew.version, 1, "a no-op set bumps nothing");
}

#[tokio::test]
async fn model_ref_member_errors_name_the_member() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let set = call_resource_action(
        &handler,
        "crew",
        "model_set",
        params_id(CrewModelSetParams {
            name: "agy-main".to_string(),
            platform: "antigravity".to_string(),
            model: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&set), "{}", text(&set));

    let unknown = call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(CrewCreateParams {
            name: "bad".to_string(),
            description: None,
            members: Some(vec![crew_json_ref_member("missing")]),
            from_ensemble: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&unknown), "{}", text(&unknown));
    assert!(
        text(&unknown).contains("unknown model_ref 'missing'"),
        "{}",
        text(&unknown)
    );

    let both = CrewMemberJson {
        platform: Some("p".to_string()),
        model: None,
        model_ref: Some("agy-main".to_string()),
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    let refused = call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(CrewCreateParams {
            name: "bad".to_string(),
            description: None,
            members: Some(vec![both]),
            from_ensemble: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    assert!(text(&refused).contains("member 0"), "{}", text(&refused));
    assert!(
        text(&refused).contains("exactly one of platform/model_ref"),
        "{}",
        text(&refused)
    );

    let neither = CrewMemberJson {
        platform: None,
        model: None,
        model_ref: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    let refused = call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(CrewCreateParams {
            name: "bad".to_string(),
            description: None,
            members: Some(vec![neither]),
            from_ensemble: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    assert!(text(&refused).contains("member 0"), "{}", text(&refused));
    assert!(
        text(&refused).contains("exactly one of platform/model_ref"),
        "{}",
        text(&refused)
    );
}

#[tokio::test]
async fn model_delete_is_refused_while_referenced_then_succeeds() {
    let (_dir, db, handler, _running_graph, _running_ens, _idle_ens) = model_test_setup().await;

    let refused = call_resource_action(
        &handler,
        "crew",
        "model_delete",
        params_id(CrewModelDeleteParams {
            name: "agy-main".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    assert!(text(&refused).contains("eager"), "{}", text(&refused));

    let listed = call_resource_action(
        &handler,
        "crew",
        "model_list",
        params_id(serde_json::json!({})),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    assert_eq!(body[0]["name"], "agy-main");
    assert_eq!(body[0]["platform"], "antigravity");
    assert_eq!(body[0]["crews"], serde_json::json!(["eager"]), "{body}");

    let updated = call_resource_action(
        &handler,
        "crew",
        "update",
        params_id(CrewUpdateParams {
            name: "eager".to_string(),
            members: Some(vec![crew_json_member("m1"), crew_json_member("m2")]),
            description: None,
            rename: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let deleted = call_resource_action(
        &handler,
        "crew",
        "model_delete",
        params_id(CrewModelDeleteParams {
            name: "agy-main".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    assert!(db.crew_model_list().unwrap().is_empty());
}

async fn setup_unbound_audit_fixture() -> (
    tempfile::TempDir,
    std::sync::Arc<Database>,
    TaskTriggerHandler,
    String,
) {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();

    let created_crew = call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(CrewCreateParams {
            name: "eager".to_string(),
            description: None,
            members: Some(vec![crew_json_member("claude"), crew_json_member("codex")]),
            from_ensemble: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created_crew), "{}", text(&created_crew));

    let (mixed_graph, _) = build_crew_ensemble(&handler, &workdir, "Mixed").await;
    let bound_node = call_graph_action(
        &handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(mixed_graph.clone()),
            name: "crew-node".to_string(),
            kind: Some("agent".to_string()),
            config: serde_json::json!({ "crew": "eager", "prompt_template": "run" })
                .as_object()
                .cloned(),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&bound_node), "{}", text(&bound_node));

    let nodes: Vec<_> = db
        .list_all_graph_nodes()
        .unwrap()
        .into_iter()
        .filter(|n| n.graph_id.as_deref() == Some(mixed_graph.as_str()))
        .collect();
    let kickoff_id = nodes
        .iter()
        .find(|n| n.name == "kickoff")
        .unwrap()
        .id
        .clone();
    let downstream_id = nodes
        .iter()
        .find(|n| n.name == "downstream")
        .unwrap()
        .id
        .clone();
    let literal_ens = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(serde_json::json!({
            "graph_id": mixed_graph,
            "name": "literal-ens",
            "from_node": kickoff_id,
            "condition": "always",
            "on_pass_to": downstream_id,
            "members": [
                { "platform": "claude", "model": "sonnet" },
                { "platform": "codex", "model": "gpt-5" }
            ],
            "prompt_template": "review"
        })),
    )
    .await
    .unwrap();
    assert!(!is_err(&literal_ens), "{}", text(&literal_ens));

    let mut graph_rec = db.get_graph(&mixed_graph).unwrap().unwrap();
    graph_rec.hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![GraphCompletionHook {
            platform: Some("claude".to_string()),
            crew: None,
            spec_ids: None,
            model: Some("haiku".to_string()),
            effort: None,
            prompt: Some("notify".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(5),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        }],
    );
    db.update_graph_hooks(&mixed_graph, &graph_rec.hooks)
        .unwrap();
    add_bound_and_archived_graphs(&db, &handler, &workdir).await;

    (dir, db, handler, mixed_graph)
}

async fn add_bound_and_archived_graphs(db: &Database, handler: &TaskTriggerHandler, workdir: &str) {
    let fully_bound = call_graph_action(
        handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "FullyBound".to_string(),
            description: None,
            workdir: workdir.to_string(),
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
    let fully_bound_id = extract_id(&fully_bound, "graph_id");
    let bound_only_node = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(fully_bound_id),
            name: "bound-only".to_string(),
            kind: Some("agent".to_string()),
            config: serde_json::json!({ "crew": "eager", "prompt_template": "run" })
                .as_object()
                .cloned(),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&bound_only_node), "{}", text(&bound_only_node));

    let archived = call_graph_action(
        handler,
        "graph_edit",
        "create",
        params_id(GraphCreateParams {
            name: "ArchivedLit".to_string(),
            description: None,
            workdir: workdir.to_string(),
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
    let archived_id = extract_id(&archived, "graph_id");
    let arch_node = call_graph_action(
        handler,
        "graph_node",
        "add",
        params_id(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(archived_id.clone()),
            name: "arch-lit".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("codex")),
            node: None,
            config_overrides: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&arch_node), "{}", text(&arch_node));
    db.archive_graph(&archived_id).unwrap();
}

#[tokio::test]
async fn graph_audit_and_crew_assignments_unbound_report_literal_targets() {
    let (_dir, _db, handler, mixed_graph) = setup_unbound_audit_fixture().await;

    let audit = call_graph_action(&handler, "graph", "audit", params_id(serde_json::json!({})))
        .await
        .unwrap();
    assert!(!is_err(&audit), "{}", text(&audit));
    let audit_body: serde_json::Value = serde_json::from_str(&raw_text(&audit)).unwrap();
    assert_eq!(audit_body["unbound_to_crew_total"], 4, "{audit_body}");
    let unbound_graphs = audit_body["unbound_to_crew"].as_array().unwrap();
    assert_eq!(unbound_graphs.len(), 1, "{audit_body}");
    assert_eq!(unbound_graphs[0]["graph_id"], mixed_graph);
    assert_eq!(
        unbound_graphs[0]["agent_nodes"].as_array().unwrap().len(),
        1
    );
    assert_eq!(unbound_graphs[0]["agent_nodes"][0]["name"], "downstream");
    let ensembles = unbound_graphs[0]["ensembles"].as_array().unwrap();
    assert_eq!(ensembles.len(), 1, "{audit_body}");
    assert_eq!(ensembles[0]["name"], "literal-ens");
    assert_eq!(ensembles[0]["members"].as_array().unwrap().len(), 2);
    let hooks = unbound_graphs[0]["hooks"].as_array().unwrap();
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0]["event"], "on_completed");

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
            unbound: Some(true),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&assignments), "{}", text(&assignments));
    let assign_body: serde_json::Value = serde_json::from_str(&raw_text(&assignments)).unwrap();
    let assign_graphs = assign_body.as_array().unwrap();
    assert_eq!(assign_graphs.len(), 1, "{assign_body}");
    assert_eq!(assign_graphs[0]["id"], mixed_graph);
    assert_eq!(assign_graphs[0]["rows"].as_array().unwrap().len(), 4);

    let with_archived = call_resource_action(
        &handler,
        "crew",
        "assignments",
        params_id(GraphCrewParams {
            graph_id: None,
            platform: None,
            model: None,
            include_archived: Some(true),
            group_by: None,
            unbound: Some(true),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&with_archived), "{}", text(&with_archived));
    let arch_body: serde_json::Value = serde_json::from_str(&raw_text(&with_archived)).unwrap();
    assert_eq!(arch_body.as_array().unwrap().len(), 2, "{arch_body}");

    let filtered = call_resource_action(
        &handler,
        "crew",
        "assignments",
        params_id(GraphCrewParams {
            graph_id: None,
            platform: Some("claude".to_string()),
            model: None,
            include_archived: Some(true),
            group_by: None,
            unbound: Some(true),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&filtered), "{}", text(&filtered));
    let filtered_body: serde_json::Value = serde_json::from_str(&raw_text(&filtered)).unwrap();
    let filtered_graphs = filtered_body.as_array().unwrap();
    assert_eq!(filtered_graphs.len(), 1, "{filtered_body}");
    assert_eq!(filtered_graphs[0]["rows"].as_array().unwrap().len(), 3);
}
