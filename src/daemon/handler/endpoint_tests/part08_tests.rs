//! Part 08 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// CM30/FR3: `graph_get` reports the effective infra budget and its
/// source per node — `"node"` for an explicit key, `"default"` when
/// nothing (neither node nor platform) sets it.
#[tokio::test]
async fn graph_get_reports_infra_config_with_source() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut explicit_config = agent_node_config("cm30-test-platform-xyz");
    explicit_config.insert("infra_retry_limit".to_string(), serde_json::Value::from(5));
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
    let plain_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Plain".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("cm30-test-platform-xyz")),
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
    let infra_of = |node_id: &str| -> serde_json::Value {
        nodes
            .iter()
            .find(|n| n["id"] == node_id)
            .unwrap_or_else(|| panic!("node {node_id} not found in {nodes:?}"))["infra_config"]
            .clone()
    };
    let explicit = infra_of(&explicit_id);
    assert_eq!(explicit["infra_retry_limit"]["value"], 5);
    assert_eq!(explicit["infra_retry_limit"]["source"], "node");
    assert_eq!(explicit["infra_crash_max_seconds"]["value"], 60);
    assert_eq!(explicit["infra_crash_max_seconds"]["source"], "default");
    let plain = infra_of(&plain_id);
    assert_eq!(plain["infra_retry_limit"]["value"], 2);
    assert_eq!(plain["infra_retry_limit"]["source"], "default");
    assert_eq!(plain["infra_backoff_seconds"]["value"], 30);
    assert_eq!(plain["infra_backoff_seconds"]["source"], "default");
}

/// CM30/FR5: `graph_audit_node_configs` lists a node/member whose
/// effective budget differs from its platform's own, and omits one
/// whose override merely restates the platform value (or that sets
/// nothing). The platform here is unconfigured, so its baseline is the
/// engine default.
#[tokio::test]
async fn graph_audit_node_configs_lists_infra_overrides() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut differing = agent_node_config("cm30-test-platform-xyz");
    differing.insert("infra_retry_limit".to_string(), serde_json::Value::from(0));
    let differing_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Differing".to_string(),
                kind: Some("agent".to_string()),
                config: Some(differing),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap(),
        "node_id",
    );
    let mut restating = agent_node_config("cm30-test-platform-xyz");
    restating.insert("infra_retry_limit".to_string(), serde_json::Value::from(2));
    let restating_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Restating".to_string(),
                kind: Some("agent".to_string()),
                config: Some(restating),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap(),
        "node_id",
    );
    let plain_id = extract_id(
        &handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: "Plain".to_string(),
                kind: Some("agent".to_string()),
                config: Some(agent_node_config("cm30-test-platform-xyz")),
                blueprint: None,
                config_overrides: None,
            }))
            .await
            .unwrap(),
        "node_id",
    );

    let audited = handler.graph_audit_node_configs().await.unwrap();
    assert!(!is_err(&audited), "{}", text(&audited));
    let json: serde_json::Value = serde_json::from_str(&raw_text(&audited)).unwrap();
    let overrides = json["infra_config_overrides"].as_array().unwrap();
    assert_eq!(overrides.len(), 1, "{overrides:?}");
    assert_eq!(overrides[0]["node_id"], differing_id);
    assert_eq!(overrides[0]["infra_retry_limit"]["value"], 0);
    assert_eq!(overrides[0]["infra_retry_limit"]["source"], "node");
    assert_eq!(overrides[0]["infra_retry_limit"]["platform_value"], 2);
    assert!(overrides.iter().all(|n| n["node_id"] != restating_id));
    assert!(overrides.iter().all(|n| n["node_id"] != plain_id));
}

/// CB40: shrinking members below the stored `min_pass` is refused and
/// writes nothing — the resize must not be committed.
#[tokio::test]
async fn graph_update_ensemble_shrink_below_min_pass_is_refused() {
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
            name: "Shrink Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(vec![member(), member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: Some(3),
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

    let shrunk = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: Some(vec![member(), member()]),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&shrunk), "shrink below min_pass should fail");
    let msg = text(&shrunk);
    assert!(msg.contains("has an invalid min_pass"), "{msg}");
    assert!(msg.contains('3'), "{msg}");
    assert!(msg.contains('2'), "{msg}");
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.members.len(), 3);
    assert_eq!(details.ensemble.min_pass, 3);
}

