//! Part 07 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// The defect this closes (2026-08-06, graph 824de730): a spec that dies
/// because a FAILING node has no outgoing edge left no blocker anywhere
/// a human could see from `graph` — the engine now derives one onto
/// the terminating run (`GraphEngine::record_terminal_blocker`), and
/// `graph` picks it up through the unchanged `graph_run_blocker`
/// read path, with no MCP-side change required.
#[tokio::test]
async fn graph_list_reports_blocked_for_a_spec_that_dead_ends_on_a_failing_node() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    db.insert_graph_node(&GraphNode {
        id: "dead-end".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "dead-end".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({
            "command": "exit 1",
            "success_condition": "exit_code_0",
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    // No outgoing edge from "dead-end" for either status — the spec
    // terminates right there.

    handler
        .graph_engine
        .run_graph(lp.id.clone(), None, None, None, None, None)
        .await
        .unwrap();

    assert_eq!(
        db.get_graph_spec(&spec.id).unwrap().unwrap().status,
        GraphSpecStatus::Failed
    );

    let listed = call_graph_action(
        &handler,
        "graph",
        "list",
        params_id(GraphListParams {
            workdir: None,
            include_archived: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    let entry = value
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == lp.id)
        .expect("graph must appear in graph");
    assert_eq!(entry["blocked"], serde_json::json!(true));
    let blocker = entry["blocker"].as_str().expect("blocker must be a string");
    assert!(blocker.contains("dead-end"), "blocker: {blocker}");
}

// ── graph_node / graph_ensemble ───────────────────────

#[tokio::test]
async fn graph_copy_node_unwired_and_wired() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let source = add_agent_node(&handler, &spec.id, "Source").await;
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let exit = add_agent_node(&handler, &spec.id, "Exit").await;

    let unwired = call_graph_action(
        &handler,
        "graph_node",
        "copy",
        params_id(GraphCopyNodeParams {
            source_node_id: source.clone(),
            spec_id: None,
            graph_id: None,
            name: Some("Source Copy".to_string()),
            config_overrides: None,
            entry_from_node: None,
            entry_condition: None,
            on_pass_to: None,
            on_fail_to: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&unwired), "{}", text(&unwired));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&unwired)).unwrap();
    assert_eq!(value["wired"], false);
    assert_eq!(db.list_graph_nodes(&spec.id).unwrap().len(), 4);

    let wired = call_graph_action(
        &handler,
        "graph_node",
        "copy",
        params_id(GraphCopyNodeParams {
            source_node_id: source,
            spec_id: None,
            graph_id: None,
            name: Some("Source Copy 2".to_string()),
            config_overrides: None,
            entry_from_node: Some(entry),
            entry_condition: Some("always".to_string()),
            on_pass_to: Some(exit),
            on_fail_to: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&wired), "{}", text(&wired));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&wired)).unwrap();
    assert_eq!(value["wired"], true);

    let missing_source = call_graph_action(
        &handler,
        "graph_node",
        "copy",
        params_id(GraphCopyNodeParams {
            source_node_id: "ghost-node".to_string(),
            spec_id: None,
            graph_id: None,
            name: None,
            config_overrides: None,
            entry_from_node: None,
            entry_condition: None,
            on_pass_to: None,
            on_fail_to: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing_source));
}

#[tokio::test]
async fn graph_copy_ensemble_duplicates_members_and_quorum() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Source Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review {{spec_name}}".to_string()),
            members: Some(vec![
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
            ]),
            blueprint: None,
            node: None,
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
        }),
    )
    .await
    .unwrap();
    let source_ensemble_id = extract_id(&created, "ensemble_id");

    let copied = call_graph_action(
        &handler,
        "graph_ensemble",
        "copy",
        params_id(GraphCopyEnsembleParams {
            source_ensemble_id: source_ensemble_id.clone(),
            spec_id: None,
            graph_id: None,
            name: Some("Copied Ensemble".to_string()),
            prompt_template: None,
            members: None,
            min_pass: None,
            timeout_minutes: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            from_node: None,
            condition: None,
            on_pass_to: None,
            on_fail_to: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&copied), "{}", text(&copied));
    let copied_ensemble_id = extract_id(&copied, "ensemble_id");
    assert_ne!(copied_ensemble_id, source_ensemble_id);
    let copied_details = db
        .get_ensemble_details(&copied_ensemble_id)
        .unwrap()
        .unwrap();
    assert_eq!(copied_details.ensemble.name, "Copied Ensemble");
    assert_eq!(copied_details.members.len(), 2);

    let missing_source = call_graph_action(
        &handler,
        "graph_ensemble",
        "copy",
        params_id(GraphCopyEnsembleParams {
            source_ensemble_id: "ghost-ensemble".to_string(),
            spec_id: None,
            graph_id: None,
            name: None,
            prompt_template: None,
            members: None,
            min_pass: None,
            timeout_minutes: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            from_node: None,
            condition: None,
            on_pass_to: None,
            on_fail_to: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing_source));
}

/// A member's `prompt_override` replaces the shared `prompt_template` for
/// that member only — everything else (platform/model semantics, the
/// other members, `graph`'s prompt_source, and `graph_ensemble`
/// carrying it forward) keeps working exactly as before this field
/// existed.
#[tokio::test]
async fn graph_add_ensemble_member_prompt_override_replaces_shared_prompt_for_that_member_only() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Panel".to_string(),
            kind: None,
            prompt_template: Some("shared review prompt".to_string()),
            members: Some(vec![
                crate::daemon::params::EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: Some("review for security issues".to_string()),
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                crate::daemon::params::EnsembleMemberParams {
                    platform: "codex".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            blueprint: None,
            node: None,
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
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();

    // Platform/model are untouched by the override — it's an independent
    // knob, not a replacement for them.
    assert_eq!(details.members[0].platform, "claude");
    assert_eq!(
        details.members[0].prompt_override.as_deref(),
        Some("review for security issues")
    );
    assert_eq!(details.members[1].prompt_override, None);

    let overridden_node = db
        .get_graph_node(&details.members[0].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        overridden_node
            .config
            .get("prompt_template")
            .and_then(|v| v.as_str()),
        Some("review for security issues")
    );
    let shared_node = db
        .get_graph_node(&details.members[1].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        shared_node
            .config
            .get("prompt_template")
            .and_then(|v| v.as_str()),
        Some("shared review prompt")
    );

    // graph's ensemble view distinguishes shared vs. override per
    // member without the caller having to diff node config themselves.
    let spec_edges = db.list_graph_edges(&spec.id).unwrap();
    let ensembles_json = crate::daemon::handler::ensemble_details_json(&details, &spec_edges);
    let members_json = ensembles_json["members"].as_array().unwrap();
    assert_eq!(members_json[0]["prompt_source"], "override");
    assert_eq!(members_json[1]["prompt_source"], "shared");

    // Copying without replacing members carries each member's own
    // override (or lack of one) forward into the copy.
    let copied = call_graph_action(
        &handler,
        "graph_ensemble",
        "copy",
        params_id(GraphCopyEnsembleParams {
            source_ensemble_id: ensemble_id.clone(),
            spec_id: None,
            graph_id: None,
            name: Some("Panel copy".to_string()),
            prompt_template: None,
            members: None,
            min_pass: None,
            timeout_minutes: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            from_node: None,
            condition: None,
            on_pass_to: None,
            on_fail_to: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&copied), "{}", text(&copied));
    let copied_ensemble_id = extract_id(&copied, "ensemble_id");
    let copied_details = db
        .get_ensemble_details(&copied_ensemble_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        copied_details.members[0].prompt_override.as_deref(),
        Some("review for security issues")
    );
    assert_eq!(copied_details.members[1].prompt_override, None);
}

// ── graph_ensemble ────────────────────────────────────────

#[tokio::test]
async fn graph_update_ensemble_prompt_members_join_and_wiring() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let alt_entry = add_agent_node(&handler, &spec.id, "AltEntry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let alt_exit = add_agent_node(&handler, &spec.id, "AltExit").await;

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Original prompt".to_string()),
            members: Some(vec![
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
            ]),
            blueprint: None,
            node: None,
            from_node: entry.clone(),
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
        }),
    )
    .await
    .unwrap();
    let ensemble_id = extract_id(&created, "ensemble_id");

    let added_entry = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            quorum_grace_minutes: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: Some(alt_entry.clone()),
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&added_entry), "{}", text(&added_entry));

    // No fields at all -> rejected.
    let no_op = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
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
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&no_op));
    assert!(text(&no_op).contains("at least one field"));

    // Prompt-only update, propagated to existing members without a resize.
    let prompt_updated = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: Some("New prompt".to_string()),
            members: None,
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
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&prompt_updated), "{}", text(&prompt_updated));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.prompt_template, "New prompt");

    // Grow membership 2 -> 4 while preserving both entry sources.
    let grown = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: Some(vec![
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
                crate::daemon::params::EnsembleMemberParams {
                    platform: "gemini".to_string(),
                    model: None,
                    prompt_override: Some("Review only for test coverage gaps.".to_string()),
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                crate::daemon::params::EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: Some("growth-test".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
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
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&grown), "{}", text(&grown));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.members.len(), 4);
    // The new member's own prompt_override is persisted on the ensemble
    // row and propagated into its node config as the effective prompt,
    // while the other two still carry no override.
    assert_eq!(details.members[0].prompt_override, None);
    assert_eq!(details.members[1].prompt_override, None);
    assert_eq!(
        details.members[2].prompt_override.as_deref(),
        Some("Review only for test coverage gaps.")
    );
    let gemini_node = db
        .get_graph_node(&details.members[2].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        gemini_node
            .config
            .get("prompt_template")
            .and_then(|v| v.as_str()),
        Some("Review only for test coverage gaps.")
    );
    let member_ids: std::collections::HashSet<_> = details
        .members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let entry_edges = db
        .list_graph_edges(&spec.id)
        .unwrap()
        .into_iter()
        .filter(|edge| {
            [entry.as_str(), alt_entry.as_str()].contains(&edge.from_node.as_str())
                && member_ids.contains(edge.to_node.as_str())
        })
        .count();
    assert_eq!(entry_edges, 8);

    // Shrink membership 4 -> 3 and retain complete fan-out.
    let shrunk = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: Some(vec![
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
                crate::daemon::params::EnsembleMemberParams {
                    platform: "gemini".to_string(),
                    model: None,
                    prompt_override: Some("Review only for test coverage gaps.".to_string()),
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
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
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&shrunk), "{}", text(&shrunk));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.members.len(), 3);
    let member_ids: std::collections::HashSet<_> = details
        .members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let entry_edges = db
        .list_graph_edges(&spec.id)
        .unwrap()
        .into_iter()
        .filter(|edge| {
            [entry.as_str(), alt_entry.as_str()].contains(&edge.from_node.as_str())
                && member_ids.contains(edge.to_node.as_str())
        })
        .count();
    assert_eq!(entry_edges, 6);

    // min_pass out of bounds.
    let bad_min_pass = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
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
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_min_pass));

    // Negative straggler timeout.
    let bad_straggler = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
            min_pass: None,
            straggler_timeout_minutes: Some(Some(-1)),
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
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_straggler));

    // Re-wire on_pass_to to a new valid target.
    let rewired = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
            min_pass: Some(2),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: Some(alt_exit.clone()),
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&rewired), "{}", text(&rewired));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.on_pass_to, alt_exit);

    // Unknown on_pass_to target.
    let bad_target = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: Some("not-a-node".to_string()),
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_target));

    let missing_ensemble = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: "ghost-ensemble".to_string(),
            kind: None,
            prompt_template: Some("x".to_string()),
            members: None,
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
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing_ensemble));
}

