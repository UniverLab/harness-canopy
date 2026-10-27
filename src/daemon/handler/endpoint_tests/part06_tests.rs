//! Part 06 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[tokio::test]
async fn graph_update_node_router_enforces_route_coverage_and_edge_consistency() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let router_result = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Router".to_string(),
            kind: Some("router".to_string()),
            config: Some(router_node_config(&two_routes(), "retry")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let router_id = extract_id(&router_result, "node_id");
    let retry_target = add_agent_node(&handler, &spec.id, "Retry target").await;

    // Wire only the "retry" route — "escalate" is declared but not yet
    // served by any edge.
    let edge = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: router_id.clone(),
            to_node: retry_target,
            condition: "route".to_string(),
            route: Some("retry".to_string()),
        }))
        .await
        .unwrap();
    assert!(!is_err(&edge), "{}", text(&edge));

    // Re-asserting the same routes now that wiring has started must
    // fail: "escalate" has no outgoing edge.
    let no_coverage = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: router_id.clone(),
            name: None,
            kind: None,
            config: Some(router_node_config(&two_routes(), "retry")),
            config_replace: None,
            position: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&no_coverage));
    assert!(text(&no_coverage).contains("'escalate' has no outgoing edge"));

    // Changing the declared routes so they no longer include "retry"
    // leaves the existing retry-labeled edge dangling on an undeclared
    // route.
    let stale_edge = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: router_id.clone(),
            name: None,
            kind: None,
            config: Some(router_node_config(
                &serde_json::json!([
                    { "label": "escalate", "description": "Hand off to a human." },
                    { "label": "abort", "description": "Give up." },
                ]),
                "escalate",
            )),
            config_replace: None,
            position: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&stale_edge));
    assert!(text(&stale_edge).contains("undeclared route 'retry'"));
}

// ── graph_add_ensemble ──────────────────────────────────────────

#[tokio::test]
async fn graph_add_ensemble_happy_path_and_validation_errors() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let members = vec![
        crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        crate::daemon::params::EnsembleMemberParams {
            platform: "opencode".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];

    let created = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Review Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(members.clone()),
            blueprint: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: Some(2),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    assert_eq!(db.list_ensembles_for_spec(&spec.id).unwrap().len(), 1);

    let missing_prompt = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "No Prompt".to_string(),
            kind: None,
            prompt_template: None,
            members: Some(members.clone()),
            blueprint: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing_prompt));
    assert!(text(&missing_prompt).contains("prompt_template"));

    let bad_min_pass = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Bad Min Pass".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(members.clone()),
            blueprint: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: Some(99),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&bad_min_pass));
    assert!(text(&bad_min_pass).contains("has an invalid min_pass"));

    let unknown_entry = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Bad Entry".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(members.clone()),
            blueprint: None,
            from_node: "not-a-node".to_string(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&unknown_entry));
    assert!(text(&unknown_entry).contains("not found"));

    let too_few_members = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id),
            graph_id: None,
            name: "Too Few".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![members[0].clone()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter,
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&too_few_members));
}

/// CM35 FR2: an ensemble member naming an interactive-only platform is
/// rejected with the spec wording and no ensemble is persisted.
/// `graph_add_ensemble` and `graph_update_ensemble` both validate
/// members through `validate_ensemble_members`, so the add arm pins the
/// shared gate; a second member on a normal platform proves the check is
/// per-member, not whole-ensemble.
#[tokio::test]
async fn graph_add_ensemble_rejects_interactive_only_member() {
    let home = tempdir().unwrap();
    write_freebuff_config(home.path());
    let _home_guard = HomeVar::set(home.path());

    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let member = |platform: &str| crate::daemon::params::EnsembleMemberParams {
        platform: platform.to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };

    let rejected = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Live Panel".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![member("freebuff"), member("claude")]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter,
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&rejected), "{}", text(&rejected));
    assert!(
        raw_text(&rejected).contains("platform 'freebuff' is interactive-only (no headless mode)"),
        "{}",
        raw_text(&rejected)
    );
    assert!(db.list_ensembles_for_spec(&spec.id).unwrap().is_empty());
}