/// CB40: raising `min_pass` above the member count is refused.
#[tokio::test]
async fn graph_update_ensemble_raise_min_pass_above_count_is_refused() {
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
            name: "Raise Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(vec![member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: Some(2),
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

    let raised = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
            min_pass: Some(99),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&raised), "raised min_pass should fail");
    assert!(
        text(&raised).contains("has an invalid min_pass"),
        "{}",
        text(&raised)
    );
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.min_pass, 2);
}

/// CB40: a shrink that also lowers `min_pass` in the same call succeeds.
#[tokio::test]
async fn graph_update_ensemble_shrink_with_adjusted_min_pass_succeeds() {
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
            name: "Adjust Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(vec![member(), member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: Some(2),
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

    let shrunk = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: Some(vec![member(), member()]),
            min_pass: Some(1),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&shrunk), "{}", text(&shrunk));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.members.len(), 2);
    assert_eq!(details.ensemble.min_pass, 1);
}

/// CB40: `graph_preflight` names an ensemble stored with `min_pass`
/// above its member count (simulating a row predating validation).
#[tokio::test]
async fn graph_preflight_reports_ensemble_with_invalid_min_pass() {
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
            name: "Stale Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(vec![member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: Some(2),
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
    db.update_ensemble_join_config(&ensemble_id, Some(3), None, None, None, None, None, None)
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "preflight should flag invalid min_pass");
    let msg = text(&result);
    assert!(msg.contains("Stale Ensemble"), "{msg}");
    assert!(msg.contains("invalid min_pass"), "{msg}");
}

#[tokio::test]
async fn graph_preflight_rejects_unsafe_optional_model_token() {
    let home = tempdir().unwrap();
    let canopy_dir = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy_dir).unwrap();
    let config = crate::domain::canopy_config::CanopyConfig {
        clis: vec![crate::domain::cli_config::CliConfig {
            name: "bad-template-cli".to_string(),
            binary: "/bin/echo".to_string(),
            invocation_template: Some("-m {{model?}}#{{effort?}} {{prompt}}".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    config.save_for_tests(&canopy_dir).unwrap();
    let _home_guard = HomeVar::set(home.path());

    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Entry".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("bad-template-cli")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(msg.contains("bad-template-cli"), "{msg}");
    assert!(msg.contains("{{model?}}"), "{msg}");
}

/// CM35 FR2: `graph_preflight` flags every existing node that points at
/// an interactive-only platform — advisory (never a tool error), naming
/// the node through `used_by` — and that pair's probe is
/// `interactive_only` with nothing spawned. The node is written straight
/// to the DB because `graph_add_node` now refuses this platform: this is
/// how a node created before the gate reaches preflight.
#[tokio::test]
async fn graph_preflight_flags_interactive_only_nodes() {
    let home = tempdir().unwrap();
    let marker = write_freebuff_config(home.path());
    let _home_guard = HomeVar::set(home.path());

    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    db.insert_graph_node(&GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "Live Only".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "freebuff"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }))
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let warnings = body["interactive_only_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("expected interactive_only_warnings: {body}"));
    assert_eq!(warnings.len(), 1, "{body}");
    assert_eq!(
        warnings[0]["platform"],
        serde_json::json!("freebuff"),
        "{body}"
    );
    assert_eq!(
        warnings[0]["used_by"],
        serde_json::json!(["node: Live Only"]),
        "{body}"
    );
    assert!(
        warnings[0]["warning"].as_str().is_some_and(
            |w| w.contains("platform 'freebuff' is interactive-only (no headless mode)")
        ),
        "{body}"
    );
    let probe = &body["probes"][0];
    assert_eq!(probe["platform"], serde_json::json!("freebuff"), "{body}");
    assert_eq!(
        probe["outcome"],
        serde_json::json!("interactive_only"),
        "{body}"
    );
    assert!(!marker.exists(), "preflight must not spawn a process");
}

#[tokio::test]
async fn graph_preflight_names_ensemble_as_commit_rights_holder() {
    // NFR1(e): preflight's `commit_rights_holders` names the ensemble
    // (kind + name), not its members, as the graph's committer.
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Committer Ensemble", &entry, &arbiter).await;
    db.update_ensemble_commit_rights(&ensemble_id, true)
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        !is_err(&result),
        "preflight must succeed: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let holders = body["commit_rights_holders"].as_array().unwrap();
    assert!(
        holders
            .iter()
            .any(|h| h["kind"] == "ensemble" && h["name"] == "Committer Ensemble"),
        "preflight must name the ensemble as committer: {body}"
    );
}

#[tokio::test]
async fn graph_preflight_warns_on_stale_loop_complete_node_in_ensemble_prompt() {
    // Build a graph with one ensemble whose shared prompt_template still
    // instructs the 2.x tool name — mirrors the CB68 objective's kilo
    // run fff7f1aa scenario (Implementer/Committer ensemble prompt).
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
            name: "Stale Ensemble".to_string(),
            kind: None,
            prompt_template: Some("end with exactly one loop_complete_node call".to_string()),
            members: Some(vec![member(), member()]),
            blueprint: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: Some(2),
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

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }))
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let warnings = parsed["stale_name_warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w["kind"] == "ensemble"
            && w["new_name"] == "graph_complete_node"
            && w["old_name"] == "loop_complete_node"),
        "{body}"
    );
}