/// CM23: `graph_ensemble`'s member list can set one member's own
/// `timeout_minutes`, independent of the ensemble's shared value — FR3
/// (set through the same call that manages membership) and the inner
/// wiring together: the override lands on the `EnsembleMember` row AND
/// gets baked into that member's own node config, not the ensemble's.
#[tokio::test]
async fn graph_update_ensemble_sets_member_own_timeout() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Original prompt".to_string()),
            members: Some(vec![
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
            ]),
            blueprint: None,
            node: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: Some(2),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: Some(20),
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter,
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    let ensemble_id = extract_id(&created, "ensemble_id");

    let updated = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: Some(vec![
                crate::daemon::params::EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: Some(3),
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
            ]),
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
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.members[0].timeout_minutes, Some(3));
    assert_eq!(details.members[1].timeout_minutes, None);

    let claude_node = db
        .get_graph_node(&details.members[0].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        claude_node
            .config
            .get("timeout_minutes")
            .and_then(|v| v.as_i64()),
        Some(3),
        "member's own override, not the ensemble's shared 20, must be baked into its node config"
    );
    let opencode_node = db
        .get_graph_node(&details.members[1].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        opencode_node
            .config
            .get("timeout_minutes")
            .and_then(|v| v.as_i64()),
        Some(20),
        "member with no override still uses the ensemble's shared timeout_minutes"
    );
}