/// CM24: a negative `quorum_grace_minutes` is refused on add with the
/// same wording pattern as the existing `straggler_timeout_minutes`
/// check — while `0` (terminate immediately) is accepted and stored.
#[tokio::test]
async fn graph_add_ensemble_rejects_negative_quorum_grace() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let member = || crate::daemon::params::EnsembleMemberParams {
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };

    let negative = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Negative Grace".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![member(), member()]),
            blueprint: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: Some(-1),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&negative));
    assert!(
        text(&negative).contains("quorum_grace_minutes must not be negative."),
        "{}",
        text(&negative)
    );

    let immediate = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Immediate Grace".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: Some(0),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter,
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&immediate), "{}", text(&immediate));
    let ensemble_id = extract_id(&immediate, "ensemble_id");
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.quorum_grace_minutes, Some(0));
}

/// CM24: `quorum_grace_minutes` can be set and cleared back to `None`
/// (wait-for-all) via `graph_update_ensemble`, mirroring the
/// `straggler_timeout_minutes` clear path.
#[tokio::test]
async fn graph_update_ensemble_sets_and_clears_quorum_grace() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let member = || crate::daemon::params::EnsembleMemberParams {
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    let created = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Grace Update".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter,
            on_fail_to: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");

    let negative = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            ensemble_id: ensemble_id.clone(),
            quorum_grace_minutes: Some(Some(-2)),
            ..blank_ensemble_update(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(is_err(&negative));
    assert!(
        text(&negative).contains("quorum_grace_minutes must not be negative."),
        "{}",
        text(&negative)
    );

    let set = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            ensemble_id: ensemble_id.clone(),
            quorum_grace_minutes: Some(Some(5)),
            ..blank_ensemble_update(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(!is_err(&set), "{}", text(&set));
    assert_eq!(
        db.get_ensemble(&ensemble_id)
            .unwrap()
            .unwrap()
            .quorum_grace_minutes,
        Some(5)
    );

    let cleared = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            ensemble_id: ensemble_id.clone(),
            quorum_grace_minutes: Some(None),
            ..blank_ensemble_update(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(!is_err(&cleared), "{}", text(&cleared));
    assert_eq!(
        db.get_ensemble(&ensemble_id)
            .unwrap()
            .unwrap()
            .quorum_grace_minutes,
        None
    );
}

// ── graph_get / graph_list ──────────────────────────────────────

#[tokio::test]
async fn graph_get_and_graph_list() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let got = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    assert!(raw_text(&got).contains(&lp.id));

    let missing = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: "ghost".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));

    let listed = handler
        .graph_list(Parameters(GraphListParams {
            workdir: None,
            include_archived: None,
        }))
        .await
        .unwrap();
    assert!(raw_text(&listed).contains(&lp.id));

    let filtered_out = handler
        .graph_list(Parameters(GraphListParams {
            workdir: Some("/nowhere".to_string()),
            include_archived: None,
        }))
        .await
        .unwrap();
    assert!(!raw_text(&filtered_out).contains(&lp.id));
}

