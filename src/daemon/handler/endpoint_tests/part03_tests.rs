//! Part 03 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// Decision 4: import never overwrites — a name collision in the target
/// workdir gets a numeric suffix instead of a refusal or an overwrite.
#[tokio::test]
async fn graph_import_dedupes_colliding_name_with_numeric_suffix() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;
    let exported = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: source_graph_id,
        }))
        .await
        .unwrap();
    let document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

    let first = handler
        .graph_import(Parameters(GraphImportParams {
            document: document.clone(),
            workdir: workdir.clone(),
            name: Some("Collide".to_string()),
        }))
        .await
        .unwrap();
    let first_body: serde_json::Value = serde_json::from_str(&raw_text(&first)).unwrap();
    assert_eq!(first_body["name"], "Collide");

    let second = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: Some("Collide".to_string()),
        }))
        .await
        .unwrap();
    let second_body: serde_json::Value = serde_json::from_str(&raw_text(&second)).unwrap();
    assert_eq!(second_body["name"], "Collide (2)");
    assert_ne!(second_body["graph_id"], first_body["graph_id"]);
    assert_eq!(db.list_graphs(Some(&workdir), true).unwrap().len(), 3);
}

/// Decision 5's all-or-nothing guarantee: a document whose edge names a
/// nonexistent node is rejected before anything is written.
#[tokio::test]
async fn graph_import_rejects_bad_edge_reference_and_writes_nothing() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let document = serde_json::json!({
        "format_version": 1,
        "name": "Broken Graph",
        "nodes": [
            {"name": "only", "kind": "check", "position": 1, "config": {"command": "true"}}
        ],
        "edges": [
            {"from_node": "only", "to_node": "ghost", "condition": "always"}
        ],
        "ensembles": []
    });

    let result = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("ghost"));
    assert!(db.list_graphs(Some(&workdir), true).unwrap().is_empty());
}

#[tokio::test]
async fn graph_import_rejects_missing_format_version() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let document = serde_json::json!({
        "name": "No Version",
        "nodes": [],
        "edges": [],
        "ensembles": []
    });

    let result = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("format_version"));
    assert!(db.list_graphs(Some(&workdir), true).unwrap().is_empty());
}

/// Requirement 5, exercised end to end through the MCP surface: export,
/// import, export again — identical document except the name — for a
/// graph that includes an ensemble, which must survive as an ensemble
/// rather than expanded member nodes.
#[tokio::test]
async fn graph_export_import_v2_round_trip_preserves_ensemble() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();

    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Ensemble Graph".to_string(),
            description: Some("Has an ensemble".to_string()),
            workdir: workdir.clone(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let kickoff = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");

    let downstream = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");

    let ensemble = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Proposers".to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-a".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-b".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            condition: "always".to_string(),
            from_node: kickoff_id.clone(),
            on_pass_to: downstream_id.clone(),
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&ensemble), "{}", text(&ensemble));

    let first_export = handler
        .graph_export(Parameters(GraphExportParams { graph_id }))
        .await
        .unwrap();
    let first_doc: serde_json::Value = serde_json::from_str(&raw_text(&first_export)).unwrap();
    assert_eq!(first_doc["ensembles"].as_array().unwrap().len(), 1);
    // CM32: the export document carries slugs only — no display string
    // is ever written to it.
    let first_raw = raw_text(&first_export);
    assert!(!first_raw.contains("platform_display"));
    assert!(!first_raw.contains("tool_name"));
    // The plain node list must exclude the ensemble's member/join nodes.
    assert_eq!(first_doc["nodes"].as_array().unwrap().len(), 2);

    let imported = handler
        .graph_import(Parameters(GraphImportParams {
            document: first_doc.clone(),
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let imported_body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    let new_graph_id = imported_body["graph_id"].as_str().unwrap().to_string();
    assert_eq!(imported_body["name"], "Ensemble Graph (2)");
    // Every member carried its platform/model through the v2 export —
    // no node should be flagged.
    assert!(imported_body["nodes_missing_platform"]
        .as_array()
        .unwrap()
        .is_empty());

    let second_export = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: new_graph_id,
        }))
        .await
        .unwrap();
    let mut second_doc: serde_json::Value =
        serde_json::from_str(&raw_text(&second_export)).unwrap();
    // Identical except the name (decision 4 renamed it on collision).
    second_doc["name"] = first_doc["name"].clone();
    assert_eq!(first_doc, second_doc);
}