#[tokio::test]
async fn graph_preflight_reports_no_stale_name_warnings_on_clean_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    add_two_member_ensemble(&handler, &spec.id, "Clean Ensemble", &entry, &arbiter).await;

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["stale_name_warnings"],
        serde_json::json!([]),
        "{body}"
    );
}

#[tokio::test]
async fn graph_preflight_hook_warning_names_the_firing_event() {
    // Same clean-graph fixture as above plus one stale agent hook on
    // `on_failed`: if the hook scan were missing, `stale_name_warnings`
    // stays empty and this fails; and a warning that labels every hook a
    // generic "completion hook" fails the `on_failed` assertions.
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    add_two_member_ensemble(&handler, &spec.id, "Clean Ensemble", &entry, &arbiter).await;

    let mut hooks = std::collections::BTreeMap::new();
    hooks.insert(
        crate::domain::graphs::GraphHookEvent::OnFailed,
        vec![crate::domain::graphs::GraphCompletionHook {
            platform: Some("claude".to_string()),
            model: None,
            effort: None,
            prompt: Some("on failure, call loop_report_blocker with details".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: None,
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,

            crew: None,
        }],
    );
    db.update_graph_hooks(&lp.id, &hooks).unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let warnings = parsed["stale_name_warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w["kind"] == "hook"
            && w["event"] == "on_failed"
            && w["old_name"] == "loop_report_blocker"
            && w["new_name"] == "graph_report_blocker"
            && w["warning"]
                .as_str()
                .is_some_and(|t| t.contains("on_failed hook"))),
        "{body}"
    );
}

/// `from_node` moves every entry edge to the new source with the new
/// condition and leaves no edge from the old source; a failed rewire
/// leaves the previous edges (and row) intact.
#[tokio::test]
async fn graph_update_ensemble_from_node_replaces_every_entry_edge() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let old_entry = add_agent_node(&handler, &spec.id, "OldEntry").await;
    let new_entry = add_agent_node(&handler, &spec.id, "NewEntry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Workers", &old_entry, &arbiter).await;
    let member_ids: Vec<String> = db
        .get_ensemble_details(&ensemble_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();

    let mut update = blank_ensemble_update(&ensemble_id);
    update.from_node = Some(new_entry.clone());
    update.condition = Some("pass".to_string());
    let rewired = handler
        .graph_update_ensemble(Parameters(update))
        .await
        .unwrap();
    assert!(!is_err(&rewired), "{}", text(&rewired));

    let edges = db.list_graph_edges(&spec.id).unwrap();
    assert!(
        !edges
            .iter()
            .any(|e| e.from_node == old_entry && member_ids.contains(&e.to_node)),
        "no entry edge may remain from the old source"
    );
    for member_id in &member_ids {
        assert!(
            edges.iter().any(|e| e.from_node == new_entry
                && e.to_node == *member_id
                && e.condition == GraphEdgeCondition::Pass),
            "new source must reach every member with the new condition"
        );
    }
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.entry_from_node, new_entry);
    assert_eq!(details.ensemble.entry_condition, GraphEdgeCondition::Pass);

    // A failed rewire (unknown source) changes nothing.
    let mut bad_update = blank_ensemble_update(&ensemble_id);
    bad_update.from_node = Some("ghost-node".to_string());
    let failed = handler
        .graph_update_ensemble(Parameters(bad_update))
        .await
        .unwrap();
    assert!(is_err(&failed));
    let edges_after = db.list_graph_edges(&spec.id).unwrap();
    assert_eq!(edges_after.len(), edges.len());
    for member_id in &member_ids {
        assert!(edges_after
            .iter()
            .any(|e| e.from_node == new_entry && e.to_node == *member_id));
    }
    let details_after = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details_after.ensemble.entry_from_node, new_entry);
}