/// `graph_get`'s node JSON must make an agent node's prompt source
/// visible: `"explicit"` for a `prompt_template`, `"preset"` for a
/// `prompt_preset`, and — the case this spec exists for — the node
/// running on the bare fallback nobody chose must be distinguishable
/// too, as `"default_fallback"`, without requiring a run first.
#[tokio::test]
async fn graph_get_reports_prompt_source_for_agent_nodes() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut explicit_config = agent_node_config("claude");
    explicit_config.insert(
        "prompt_template".to_string(),
        serde_json::Value::String("do the thing".to_string()),
    );
    let explicit_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Explicit".to_string(),
                kind: Some("agent".to_string()),
                config: Some(explicit_config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap(),
        "node_id",
    );

    let mut preset_config = agent_node_config("claude");
    preset_config.insert(
        "prompt_preset".to_string(),
        serde_json::Value::String("implementer".to_string()),
    );
    let preset_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Preset".to_string(),
                kind: Some("agent".to_string()),
                config: Some(preset_config),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap(),
        "node_id",
    );

    let default_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Default".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("claude")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap(),
        "node_id",
    );

    let got = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    let nodes = json["specs"][0]["nodes"].as_array().unwrap();
    let prompt_source_of = |node_id: &str| -> String {
        nodes
            .iter()
            .find(|n| n["id"] == node_id)
            .unwrap_or_else(|| panic!("node {node_id} not found in {nodes:?}"))["prompt_source"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(prompt_source_of(&explicit_id), "explicit");
    assert_eq!(prompt_source_of(&preset_id), "preset");
    assert_eq!(prompt_source_of(&default_id), "default_fallback");
}

/// `graph_audit_node_configs` must find a node that predates write-time
/// validation and still carries a key its kind never reads — verified
/// against the exact incident shape (a `prompt` key on an agent node)
/// and resolving the owning graph through the node's spec, since a
/// spec-scoped node's own row has no `graph_id`.
#[tokio::test]
async fn graph_audit_node_configs_finds_node_with_ignored_prompt_key() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    // Bypass `graph_add_node`'s validation to simulate a node that was
    // written before this spec's check existed — exactly how graph
    // `824de730-7fec-4031-800a-7933d2cf94c1`'s node
    // `d0458fe0-24b8-4a29-8a69-7bb0e742e046` ended up carrying `prompt`.
    let legacy_node = GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Legacy Implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude", "prompt": "implement the spec"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&legacy_node).unwrap();

    // A clean node must not show up in the audit.
    let clean_node = insert_named_node(&db, &spec.id, "Clean", 2);

    let audited = handler.graph_audit_node_configs().await.unwrap();
    assert!(!is_err(&audited), "{}", text(&audited));
    let json: serde_json::Value = serde_json::from_str(&raw_text(&audited)).unwrap();
    let flagged = json["flagged_nodes"].as_array().unwrap();
    assert_eq!(flagged.len(), 1);
    let flagged_node = &flagged[0];
    assert_eq!(flagged_node["node_id"], legacy_node.id);
    assert_eq!(flagged_node["graph_id"], lp.id);
    assert_eq!(flagged_node["unknown_keys"], serde_json::json!(["prompt"]));
    assert!(flagged.iter().all(|n| n["node_id"] != clean_node.id));
}

#[tokio::test]
async fn graph_node_runs_list_defaults_to_most_recent_first_and_resolves_node_name() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node_a = insert_named_node(&db, &spec.id, "build", 1);
    let node_b = insert_named_node(&db, &spec.id, "review", 2);

    let now = chrono::Utc::now();
    let older = insert_finalized_node_run(
        &db,
        &lp.id,
        &spec.id,
        &node_a.id,
        GraphRunStatus::Pass,
        Some(serde_json::json!({"reported_output": "ok"})),
        now - chrono::Duration::minutes(10),
    );
    let newer = insert_finalized_node_run(
        &db,
        &lp.id,
        &spec.id,
        &node_b.id,
        GraphRunStatus::Fail,
        Some(serde_json::json!({"stderr": "Error: Unsupported model mimo-auto"})),
        now,
    );

    let listed = handler
        .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: None,
            node_id: None,
            limit: None,
            offset: None,
            compact: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body = raw_text(&listed);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let runs = parsed["runs"].as_array().unwrap();
    assert_eq!(
        runs[0]["id"], newer.id,
        "the most recently started run must lead the default listing"
    );
    assert_eq!(runs[1]["id"], older.id);
    assert_eq!(runs[0]["node_name"], "review");
    assert_eq!(runs[0]["status"], "fail");
    assert_eq!(runs[0]["session_id"], "ses_test_123");
    assert!(
        runs[0].get("output").is_none(),
        "list responses must omit output — fetch it via graph_node_run_get"
    );

    let unknown_graph = handler
        .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
            graph_id: "ghost".to_string(),
            spec_id: None,
            node_id: None,
            limit: None,
            offset: None,
            compact: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&unknown_graph));
}