/// CM30: `graph_node`/`graph_node` accept and validate the
/// three infra-retry keys on a plain agent node.
#[test]
fn validate_node_config_accepts_infra_retry_keys_on_agent() {
    let config = serde_json::json!({
        "platform": "claude",
        "infra_retry_limit": 0,
        "infra_crash_max_seconds": 120,
        "infra_backoff_seconds": 0,
    });
    assert!(validate_node_config(GraphNodeKind::Agent, &config).is_ok());
}

/// CM30: non-integer or negative infra-retry values are rejected with
/// the new error message (mirrors the timeout_minutes convention: 0 is
/// legitimate, negatives and strings are not).
#[test]
fn validate_node_config_rejects_bad_infra_retry_values() {
    for (key, bad) in [
        ("infra_retry_limit", serde_json::json!(-1)),
        ("infra_retry_limit", serde_json::json!("3")),
        ("infra_retry_limit", serde_json::json!(1.5)),
        ("infra_crash_max_seconds", serde_json::json!(-2)),
        ("infra_backoff_seconds", serde_json::json!("30")),
    ] {
        let config = serde_json::json!({"platform": "claude", key: bad});
        let err = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
        assert!(
            err.contains(key) && err.contains("non-negative integer"),
            "unexpected error for {key}={bad}: {err}"
        );
    }
}