/// An ensemble entered from three different nodes carries entry edges
/// from each of them to every member, and the graph still validates —
/// the review-graph shape (designer, failing gate, reviewer bouncing
/// back) with no relay node.
#[tokio::test]
async fn graph_update_ensemble_multi_source_entry_reaches_every_member() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let designer = add_agent_node(&handler, &spec.id, "Designer").await;
    let gate = add_agent_node(&handler, &spec.id, "Gate").await;
    let reviewer = add_agent_node(&handler, &spec.id, "Reviewer").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Implementers", &designer, &arbiter).await;
    let member_ids: Vec<String> = db
        .get_ensemble_details(&ensemble_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();

    for source in [&gate, &reviewer] {
        let mut update = blank_ensemble_update(&ensemble_id);
        update.add_entry_from = Some(source.clone());
        let added = handler
            .graph_update_ensemble(Parameters(update))
            .await
            .unwrap();
        assert!(!is_err(&added), "{}", text(&added));
    }

    let edges = db.list_graph_edges(&spec.id).unwrap();
    for source in [&designer, &gate, &reviewer] {
        for member_id in &member_ids {
            assert!(
                edges.iter().any(|e| e.from_node == *source
                    && e.to_node == *member_id
                    && e.condition == GraphEdgeCondition::Always),
                "source '{source}' must reach member '{member_id}'"
            );
        }
    }
    // The primary entry is untouched by additive wiring.
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.entry_from_node, designer);

    // The multi-entry graph validates as a unit.
    let ensembles = db.list_ensembles_for_spec(&spec.id).unwrap();
    let nodes = db.list_graph_nodes(&spec.id).unwrap();
    crate::domain::validation::validate_ensembles_in_graph(&ensembles, &nodes, &edges)
        .expect("three-source entry must validate");

    // `graph_get` shows every entry source, not just the primary.
    let got = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    let ensembles_json = json["specs"][0]["ensembles"].as_array().unwrap();
    assert_eq!(ensembles_json.len(), 1);
    let sources = ensembles_json[0]["entry_sources"].as_array().unwrap();
    assert_eq!(sources.len(), 3);
}

/// Detaching one entry source removes only its edges; detaching the
/// primary promotes another source; the last source cannot be detached.
#[tokio::test]
async fn graph_update_ensemble_entry_remove_detaches_one_source() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let first = add_agent_node(&handler, &spec.id, "First").await;
    let second = add_agent_node(&handler, &spec.id, "Second").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Workers", &first, &arbiter).await;
    let member_ids: Vec<String> = db
        .get_ensemble_details(&ensemble_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();

    let mut add = blank_ensemble_update(&ensemble_id);
    add.add_entry_from = Some(second.clone());
    assert!(!is_err(
        &handler
            .graph_update_ensemble(Parameters(add))
            .await
            .unwrap()
    ));

    // Detach the primary: the other source is promoted in its place.
    let mut remove = blank_ensemble_update(&ensemble_id);
    remove.remove_entry_from = Some(first.clone());
    let removed = handler
        .graph_update_ensemble(Parameters(remove))
        .await
        .unwrap();
    assert!(!is_err(&removed), "{}", text(&removed));
    let edges = db.list_graph_edges(&spec.id).unwrap();
    assert!(!edges.iter().any(|e| e.from_node == first));
    for member_id in &member_ids {
        assert!(edges
            .iter()
            .any(|e| e.from_node == second && e.to_node == *member_id));
    }
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.entry_from_node, second);

    // The last source cannot be detached — and the refusal changes nothing.
    let edge_count = edges.len();
    let mut remove_last = blank_ensemble_update(&ensemble_id);
    remove_last.remove_entry_from = Some(second.clone());
    let refused = handler
        .graph_update_ensemble(Parameters(remove_last))
        .await
        .unwrap();
    assert!(is_err(&refused));
    assert!(text(&refused).contains("last entry source"));
    assert_eq!(db.list_graph_edges(&spec.id).unwrap().len(), edge_count);
}