#[tokio::test]
async fn graph_node_runs_list_filters_by_spec_and_node_and_respects_limit() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec1 = insert_test_spec(&db, &lp.id, 1);
    let spec2 = insert_test_spec(&db, &lp.id, 2);
    let node1 = insert_named_node(&db, &spec1.id, "node1", 1);
    let node2 = insert_named_node(&db, &spec2.id, "node2", 1);
    let now = chrono::Utc::now();
    insert_finalized_node_run(
        &db,
        &lp.id,
        &spec1.id,
        &node1.id,
        GraphRunStatus::Pass,
        None,
        now - chrono::Duration::minutes(2),
    );
    let spec2_run = insert_finalized_node_run(
        &db,
        &lp.id,
        &spec2.id,
        &node2.id,
        GraphRunStatus::Pass,
        None,
        now - chrono::Duration::minutes(1),
    );
    insert_finalized_node_run(
        &db,
        &lp.id,
        &spec1.id,
        &node1.id,
        GraphRunStatus::Fail,
        None,
        now,
    );

    let by_spec = handler
        .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: Some(spec2.id.clone()),
            node_id: None,
            limit: None,
            offset: None,
            compact: None,
        }))
        .await
        .unwrap();
    let by_spec_runs = serde_json::from_str::<serde_json::Value>(&raw_text(&by_spec)).unwrap()
        ["runs"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(by_spec_runs.len(), 1);
    assert_eq!(by_spec_runs[0]["id"], spec2_run.id);

    let limited = handler
        .graph_node_runs_list(Parameters(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: None,
            node_id: None,
            limit: Some(1),
            offset: None,
            compact: None,
        }))
        .await
        .unwrap();
    let limited_runs = serde_json::from_str::<serde_json::Value>(&raw_text(&limited)).unwrap()
        ["runs"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(limited_runs.len(), 1, "limit must bound the page size");
    assert_eq!(
        limited_runs[0]["status"], "fail",
        "the single returned run must be the most recent one"
    );
}

#[tokio::test]
async fn graph_node_run_get_returns_full_output_and_redacts_secrets() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node = insert_named_node(&db, &spec.id, "resilience", 1);
    let run = insert_finalized_node_run(
        &db,
        &lp.id,
        &spec.id,
        &node.id,
        GraphRunStatus::Fail,
        Some(serde_json::json!({
            "cli": "mimocode",
            "stderr": "Error: Unsupported model mimo-auto",
            "stdout": "leaked token=abcdefghij1234567890 in output",
            "infra_attempt": 1,
            "infra_crash": true,
        })),
        chrono::Utc::now(),
    );

    let got = handler
        .graph_node_run_get(Parameters(GraphNodeRunGetParams {
            run_id: run.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body = raw_text(&got);
    assert!(body.contains("Unsupported model mimo-auto"));
    assert!(body.contains("\"node_name\": \"resilience\""));
    assert!(
        body.contains("\"infra_attempt\": 1") && body.contains("\"infra_crash\": true"),
        "B19 infra markers must survive verbatim: {body}"
    );
    assert!(
        !body.contains("abcdefghij1234567890"),
        "a token-shaped value must be redacted, not passed through: {body}"
    );
    assert!(body.contains("[REDACTED]"));

    let missing = handler
        .graph_node_run_get(Parameters(GraphNodeRunGetParams {
            run_id: "ghost-run".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));
}

// ── graph_pause / graph_continue ────────────────────────────────

#[tokio::test]
async fn graph_pause_rejects_non_running_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let result = handler
        .graph_pause(Parameters(GraphPauseParams {
            graph_id: lp.id.clone(),
            interrupt: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("not running"));
}

#[tokio::test]
async fn graph_continue_requires_a_paused_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let not_paused = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "retry_current_node".to_string(),
            node_id: None,
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&not_paused));
    assert!(text(&not_paused).contains("not paused"));

    let missing = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: "ghost".to_string(),
            action: "retry_current_node".to_string(),
            node_id: None,
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));

    db.update_graph_status(&lp.id, GraphStatus::Paused, None, None)
        .unwrap();
    let bad_action = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "sideways".to_string(),
            node_id: None,
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&bad_action));
    assert!(text(&bad_action).contains("retry_current_node, skip_next_spec or run_from_node"));
}