/// CM30: member-level and ensemble-level infra fields round-trip through
/// `get_ensemble_details`, bake into member node configs member-wins,
/// and negative values are rejected at both levels.
#[tokio::test]
async fn graph_add_ensemble_infra_fields_round_trip_and_reject_negative() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let bad_member = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Bad Member".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![
                crate::daemon::params::EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: Some(-1),
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
            ]),
            blueprint: None,
            node: None,
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
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_member), "{}", text(&bad_member));
    assert!(
        text(&bad_member).contains("infra_retry_limit"),
        "{}",
        text(&bad_member)
    );

    let bad_ensemble = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Bad Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![
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
            ]),
            blueprint: None,
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: Some(-5),
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_ensemble), "{}", text(&bad_ensemble));
    assert!(
        text(&bad_ensemble).contains("infra_crash_max_seconds"),
        "{}",
        text(&bad_ensemble)
    );

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Infra Ensemble".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![
                crate::daemon::params::EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: Some(1),
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: Some(7),
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
            ]),
            blueprint: None,
            node: None,
            from_node: entry,
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: Some(0),
            infra_crash_max_seconds: Some(120),
            infra_backoff_seconds: None,
            on_pass_to: arbiter,
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.infra_retry_limit, Some(0));
    assert_eq!(details.ensemble.infra_crash_max_seconds, Some(120));
    assert_eq!(details.ensemble.infra_backoff_seconds, None);
    assert_eq!(details.members[0].infra_retry_limit, Some(1));
    assert_eq!(details.members[0].infra_backoff_seconds, Some(7));
    assert_eq!(details.members[1].infra_retry_limit, None);

    // Member wins over ensemble in the baked node config; a member with
    // no override inherits the ensemble default; an unset ensemble field
    // bakes nothing at all.
    let first = db
        .get_graph_node(&details.members[0].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        first
            .config
            .get("infra_retry_limit")
            .and_then(|v| v.as_i64()),
        Some(1)
    );
    assert_eq!(
        first
            .config
            .get("infra_crash_max_seconds")
            .and_then(|v| v.as_i64()),
        Some(120)
    );
    assert_eq!(
        first
            .config
            .get("infra_backoff_seconds")
            .and_then(|v| v.as_i64()),
        Some(7)
    );
    let second = db
        .get_graph_node(&details.members[1].node_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        second
            .config
            .get("infra_retry_limit")
            .and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        second
            .config
            .get("infra_crash_max_seconds")
            .and_then(|v| v.as_i64()),
        Some(120)
    );
    assert!(second.config.get("infra_backoff_seconds").is_none());
}