#[tokio::test]
async fn graph_export_import_round_trip_preserves_all_ensemble_kinds() {
    // CB62 end-to-end: cascade + round_robin ensembles survive export ->
    // import -> export identical (modulo the collision-rename), so
    // `graph_get` on the imported graph shows cascade/round_robin, not
    // parallel. CB71 carries the cascade's second entry source in the
    // document. Infra budgets remain absent, and the round trip stays
    // identical for them because both exports omit them.
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();

    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Kind Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let kickoff = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");

    let alt = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "alt".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let alt_id = extract_id(&alt, "node_id");

    let downstream = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");

    let wired = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            from_node: kickoff_id.clone(),
            to_node: alt_id.clone(),
            condition: "always".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&wired), "{}", text(&wired));

    let cascade = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: Some("cascade".to_string()),
            name: "CascadeTeam".to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-a".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: Some(5),
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-b".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            condition: "always".to_string(),
            from_node: kickoff_id.clone(),
            on_pass_to: downstream_id.clone(),
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: Some(2),
            infra_crash_max_seconds: Some(60),
            infra_backoff_seconds: Some(30),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&cascade), "{}", text(&cascade));
    let cascade_id = extract_id(&cascade, "ensemble_id");

    let round_robin = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: Some("round_robin".to_string()),
            name: "RrTeam".to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-a".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-b".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            condition: "always".to_string(),
            from_node: kickoff_id.clone(),
            on_pass_to: downstream_id.clone(),
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: Some(3),
            infra_crash_max_seconds: Some(61),
            infra_backoff_seconds: Some(31),
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&round_robin), "{}", text(&round_robin));

    let second_source = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            ensemble_id: cascade_id.clone(),
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
            add_entry_from: Some(alt_id.clone()),
            add_entry_condition: None,
            remove_entry_from: None,
            commit_rights: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&second_source), "{}", text(&second_source));

    let first_export = handler
        .graph_export(Parameters(GraphExportParams { graph_id }))
        .await
        .unwrap();
    let first_doc: serde_json::Value = serde_json::from_str(&raw_text(&first_export)).unwrap();
    assert_eq!(first_doc["ensembles"].as_array().unwrap().len(), 2);

    let imported = handler
        .graph_import(Parameters(GraphImportParams {
            document: first_doc.clone(),
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let imported_body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    let new_graph_id = imported_body["graph_id"].as_str().unwrap().to_string();

    let second_export = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: new_graph_id,
        }))
        .await
        .unwrap();
    let mut second_doc: serde_json::Value =
        serde_json::from_str(&raw_text(&second_export)).unwrap();
    second_doc["name"] = first_doc["name"].clone();
    assert_eq!(first_doc, second_doc);

    let kinds: Vec<&str> = second_doc["ensembles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap_or("parallel"))
        .collect();
    assert!(kinds.contains(&"cascade"), "kinds: {kinds:?}");
    assert!(kinds.contains(&"round_robin"), "kinds: {kinds:?}");

    // CB71: the cascade's second entry source (`alt`, added with
    // add_entry_from, default condition always) is carried by the document
    // and restored — the round-trip equality above already pins that it
    // survives import; this pins that the FIRST export wrote it.
    let cascade_entry = first_doc["ensembles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "CascadeTeam")
        .expect("CascadeTeam in export");
    assert_eq!(
        cascade_entry["extra_entry_sources"],
        serde_json::json!([{"from_node": "alt", "condition": "always"}]),
        "CB71: the second entry source must appear in the export document"
    );
}

#[tokio::test]
async fn graph_export_import_round_trip_preserves_ensemble_commit_rights() {
    // NFR1(d): an ensemble created with `commit_rights: true` keeps the
    // flag through export (document) and import (new graph's DB row).
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();

    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Commit Rights Ensemble Graph".to_string(),
            description: None,
            workdir: workdir.clone(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let kickoff = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "kickoff".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let kickoff_id = extract_id(&kickoff, "node_id");

    let downstream = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "downstream".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let downstream_id = extract_id(&downstream, "node_id");

    let ensemble = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            commit_rights: Some(true),
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Committer".to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-a".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: Some("model-b".to_string()),
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            condition: "always".to_string(),
            from_node: kickoff_id.clone(),
            on_pass_to: downstream_id.clone(),
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&ensemble), "{}", text(&ensemble));

    let exported = handler
        .graph_export(Parameters(GraphExportParams { graph_id }))
        .await
        .unwrap();
    let doc: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
    assert_eq!(doc["ensembles"].as_array().unwrap().len(), 1);
    assert_eq!(
        doc["ensembles"][0]["commit_rights"],
        serde_json::json!(true),
        "export must carry the ensemble's commit_rights: {doc}"
    );

    let imported = handler
        .graph_import(Parameters(GraphImportParams {
            document: doc,
            workdir: workdir.clone(),
            name: Some("Commit Rights Ensemble Graph Imported".to_string()),
        }))
        .await
        .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let imported_body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    let new_graph_id = imported_body["graph_id"].as_str().unwrap().to_string();

    let imported_ensembles = db.list_ensembles_for_graph(&new_graph_id).unwrap();
    assert_eq!(imported_ensembles.len(), 1);
    assert!(
        imported_ensembles[0].ensemble.commit_rights,
        "imported ensemble must hold commit rights"
    );
}