// ── graph_continue run_from_node ───────────

#[tokio::test]
async fn graph_continue_run_from_node_refuses_a_graph_with_an_active_run() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    // A blocker can flip a graph to `paused` while a sibling node is
    // still executing — FR2 refuses a directed run there, naming the
    // run, whatever the status column says.
    let run = insert_running_node_run(&db, &lp.id);
    db.update_graph_status(&lp.id, GraphStatus::Paused, None, None)
        .unwrap();

    let refused = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "run_from_node".to_string(),
            node_id: Some("Node".to_string()),
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    let msg = text(&refused);
    assert!(msg.contains("still executing"), "{msg}");
    assert!(msg.contains(&run.id), "must name the run: {msg}");

    // A graph that is outright `running` (between nodes — no live run
    // row) also refuses.
    db.interrupt_graph_run(&run.id, "test").unwrap();
    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();
    let running = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "run_from_node".to_string(),
            node_id: Some("Node".to_string()),
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&running), "{}", text(&running));
    assert!(text(&running).contains("pause it before a directed run"));
}

#[tokio::test]
async fn graph_continue_run_from_node_accepts_spec_paused_on_and_reports_directed_plan() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    // The state `graph_pause` mid-spec leaves behind: graph paused, no
    // active run, paused-on spec reading `running`. FR2's "paused" is
    // exactly this — a directed run must accept it.
    insert_named_spec(&db, &lp.id, "Spec One", 1, GraphSpecStatus::Running);
    let skipped = insert_named_spec(&db, &lp.id, "Spec Two", 2, GraphSpecStatus::Skipped);
    db.update_graph_status(&lp.id, GraphStatus::Paused, None, None)
        .unwrap();

    // Top-level graph the specs fall back to: A --pass--> B (both quick
    // checks, so the background dispatch the accepted call spawns stays
    // harmless — under the plan's semantics every spec AFTER the
    // directed target also runs from Start, including the `Skipped` one).
    db.insert_graph_node(&GraphNode {
        id: "node-a".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "A".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-b".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "B".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0"
        }),
        position: 2,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_edge(&GraphEdge {
        id: "edge-a-b".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        from_node: "node-a".to_string(),
        to_node: "node-b".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();

    // A `skipped` target is the one spec status outside FR2's accepted
    // states.
    let refused = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "run_from_node".to_string(),
            node_id: Some("B".to_string()),
            spec_id: Some(skipped.id.clone()),
        }))
        .await
        .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    assert!(
        text(&refused).contains("'skipped', which a directed run does not accept"),
        "{}",
        text(&refused)
    );

    // An unknown node is refused with the list of valid names.
    let unknown = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "run_from_node".to_string(),
            node_id: Some("Zed".to_string()),
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&unknown), "{}", text(&unknown));
    let unknown_msg = text(&unknown);
    assert!(
        unknown_msg.contains("No node or ensemble matches 'Zed'"),
        "{unknown_msg}"
    );
    assert!(unknown_msg.contains("Valid nodes"), "{unknown_msg}");
    assert!(unknown_msg.contains("A, B"), "{unknown_msg}");

    // The paused-on spec is accepted (spec_id defaults to it), and the
    // response reports the whole directed plan.
    let accepted = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "run_from_node".to_string(),
            node_id: Some("B".to_string()),
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&accepted), "{}", text(&accepted));
    let msg = text(&accepted);
    assert!(msg.contains("resumed"), "{msg}");
    assert!(msg.contains("starts at node 'B'"), "{msg}");
    assert!(msg.contains("skipped A node(s)"), "{msg}");
    assert!(msg.contains("reused outputs: none"), "{msg}");
    assert!(msg.contains("iteration counters restarted at 1"), "{msg}");
}