/// CB54 FR5/FR6a (handler level, not just validate_ensembles_in_graph):
/// growing then shrinking a two-source ensemble keeps `graph_run` accepting
/// it at every member count, through the real graph_update_ensemble +
/// graph_run tool path, with no manual rewiring in between.
///
/// Uses three independent graphs (one per step) rather than three
/// sequential `graph_run` calls on one graph: `graph_run` returns as soon
/// as it spawns the background run task, so calling it twice on the same
/// graph_id would race a real background run against the next resize.
/// Three independent graphs keep each `graph_run` call deterministic
/// while still exercising resize-then-launch for every member count
/// through the real tool path.
#[tokio::test]
async fn graph_run_accepts_two_source_ensemble_through_grow_and_shrink() {
    let (_dir, db, handler) = endpoint_test_handler();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());

    async fn run_and_assert_accepted(
        db: &Database,
        handler: &TaskTriggerHandler,
        git_dir: &std::path::Path,
        member_count: usize,
        resize_to: Option<usize>,
        resize_again_to: Option<usize>,
    ) {
        let member = || crate::daemon::params::EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        let (lp, spec, ensemble_id, entry, alt_entry) =
            build_two_source_ensemble_graph(db, handler, git_dir, member_count).await;
        for target in [resize_to, resize_again_to].into_iter().flatten() {
            let mut update = blank_ensemble_update(&ensemble_id);
            update.members = Some((0..target).map(|_| member()).collect());
            let resized = handler
                .graph_update_ensemble(Parameters(update))
                .await
                .unwrap();
            assert!(!is_err(&resized), "resize to {target}: {}", text(&resized));
        }
        let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
        let final_count = resize_again_to.or(resize_to).unwrap_or(member_count);
        assert_eq!(details.members.len(), final_count);
        let member_ids: std::collections::HashSet<_> =
            details.members.iter().map(|m| m.node_id.as_str()).collect();
        let entry_edges = db
            .list_graph_edges(&spec.id)
            .unwrap()
            .into_iter()
            .filter(|e| {
                [entry.as_str(), alt_entry.as_str()].contains(&e.from_node.as_str())
                    && member_ids.contains(e.to_node.as_str())
            })
            .count();
        assert_eq!(entry_edges, 2 * final_count);

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
        assert!(
            !is_err(&result),
            "graph_run must accept a fully-fanned-out {final_count}-member ensemble: {}",
            text(&result)
        );
    }

    // Step 1: freshly built at 3 members, 2 sources.
    run_and_assert_accepted(&db, &handler, git_dir.path(), 3, None, None).await;
    // Step 2: built at 3, grown to 4.
    run_and_assert_accepted(&db, &handler, git_dir.path(), 3, Some(4), None).await;
    // Step 3: built at 3, grown to 4, shrunk back to 3.
    run_and_assert_accepted(&db, &handler, git_dir.path(), 3, Some(4), Some(3)).await;
}