#[tokio::test]
async fn graph_preflight_rejects_unknown_named_output_reference() {
    // A structurally valid two-node chain where the downstream node's
    // prompt references `{{output:Ghost}}` — a node that isn't in the
    // graph. Preflight must reject it before spending any probe quota,
    // not leave the marker to surface unsubstituted at runtime (CM1).
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Preflight Named Output Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    let alpha = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "alpha".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let alpha_id = extract_id(&alpha, "node_id");

    let mut beta_config = agent_node_config("claude");
    beta_config.insert(
        "prompt_template".to_string(),
        serde_json::Value::String("Follow the plan: {{output:Ghost}}".to_string()),
    );
    let beta = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "beta".to_string(),
            kind: Some("agent".to_string()),
            config: Some(beta_config),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let beta_id = extract_id(&beta, "node_id");

    let edge = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            from_node: alpha_id,
            to_node: beta_id,
            condition: "always".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&edge), "{}", text(&edge));

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: graph_id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "preflight should reject the unknown named-output reference: {}",
        text(&result)
    );
    let msg = text(&result);
    assert!(
        msg.contains("references unknown node 'Ghost'"),
        "preflight error should name the missing node, got: {msg}"
    );
}

#[tokio::test]
async fn graph_preflight_reports_graph_validation_errors() {
    // Two agent nodes with no edge between them => multiple entry points.
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Preflight Graph".to_string(),
            description: None,
            workdir,
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "alpha".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "beta".to_string(),
            kind: Some("agent".to_string()),
            config: Some(agent_node_config("claude")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: graph_id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "preflight should report graph error: {}",
        text(&result)
    );
    let msg = text(&result).to_lowercase();
    assert!(
        msg.contains("entry"),
        "preflight error should mention entry points, got: {msg}"
    );
    assert!(
        msg.contains("alpha") && msg.contains("beta"),
        "preflight error should name the concrete nodes, got: {msg}"
    );
}

/// (CM38 FR4) `graph_preflight` reports an unknown `{{...}}` marker in a
/// check command as an error, before any probe quota is spent.
#[tokio::test]
async fn graph_preflight_reports_unknown_marker_in_check_command() {
    let (dir, _db, handler) = endpoint_test_handler();
    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Marker Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "gatekeeper".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("echo {{nope}}")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: graph_id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        is_err(&result),
        "preflight must report the unknown marker: {}",
        text(&result)
    );
    let msg = text(&result);
    assert!(
        msg.contains("unknown marker {{nope}} in check command"),
        "message must carry the exact refusal text: {msg}"
    );
    assert!(
        msg.contains("gatekeeper"),
        "message must name the node: {msg}"
    );
}

/// (CM38 FR4) every documented check-command marker clears preflight.
#[tokio::test]
async fn graph_preflight_accepts_every_known_check_command_marker() {
    let (dir, _db, handler) = endpoint_test_handler();
    let created = handler
        .graph_create(Parameters(GraphCreateParams {
            name: "Marker Graph".to_string(),
            description: None,
            workdir: dir.path().to_string_lossy().to_string(),
            trigger: None,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        }))
        .await
        .unwrap();
    let graph_id = extract_id(&created, "graph_id");

    handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "gatekeeper".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config(
                "echo {{graph_name}} {{workdir}} {{spec_id}} {{spec_name}} \
                     {{spec_start_head}} {{spec_start_dirty}} {{spec_committed_head}}",
            )),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: graph_id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(
        !is_err(&result),
        "all seven check-command markers must clear preflight: {}",
        text(&result)
    );
}