#[tokio::test]
async fn graph_complete_node_records_result_and_rejects_stale_report() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let node_id = run.node_id.clone();

    let bad_status = handler
        .graph_complete_node(Parameters(GraphCompleteNodeParams {
            run_id: run.id.clone(),
            node_id: node_id.clone(),
            status: "sideways".to_string(),
            output: "out".to_string(),
            summary: "sum".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&bad_status));

    let ok = handler
        .graph_complete_node(Parameters(GraphCompleteNodeParams {
            run_id: run.id.clone(),
            node_id: node_id.clone(),
            status: "pass".to_string(),
            output: "out".to_string(),
            summary: "sum".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&ok), "{}", text(&ok));

    // Reporting again against the now-finalized run must be rejected as
    // stale, not silently accepted.
    let stale = handler
        .graph_complete_node(Parameters(GraphCompleteNodeParams {
            run_id: run.id.clone(),
            node_id: node_id.clone(),
            status: "pass".to_string(),
            output: "out2".to_string(),
            summary: "sum2".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&stale));
    assert!(text(&stale).contains("no longer active"));

    let wrong_node = handler
        .graph_complete_node(Parameters(GraphCompleteNodeParams {
            run_id: run.id,
            node_id: "not-the-right-node".to_string(),
            status: "pass".to_string(),
            output: "out".to_string(),
            summary: "sum".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&wrong_node));

    let unknown_run = handler
        .graph_complete_node(Parameters(GraphCompleteNodeParams {
            run_id: "ghost-run".to_string(),
            node_id,
            status: "pass".to_string(),
            output: "out".to_string(),
            summary: "sum".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&unknown_run));
}

#[tokio::test]
async fn graph_report_blocker_pauses_the_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let node_id = run.node_id.clone();

    let result = handler
        .graph_report_blocker(Parameters(GraphReportBlockerParams {
            run_id: run.id,
            node_id,
            description: "waiting on human input".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert_eq!(
        db.get_graph(&lp.id).unwrap().unwrap().status,
        GraphStatus::Paused
    );
}

#[tokio::test]
async fn loop_complete_node_records_result_like_graph_complete_node_and_notes_deprecation() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);

    let result = handler
        .loop_complete_node(Parameters(GraphCompleteNodeParams {
            run_id: run.id.clone(),
            node_id: run.node_id.clone(),
            status: "pass".to_string(),
            output: "out".to_string(),
            summary: "sum".to_string(),
        }))
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("loop_complete_node was renamed graph_complete_node in 3.0.0"));

    // Same DB effect as graph_complete_node: the run is finalized pass.
    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphRunStatus::Pass);
}

#[tokio::test]
async fn loop_report_blocker_pauses_the_graph_and_notes_deprecation() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);

    let result = handler
        .loop_report_blocker(Parameters(GraphReportBlockerParams {
            run_id: run.id,
            node_id: run.node_id,
            description: "waiting on human input".to_string(),
        }))
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    assert!(
        raw_text(&result).contains("loop_report_blocker was renamed graph_report_blocker in 3.0.0")
    );
    assert_eq!(
        db.get_graph(&lp.id).unwrap().unwrap().status,
        GraphStatus::Paused
    );
}