/// CB54 FR3/FR6b (handler level): a source that already reaches some but
/// not all members — e.g. after a hand edit deleted one of its edges — is
/// completed by add_entry_from, not refused as "already an entry source".
#[tokio::test]
async fn graph_update_ensemble_add_entry_from_completes_partial_source() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let alt_entry = add_agent_node(&handler, &spec.id, "AltEntry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Workers", &entry, &arbiter).await;

    let mut add_alt = blank_ensemble_update(&ensemble_id);
    add_alt.add_entry_from = Some(alt_entry.clone());
    let added = handler
        .graph_update_ensemble(Parameters(add_alt))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let member_ids: Vec<String> = db
        .get_ensemble_details(&ensemble_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();
    assert_eq!(member_ids.len(), 2);

    // Hand-delete alt_entry's edge to the first member only — simulating the
    // exact partial state CB54 is about (some, not all, members reachable).
    let stale_edge = db
        .list_graph_edges(&spec.id)
        .unwrap()
        .into_iter()
        .find(|e| e.from_node == alt_entry && e.to_node == member_ids[0])
        .expect("alt_entry -> first member edge must exist before deletion");
    assert!(db.delete_graph_edge(&stale_edge.id).unwrap());

    // Re-adding the same source must complete the missing edge, not refuse
    // with "already an entry source".
    let mut repair = blank_ensemble_update(&ensemble_id);
    repair.add_entry_from = Some(alt_entry.clone());
    let repaired = handler
        .graph_update_ensemble(Parameters(repair))
        .await
        .unwrap();
    assert!(
        !is_err(&repaired),
        "add_entry_from must complete a partial source, not refuse it: {}",
        text(&repaired)
    );

    let edges = db.list_graph_edges(&spec.id).unwrap();
    for member_id in &member_ids {
        assert!(
            edges.iter().any(|e| e.from_node == alt_entry
                && &e.to_node == member_id
                && e.condition == GraphEdgeCondition::Always),
            "alt_entry must reach member '{member_id}' after the repair"
        );
    }
}

/// CB54 FR4/FR6c (handler level): graph_preflight refuses a partially-wired
/// multi-source ensemble (a source that reaches some but not all members)
/// before spending any probe quota, naming the source and the exact reach
/// count.
#[tokio::test]
async fn graph_preflight_refuses_partial_multi_source_ensemble() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let alt_entry = add_agent_node(&handler, &spec.id, "AltEntry").await;
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
            name: "Proposers".to_string(),
            kind: None,
            prompt_template: Some("Draft it".to_string()),
            members: Some(vec![member(), member(), member()]),
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
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");

    let mut add_alt = blank_ensemble_update(&ensemble_id);
    add_alt.add_entry_from = Some(alt_entry.clone());
    let added = handler
        .graph_update_ensemble(Parameters(add_alt))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let member_ids: Vec<String> = db
        .get_ensemble_details(&ensemble_id)
        .unwrap()
        .unwrap()
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();
    assert_eq!(member_ids.len(), 3);

    // Hand-break alt_entry's wiring to exactly 2 of 3 members.
    let stale_edge = db
        .list_graph_edges(&spec.id)
        .unwrap()
        .into_iter()
        .find(|e| e.from_node == alt_entry && e.to_node == member_ids[2])
        .expect("alt_entry -> third member edge must exist before deletion");
    assert!(db.delete_graph_edge(&stale_edge.id).unwrap());

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "partial multi-source ensemble must refuse preflight"
    );
    let msg = text(&result);
    assert!(
        msg.contains(&alt_entry),
        "message must name the partial source: {msg}"
    );
    assert!(msg.contains("reaches 2 of 3 members"), "{msg}");
}

/// `graph_delete_ensemble` removes members, quorum and all their edges as
/// one unit, leaves the rest of the graph intact, and strands no edge.
#[tokio::test]
async fn graph_delete_ensemble_removes_members_quorum_and_edges() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id = add_two_member_ensemble(&handler, &spec.id, "Doomed", &entry, &arbiter).await;
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    let owned: Vec<String> = details
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .chain(std::iter::once(details.ensemble.join_node_id.clone()))
        .collect();

    let deleted = handler
        .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
            ensemble_id: ensemble_id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));

    for node_id in &owned {
        assert!(db.get_graph_node(node_id).unwrap().is_none());
    }
    assert!(db.get_ensemble_details(&ensemble_id).unwrap().is_none());
    // The surrounding graph survives.
    assert!(db.get_graph_node(&entry).unwrap().is_some());
    assert!(db.get_graph_node(&arbiter).unwrap().is_some());
    // No dangling edge may name a deleted node.
    for edge in db.list_graph_edges(&spec.id).unwrap() {
        assert!(
            !owned.contains(&edge.from_node) && !owned.contains(&edge.to_node),
            "dangling edge after delete: {edge:?}"
        );
        assert!(db.get_graph_node(&edge.from_node).unwrap().is_some());
        assert!(db.get_graph_node(&edge.to_node).unwrap().is_some());
    }

    // Deleting twice reports not-found instead of succeeding silently.
    let again = handler
        .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams { ensemble_id }))
        .await
        .unwrap();
    assert!(is_err(&again));
}