/// (CM19) `graph_preflight` reports a non-terminal node with no `fail`
/// edge as a structured `graph_warnings` entry — advisory, not a tool
/// error, and it does not change the verdict. A declared terminal with
/// the same missing edge is not reported.
#[tokio::test]
async fn graph_preflight_warns_for_node_with_no_fail_edge() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let mk_node = |name: &str, position: i64| GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        // Unconfigured platform -> probe returns NotConfigured instantly,
        // no quota spent, preflight still succeeds.
        config: serde_json::json!({"platform": "cm19-unconfigured-platform"}),
        position,
        created_at: chrono::Utc::now(),
    };
    let alpha = mk_node("alpha", 1);
    let beta = mk_node("beta", 2);
    let gamma = mk_node("gamma", 3);
    db.insert_graph_node(&alpha).unwrap();
    db.insert_graph_node(&beta).unwrap();
    db.insert_graph_node(&gamma).unwrap();

    let mk_edge = |from: &str, to: &str, cond: GraphEdgeCondition| GraphEdge {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition: cond,
    };
    // alpha: pass + fail -> fully routed (not a dead end, not a terminal)
    db.insert_graph_edge(&mk_edge(&alpha.id, &beta.id, GraphEdgeCondition::Pass))
        .unwrap();
    db.insert_graph_edge(&mk_edge(&alpha.id, &beta.id, GraphEdgeCondition::Fail))
        .unwrap();
    // beta: pass only -> continues on success, dead-ends on fail => WARN
    db.insert_graph_edge(&mk_edge(&beta.id, &gamma.id, GraphEdgeCondition::Pass))
        .unwrap();
    // gamma: no outgoing edges -> deliberate terminal => NOT warned

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
        "a preflight warning must not be a tool error: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();

    let warnings = body["graph_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("preflight must include graph_warnings: {body}"));
    assert_eq!(
        warnings.len(),
        1,
        "expected exactly one graph warning: {body}"
    );
    assert_eq!(warnings[0]["node_id"], beta.id);
    assert_eq!(warnings[0]["node_name"], "beta");
    assert_eq!(warnings[0]["missing_edge"], "fail");
    assert_eq!(warnings[0]["has_error_path"], false);
    let warn_text = warnings[0]["warning"].as_str().unwrap_or_default();
    assert!(
        warn_text.contains("beta") && warn_text.contains("no outgoing edge for that status"),
        "warning must name the node and state the consequence: {body}"
    );

    // gamma (declared terminal) must be in `terminals`, not in warnings.
    let terminals = body["terminals"].as_array().unwrap();
    assert!(
        terminals
            .iter()
            .any(|t| t.as_str().unwrap_or_default().contains("gamma")),
        "gamma should be listed as a terminal: {body}"
    );

    // Verdict is unaffected by the warning (requirement 4).
    assert!(
        body.get("would_fail").is_some(),
        "verdict fields intact: {body}"
    );
}

/// (FR1/FR4-a) An ensemble with `on_pass_to` and no `on_fail_to`, whose
/// quorum has no `fail`/`always` edge, produces exactly one advisory
/// `graph_warnings` entry naming the ensemble and its quorum node.
#[tokio::test]
async fn graph_preflight_warns_quorum_without_fail_exit() {
    let (dir, db, handler) = endpoint_test_handler();
    let (lp, ensemble_id, join_id, _arbiter_id) =
        graph_with_unexited_quorum(&dir, &db, &handler).await;

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
        "a quorum warning must not be a tool error: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let warnings = body["graph_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("preflight must include graph_warnings: {body}"));
    println!(
        "{}",
        serde_json::to_string_pretty(warnings).unwrap_or_default()
    );
    assert_eq!(
        warnings.len(),
        1,
        "expected exactly one quorum warning: {body}"
    );
    let warning = &warnings[0];
    let details = db.get_graph_details(&lp.id).unwrap().unwrap();
    let join_name = details
        .graph_nodes
        .iter()
        .find(|n| n.id == join_id)
        .map(|n| n.name.clone())
        .unwrap();
    assert_eq!(warning["node_id"], join_id);
    assert_eq!(warning["node_name"], join_name);
    assert_eq!(warning["kind"], "join");
    assert_eq!(warning["ensemble_id"], ensemble_id);
    assert_eq!(warning["ensemble_name"], "Implementer");
    assert_eq!(warning["missing_edge"], "fail");
    let warn_text = warning["warning"].as_str().unwrap_or_default();
    assert!(
        warn_text.contains("Ensemble 'Implementer'")
            && warn_text.contains(&ensemble_id)
            && warn_text.contains("no on_fail_to"),
        "warning must name the ensemble and the missing exit: {body}"
    );
    assert!(
        body.get("would_fail").is_some(),
        "verdict fields intact: {body}"
    );
}