/// CM30: `graph_ensemble` sets, propagates (prompt-only branch)
/// and clears (null) the ensemble-level infra defaults, rebaking member
/// node configs each time.
#[tokio::test]
async fn graph_update_ensemble_sets_propagates_and_clears_infra_defaults() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Infra Update", &entry, &arbiter).await;

    // Infra-only update (no members/prompt/timeout): the prompt-only
    // branch must still rebake every member's node config.
    let mut update = blank_ensemble_update(&ensemble_id);
    update.infra_retry_limit = Some(Some(0));
    update.infra_crash_max_seconds = Some(Some(90));
    let updated = call_graph_action(&handler, "graph_ensemble", "update", params_id(update))
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.infra_retry_limit, Some(0));
    assert_eq!(details.ensemble.infra_crash_max_seconds, Some(90));
    for member in &details.members {
        let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
        assert_eq!(
            node.config
                .get("infra_retry_limit")
                .and_then(|v| v.as_i64()),
            Some(0)
        );
        assert_eq!(
            node.config
                .get("infra_crash_max_seconds")
                .and_then(|v| v.as_i64()),
            Some(90)
        );
    }

    // Clear back to deferring: the value is removed from the row and the
    // baked key. The wire shape cannot carry the clear (a plain
    // `Option<Option<_>>` reads JSON null as "leave unchanged"), so the
    // clear path is exercised at the impl seam.
    let mut clear = blank_ensemble_update(&ensemble_id);
    clear.infra_retry_limit = Some(None);
    clear.infra_crash_max_seconds = Some(None);
    let cleared = handler.graph_update_ensemble_impl(clear).await.unwrap();
    assert!(!is_err(&cleared), "{}", text(&cleared));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.infra_retry_limit, None);
    assert_eq!(details.ensemble.infra_crash_max_seconds, None);
    for member in &details.members {
        let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
        assert!(node.config.get("infra_retry_limit").is_none());
        assert!(node.config.get("infra_crash_max_seconds").is_none());
    }

    // Negative ensemble-level values are rejected, and rejected BEFORE
    // the propagate branch bakes them into every member's node config —
    // a refusal must change nothing, same guarantee CB40 gives min_pass.
    let mut negative = blank_ensemble_update(&ensemble_id);
    negative.infra_backoff_seconds = Some(Some(-1));
    let rejected = call_graph_action(&handler, "graph_ensemble", "update", params_id(negative))
        .await
        .unwrap();
    assert!(is_err(&rejected), "{}", text(&rejected));
    assert!(
        text(&rejected).contains("infra_backoff_seconds"),
        "{}",
        text(&rejected)
    );
    for member in &details.members {
        let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
        assert!(
            node.config.get("infra_backoff_seconds").is_none(),
            "rejected update must not bake infra_backoff_seconds into member config"
        );
    }

    // Same guarantee on the member-resize branch: a negative
    // ensemble-level value paired with a `members` resize must reject
    // before `replace_ensemble_members` runs, leaving the old members in
    // place.
    let mut negative_resize = blank_ensemble_update(&ensemble_id);
    negative_resize.infra_retry_limit = Some(Some(-1));
    negative_resize.members = Some(vec![
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
        crate::daemon::params::EnsembleMemberParams {
            platform: "codex".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ]);
    let rejected_resize = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(negative_resize),
    )
    .await
    .unwrap();
    assert!(is_err(&rejected_resize), "{}", text(&rejected_resize));
    let details_after = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(
        details_after.members.len(),
        details.members.len(),
        "rejected update must not resize the member list"
    );
}