/// Deleting an ensemble while its graph runs is refused and changes nothing.
#[tokio::test]
async fn graph_delete_ensemble_refused_while_graph_running() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id = add_two_member_ensemble(&handler, &spec.id, "Busy", &entry, &arbiter).await;
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    let member_id = details.members[0].node_id.clone();

    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();
    let refused = handler
        .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
            ensemble_id: ensemble_id.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&refused));
    assert!(text(&refused).contains("running"), "{}", text(&refused));
    assert!(db.get_ensemble_details(&ensemble_id).unwrap().is_some());
    assert!(db.get_graph_node(&member_id).unwrap().is_some());

    // Entry rewiring is a topology change too — refused while running.
    let mut update = blank_ensemble_update(&ensemble_id);
    update.from_node = Some(entry.clone());
    let rewired = handler
        .graph_update_ensemble(Parameters(update))
        .await
        .unwrap();
    assert!(is_err(&rewired));
    assert!(text(&rewired).contains("running"), "{}", text(&rewired));
}

/// A quorum routes into another ensemble with no intermediate node: the
/// node count does not grow, the join fans out to every member of the
/// target, and the target cannot be deleted while chained into.
#[tokio::test]
async fn graph_update_ensemble_chains_quorum_into_another_ensemble() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let designer = add_agent_node(&handler, &spec.id, "Designer").await;
    let gate = add_agent_node(&handler, &spec.id, "Gate").await;
    let end = add_agent_node(&handler, &spec.id, "End").await;
    let implementers =
        add_two_member_ensemble(&handler, &spec.id, "Implementers", &designer, &end).await;
    let reviewers = add_two_member_ensemble(&handler, &spec.id, "Reviewers", &gate, &end).await;
    let impl_details = db.get_ensemble_details(&implementers).unwrap().unwrap();
    let review_details = db.get_ensemble_details(&reviewers).unwrap().unwrap();
    let review_member_ids: Vec<String> = review_details
        .members
        .iter()
        .map(|m| m.node_id.clone())
        .collect();
    let node_count = db.list_graph_nodes(&spec.id).unwrap().len();

    // Chain: implementers' quorum passes straight into the reviewers.
    let mut chain = blank_ensemble_update(&implementers);
    chain.on_pass_to = Some(reviewers.clone());
    let chained = handler
        .graph_update_ensemble(Parameters(chain))
        .await
        .unwrap();
    assert!(!is_err(&chained), "{}", text(&chained));

    // No relay node was created for the chain.
    assert_eq!(db.list_graph_nodes(&spec.id).unwrap().len(), node_count);
    let edges = db.list_graph_edges(&spec.id).unwrap();
    for member_id in &review_member_ids {
        assert!(
            edges
                .iter()
                .any(|e| e.from_node == impl_details.ensemble.join_node_id
                    && e.to_node == *member_id
                    && e.condition == GraphEdgeCondition::Pass),
            "quorum must fan out to every member of the chained ensemble"
        );
    }
    let chained_details = db.get_ensemble_details(&implementers).unwrap().unwrap();
    assert_eq!(
        chained_details.ensemble.on_pass_to,
        review_details.ensemble.join_node_id
    );
    crate::domain::validation::validate_ensembles_in_graph(
        &db.list_ensembles_for_spec(&spec.id).unwrap(),
        &db.list_graph_nodes(&spec.id).unwrap(),
        &edges,
    )
    .expect("chained ensembles must validate");

    // The chained-into ensemble cannot be deleted while targeted.
    let blocked = handler
        .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
            ensemble_id: reviewers.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&blocked));
    assert!(text(&blocked).contains("routes its exit into it"));

    // Unchain, then the deletion goes through.
    let mut unchain = blank_ensemble_update(&implementers);
    unchain.on_pass_to = Some(end.clone());
    let unchained = handler
        .graph_update_ensemble(Parameters(unchain))
        .await
        .unwrap();
    assert!(!is_err(&unchained), "{}", text(&unchained));
    let deleted = handler
        .graph_delete_ensemble(Parameters(GraphDeleteEnsembleParams {
            ensemble_id: reviewers.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    assert!(db.get_ensemble_details(&reviewers).unwrap().is_none());
}