/// (FR2/FR4-b) The same ensemble with `on_fail_to` set is silent — while
/// the identical graph without it warns first, so silence cannot be the
/// absence of the check.
#[tokio::test]
async fn graph_preflight_quiet_with_on_fail_to() {
    let (dir, db, handler) = endpoint_test_handler();
    let (lp, ensemble_id, _join_id, arbiter_id) =
        graph_with_unexited_quorum(&dir, &db, &handler).await;

    assert_eq!(
        preflight_warnings(&handler, &lp.id).await.len(),
        1,
        "baseline: the null-on_fail_to graph must warn before the update"
    );

    let updated = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            ensemble_id: ensemble_id.clone(),
            kind: None,
            prompt_template: None,
            members: None,
            crew: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: Some(Some(arbiter_id)),
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,
            commit_rights: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let warnings = preflight_warnings(&handler, &lp.id).await;
    assert!(
        warnings.is_empty(),
        "on_fail_to must silence the quorum warning: {warnings:?}"
    );
}

/// (FR2/FR4-c) No `on_fail_to` but a hand-added `always` edge from the
/// quorum routes a failed quorum onward, so there is nothing to warn about
/// — while the same graph without that edge warns first.
#[tokio::test]
async fn graph_preflight_quiet_with_manual_always_edge() {
    let (dir, db, handler) = endpoint_test_handler();
    let (lp, _ensemble_id, join_id, arbiter_id) =
        graph_with_unexited_quorum(&dir, &db, &handler).await;

    assert_eq!(
        preflight_warnings(&handler, &lp.id).await.len(),
        1,
        "baseline: the quorum must warn before the manual edge is added"
    );

    db.insert_graph_edge(&GraphEdge {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        from_node: join_id,
        to_node: arbiter_id,
        condition: GraphEdgeCondition::Always,
    })
    .unwrap();

    let warnings = preflight_warnings(&handler, &lp.id).await;
    assert!(
        warnings.is_empty(),
        "a manual 'always' edge is a fail exit: {warnings:?}"
    );
}

/// A spec-owned ensemble (not the graph's top-level one) is the case this
/// warning exists for: a round-robin member reports FAIL and the spec ends
/// at the quorum. The spec's own graph reaches preflight through the
/// per-spec branch, so assert it there rather than only on the top level.
#[tokio::test]
async fn graph_preflight_warns_spec_quorum_without_fail_exit() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mk_node =
        |name: &str, kind: GraphNodeKind, config: serde_json::Value, position: i64| GraphNode {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: name.to_string(),
            kind,
            config,
            position,
            created_at: chrono::Utc::now(),
        };
    let kickoff = mk_node(
        "kickoff",
        GraphNodeKind::Check,
        serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        1,
    );
    let arbiter = mk_node(
        "arbiter",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "cm19-unconfigured-platform"}),
        2,
    );
    db.insert_graph_node(&kickoff).unwrap();
    db.insert_graph_node(&arbiter).unwrap();

    let added = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Implementer".to_string(),
            kind: None,
            prompt_template: Some("Review".to_string()),
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "cm19-unconfigured-platform".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "cm19-unconfigured-platform".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            blueprint: None,
            from_node: kickoff.id.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.id.clone(),
            on_fail_to: None,
            commit_rights: None,
            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let ensembles = db.list_ensembles_for_spec(&spec.id).unwrap();
    assert_eq!(ensembles.len(), 1, "expected one spec ensemble");
    let ensemble = &ensembles[0].ensemble;

    let warnings = preflight_warnings(&handler, &lp.id).await;
    assert_eq!(
        warnings.len(),
        1,
        "the spec's quorum must warn: {warnings:?}"
    );
    assert_eq!(warnings[0]["node_id"], ensemble.join_node_id);
    assert_eq!(warnings[0]["ensemble_id"], ensemble.id);
    assert_eq!(warnings[0]["ensemble_name"], "Implementer");
    assert_eq!(warnings[0]["kind"], "join");
}