#[tokio::test]
async fn loop_schedule_autorun_accepts_loop_id_as_graph_id() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    // Deserialize straight from JSON using the wire field name `loop_id`,
    // proving the `#[serde(alias = "loop_id")]` on `graph_id` actually
    // works end to end, not just as a Rust-level convenience constructor.
    let raw = serde_json::json!({
        "loop_id": lp.id,
        "at": null,
        "quota_reset_message": null,
    });
    let params: LoopScheduleAutorunParams = serde_json::from_value(raw).unwrap();
    assert_eq!(params.graph_id, lp.id);

    let result = handler
        .loop_schedule_autorun(Parameters(params))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(raw_text(&result)
        .contains("loop_schedule_autorun was renamed graph_schedule_autorun in 3.0.0"));
}

#[test]
fn tool_listing_hides_the_three_deprecated_aliases_but_keeps_them_registered() {
    let visible = super::visible_tool_list();
    for hidden in [
        "loop_complete_node",
        "loop_report_blocker",
        "loop_schedule_autorun",
    ] {
        assert!(
            !visible.iter().any(|t| t.name == hidden),
            "{hidden} must not appear in the tool listing"
        );
    }
    // Still registered in the router (i.e. still callable) — just not listed.
    let all = TaskTriggerHandler::tool_router().list_all();
    for still_registered in [
        "loop_complete_node",
        "loop_report_blocker",
        "loop_schedule_autorun",
    ] {
        assert!(
            all.iter().any(|t| t.name == still_registered),
            "{still_registered} must remain registered in the router so call_tool still works"
        );
    }
    // A real graph_* tool must still be listed — the filter must be
    // exact-name, not a prefix/substring match that could eat something else.
    assert!(visible.iter().any(|t| t.name == "graph_complete_node"));
}

/// C19 FR4 (reusing the same blocker mechanism `graph_report_blocker`
/// uses): a graph paused with an active blocker must not be silently
/// relaunched via `graph_run` — that's the whole "not started again
/// until a human clears it" the budget exists for. A plain
/// `graph_pause` (`Paused`, no blocker) is unaffected — `graph_run`
/// resuming that is the normal, sanctioned path and must keep working.
#[tokio::test]
async fn graph_run_refuses_a_paused_graph_with_an_active_blocker() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let node_id = run.node_id.clone();

    let blocked = handler
        .graph_report_blocker(Parameters(GraphReportBlockerParams {
            run_id: run.id,
            node_id,
            description: "waiting on human input".to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&blocked), "{}", text(&blocked));

    let result = handler
        .graph_run(Parameters(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: None,
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "a blocked graph must refuse graph_run");
    assert!(text(&result).contains("blocked"), "{}", text(&result));
    assert_eq!(
        db.get_graph(&lp.id).unwrap().unwrap().status,
        GraphStatus::Paused,
        "the refusal must not itself change the graph's status"
    );
}

/// FR3: `graph_continue run_from_node` on an idea-driven graph that has
/// never run (top-level graph, no specs, no stored idea) is refused with an
/// error that names `graph_run` with an `idea` as the way to start it —
/// instead of leaking the bare "no spec to target" from spec resolution.
#[tokio::test]
async fn graph_continue_run_from_node_refuses_never_run_idea_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    // The idea-graph shape: a top-level graph, zero bound specs.
    db.insert_graph_node(&GraphNode {
        id: "node-check".to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "Check".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "true",
            "success_condition": "exit_code_0"
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let refused = handler
        .graph_continue(Parameters(GraphContinueParams {
            graph_id: lp.id.clone(),
            action: "run_from_node".to_string(),
            node_id: Some("Check".to_string()),
            spec_id: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    let msg = text(&refused);
    assert!(msg.contains("graph_run"), "must name graph_run: {msg}");
    assert!(msg.contains("idea"), "must name the idea: {msg}");
}