#[tokio::test]
async fn graph_update_rejects_unknown_graph_and_requires_a_field() {
    let (dir, db, handler) = endpoint_test_handler();
    let missing = handler
        .graph_update(Parameters(GraphUpdateParams {
            graph_id: "ghost".to_string(),
            name: Some("x".to_string()),
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: None,
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));

    let lp = insert_test_graph(&db, dir.path());
    let no_fields = handler
        .graph_update(Parameters(GraphUpdateParams {
            graph_id: lp.id.clone(),
            name: None,
            description: None,
            workdir: None,
            trigger: None,
            on_completed: None,
            hooks: None,
            infra_node_id: None,
            allow_dirty_start: None,
            require_spec_type: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&no_fields));
    assert!(text(&no_fields).contains("at least one field"));
}

// ── CM29: dirty-workdir warning/refusal ─────────────────────────

#[tokio::test]
async fn graph_preflight_reports_dirty_workdir_warning() {
    let (_dir, db, handler) = endpoint_test_handler();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());
    let lp = insert_test_graph(&db, git_dir.path());
    std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();

    let result = handler
        .graph_preflight(Parameters(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert_eq!(body["dirty_workdir_warning"]["count"], 2);
}

#[tokio::test]
async fn graph_run_refuses_when_previous_spec_left_workdir_dirty() {
    let (_dir, db, handler) = endpoint_test_handler();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());

    // Spec A ended Failed on this workdir in another graph — it is the
    // blocking predecessor. No run of `lp` recorded the dirt, so there is
    // no attribution, but the refusal still names the predecessor (CB69).
    let failed_lp = insert_test_graph(&db, git_dir.path());
    insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

    let lp = insert_test_graph(&db, git_dir.path());
    insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);

    std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();
    std::fs::write(git_dir.path().join("c.txt"), "c").unwrap();

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
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(
        msg.contains("Spec A"),
        "refusal must name the blocking predecessor: {msg}"
    );
    assert!(
        msg.contains("failed"),
        "refusal must name the predecessor's end status: {msg}"
    );
    assert!(
        msg.contains("not launched"),
        "refusal must say not launched: {msg}"
    );
    assert!(
        msg.contains("Set allow_dirty_start"),
        "flag hint kept: {msg}"
    );
    assert!(
        !msg.contains("dirty before this graph ran"),
        "refusal must not use the attribution sentence: {msg}"
    );
}

#[tokio::test]
async fn graph_run_allow_dirty_start_launches_with_warning() {
    let (_dir, db, handler) = endpoint_test_handler();
    let git_dir = tempdir().unwrap();
    init_git_repo(git_dir.path());

    let failed_lp = insert_test_graph(&db, git_dir.path());
    insert_named_spec(&db, &failed_lp.id, "Spec A", 1, GraphSpecStatus::Failed);

    let lp = insert_test_graph(&db, git_dir.path());
    insert_named_spec(&db, &lp.id, "Spec B", 1, GraphSpecStatus::Pending);
    db.update_graph_allow_dirty_start(&lp.id, true).unwrap();

    std::fs::write(git_dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(git_dir.path().join("b.txt"), "b").unwrap();
    std::fs::write(git_dir.path().join("c.txt"), "c").unwrap();

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
    assert!(!is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(
        msg.contains("WARNING"),
        "launch warning still surfaced: {msg}"
    );
    assert!(
        msg.contains("allow_dirty_start is set"),
        "warning must state the reason: {msg}"
    );
    assert!(
        !msg.contains("Set allow_dirty_start"),
        "must not tell the user to set an already-set flag: {msg}"
    );
}

/// A document naming a custom prompt preset this machine does not have must
/// be refused through the MCP surface (never imported to then spawn with an
/// empty prompt), and nothing may be written.
#[tokio::test]
async fn graph_import_rejects_unknown_prompt_preset_and_writes_nothing() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let preset = format!("absent-{}", uuid::Uuid::new_v4());
    let document = serde_json::json!({
        "format_version": 1,
        "name": "Preset Graph",
        "nodes": [
            {
                "name": "worker",
                "kind": "agent",
                "position": 1,
                "config": {"platform": "claude", "prompt_preset": preset}
            }
        ],
        "edges": [],
        "ensembles": []
    });

    let result = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();

    assert!(is_err(&result), "{}", text(&result));
    assert!(
        text(&result).contains(&preset),
        "error must name the unknown preset: {}",
        text(&result)
    );
    assert!(db.list_graphs(Some(&workdir), true).unwrap().is_empty());
}
