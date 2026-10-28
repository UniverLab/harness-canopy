//! Part 05 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[tokio::test]
async fn graph_update_node_response_names_changed_keys() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut config = agent_node_config("claude");
    config.insert("model".to_string(), serde_json::json!("old-model"));
    config.insert("timeout_minutes".to_string(), serde_json::json!(5));

    let added = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Agent".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let node_id = extract_id(&added, "node_id");

    let updated = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: node_id.clone(),
            name: None,
            kind: None,
            config: Some({
                let mut m = serde_json::Map::new();
                m.insert("model".to_string(), serde_json::json!("new-model"));
                m.insert("timeout_minutes".to_string(), serde_json::json!(10));
                m
            }),
            config_replace: None,
            position: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let response_text = text(&updated);
    assert!(
        response_text.contains("model"),
        "response should name 'model' as changed: {response_text}"
    );
    assert!(
        response_text.contains("timeout_minutes"),
        "response should name 'timeout_minutes' as changed: {response_text}"
    );
}

#[tokio::test]
async fn graph_import_preserves_skills_on_agent_nodes() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;

    let exported = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: source_graph_id.clone(),
        }))
        .await
        .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

    let nodes = document["nodes"].as_array_mut().unwrap();
    for node in nodes.iter_mut() {
        if node["name"] == "implementer" {
            node["config"]["skills"] = serde_json::json!(["skill-a", "skill-b"]);
        }
    }

    let imported = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: Some("Imported Graph".to_string()),
        }))
        .await
        .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    let new_graph_id = body["graph_id"].as_str().unwrap().to_string();

    let nodes = db.list_graph_nodes_for_graph(&new_graph_id).unwrap();
    let implementer = nodes.iter().find(|n| n.name == "implementer").unwrap();
    let skills = implementer.config["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 2);
    assert_eq!(skills[0].as_str().unwrap(), "skill-a");
    assert_eq!(skills[1].as_str().unwrap(), "skill-b");
}

#[tokio::test]
async fn graph_add_node_rejects_skills_on_check_gate_router() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    for (kind, base_config) in [
        ("check", check_node_config("cargo test")),
        ("gate", {
            let mut m = serde_json::Map::new();
            m.insert(
                "evaluate".to_string(),
                serde_json::Value::String("pass".to_string()),
            );
            m
        }),
        ("router", {
            let mut m = serde_json::Map::new();
            m.insert("routes".to_string(), serde_json::json!(["a", "b"]));
            m
        }),
    ] {
        let mut config = base_config;
        config.insert("skills".to_string(), serde_json::json!(["alpha", "beta"]));

        let rejected = handler
            .graph_add_node(Parameters(GraphAddNodeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                name: format!("Node-{kind}"),
                kind: Some(kind.to_string()),
                config: Some(config),
                blueprint: None,
                node: None,
                config_overrides: None,
            }))
            .await
            .unwrap();
        assert!(is_err(&rejected), "kind={kind} should reject skills");
        // Must be the pre-existing generic unknown-key message for this
        // kind — not a new skills-specific special case (spec FR3).
        let msg = text(&rejected);
        assert!(
            msg.contains(&format!("kind '{kind}'"))
                && msg.contains("unrecognized key 'skills'")
                && msg.contains("Accepted keys:"),
            "kind={kind}: {msg}"
        );
    }

    assert!(db.list_graph_nodes(&spec.id).unwrap().is_empty());
}

#[tokio::test]
async fn graph_import_rejects_skills_on_non_agent_nodes() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let (source_graph_id, ..) = build_simple_graph(&handler, &workdir, "Source Graph").await;

    let exported = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: source_graph_id.clone(),
        }))
        .await
        .unwrap();
    let mut document: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();

    let nodes = document["nodes"].as_array_mut().unwrap();
    for node in nodes.iter_mut() {
        if node["name"] == "gate" {
            node["config"]["skills"] = serde_json::json!(["alpha"]);
        }
    }

    let rejected = handler
        .graph_import(Parameters(GraphImportParams {
            document,
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&rejected), "{}", text(&rejected));
    // The node named "gate" in `build_simple_graph` is a `check` kind, so
    // import must reject it with the same generic unknown-key message
    // `graph_add_node`/`graph_update_node` produce (spec FR3).
    let msg = text(&rejected);
    assert!(
        msg.contains("kind 'check'")
            && msg.contains("unrecognized key 'skills'")
            && msg.contains("Accepted keys:"),
        "{msg}"
    );
}

#[tokio::test]
async fn graph_add_node_tolerates_malformed_skills_values() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let mut config_string = agent_node_config("claude");
    config_string.insert(
        "skills".to_string(),
        serde_json::Value::String("not-an-array".to_string()),
    );

    let added_string = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "StringSkills".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config_string),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added_string), "{}", text(&added_string));
    let node_id_string = extract_id(&added_string, "node_id");
    let stored_string = db.get_graph_node(&node_id_string).unwrap().unwrap();
    assert!(stored_string.config["skills"].is_string());

    let mut config_mixed = agent_node_config("claude");
    config_mixed.insert(
        "skills".to_string(),
        serde_json::json!(["alpha", 42, "beta"]),
    );

    let added_mixed = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "MixedSkills".to_string(),
            kind: Some("agent".to_string()),
            config: Some(config_mixed),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added_mixed), "{}", text(&added_mixed));
    let node_id_mixed = extract_id(&added_mixed, "node_id");
    let stored_mixed = db.get_graph_node(&node_id_mixed).unwrap().unwrap();
    let skills = stored_mixed.config["skills"].as_array().unwrap();
    assert_eq!(skills.len(), 3);
    assert_eq!(skills[0].as_str().unwrap(), "alpha");
    assert!(skills[1].is_number());
    assert_eq!(skills[2].as_str().unwrap(), "beta");
}

#[tokio::test]
async fn graph_add_edge_wires_nodes_and_rejects_foreign_node() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;

    let edge = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a.clone(),
            to_node: b.clone(),
            condition: "always".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&edge), "{}", text(&edge));
    assert_eq!(db.list_graph_edges(&spec.id).unwrap().len(), 1);

    let foreign = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a.clone(),
            to_node: "not-a-real-node".to_string(),
            condition: "always".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&foreign));

    let bad_condition = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id),
            graph_id: None,
            from_node: a,
            to_node: b,
            condition: "sideways".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&bad_condition));

    let edges = db
        .list_graph_edges(&db.list_graph_specs(&lp.id).unwrap()[0].id)
        .unwrap();
    let edge_id = edges[0].id.clone();
    let updated = handler
        .graph_update_edge(Parameters(GraphUpdateEdgeParams {
            edge_id: edge_id.clone(),
            condition: "fail".to_string(),
            route: None,
            to_node: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let same_condition = handler
        .graph_update_edge(Parameters(GraphUpdateEdgeParams {
            edge_id: edge_id.clone(),
            condition: "fail".to_string(),
            route: None,
            to_node: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&same_condition));
    assert!(text(&same_condition).contains("already uses condition"));

    let missing_edge = handler
        .graph_update_edge(Parameters(GraphUpdateEdgeParams {
            edge_id: "ghost-edge".to_string(),
            condition: "pass".to_string(),
            route: None,
            to_node: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing_edge));
}

// ── graph_update_edge(to_node) / graph_delete_edge / graph_delete_node ──

#[tokio::test]
async fn graph_update_edge_retargets_destination_without_changing_condition() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;
    let c = add_agent_node(&handler, &spec.id, "C").await;

    let added = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a.clone(),
            to_node: b.clone(),
            condition: "pass".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));
    let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

    let retargeted = handler
        .graph_update_edge(Parameters(GraphUpdateEdgeParams {
            edge_id: edge_id.clone(),
            condition: "pass".to_string(),
            route: None,
            to_node: Some(c.clone()),
        }))
        .await
        .unwrap();
    assert!(!is_err(&retargeted), "{}", text(&retargeted));

    let edge = db.get_graph_edge(&edge_id).unwrap().unwrap();
    assert_eq!(edge.to_node, c, "target changed to the new node");
    assert_eq!(
        edge.condition,
        crate::domain::graphs::GraphEdgeCondition::Pass,
        "condition untouched by a target-only update"
    );
    assert_eq!(
        edge.from_node, a,
        "source untouched by a target-only update"
    );
}

#[tokio::test]
async fn graph_update_edge_rejects_retarget_to_a_node_in_another_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;

    let other_lp = insert_test_graph(&db, dir.path());
    let other_spec = insert_test_spec(&db, &other_lp.id, 1);
    let foreign = add_agent_node(&handler, &other_spec.id, "Foreign").await;

    handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a.clone(),
            to_node: b.clone(),
            condition: "pass".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

    let result = handler
        .graph_update_edge(Parameters(GraphUpdateEdgeParams {
            edge_id: edge_id.clone(),
            condition: "pass".to_string(),
            route: None,
            to_node: Some(foreign),
        }))
        .await
        .unwrap();
    assert!(is_err(&result));

    let edge = db.get_graph_edge(&edge_id).unwrap().unwrap();
    assert_eq!(edge.to_node, b, "cross-graph retarget never persisted");
}

#[tokio::test]
async fn graph_delete_edge_removes_a_single_edge() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;
    handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a,
            to_node: b,
            condition: "pass".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

    let deleted = handler
        .graph_delete_edge(Parameters(GraphDeleteEdgeParams {
            edge_id: edge_id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    assert!(db.get_graph_edge(&edge_id).unwrap().is_none());

    let missing = handler
        .graph_delete_edge(Parameters(GraphDeleteEdgeParams {
            edge_id: "ghost-edge".to_string(),
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));
}

#[tokio::test]
async fn graph_delete_node_cascades_to_its_edges() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;
    let c = add_agent_node(&handler, &spec.id, "C").await;
    for (from, to) in [(a.clone(), b.clone()), (b.clone(), c.clone())] {
        handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: from,
                to_node: to,
                condition: "pass".to_string(),
                route: None,
            }))
            .await
            .unwrap();
    }
    assert_eq!(db.list_graph_edges(&spec.id).unwrap().len(), 2);

    // `b` (not the entry point — `a` is) sits between two edges; deleting
    // it must drop both, not just the ones naming it as `from_node`.
    let deleted = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: b.clone() }))
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));

    assert!(db.get_graph_node(&b).unwrap().is_none());
    assert!(db.list_graph_edges(&spec.id).unwrap().is_empty());
}

#[tokio::test]
async fn graph_delete_node_rejects_the_graphs_entry_point() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;
    handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a.clone(),
            to_node: b,
            condition: "pass".to_string(),
            route: None,
        }))
        .await
        .unwrap();

    let result = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: a.clone() }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(db.get_graph_node(&a).unwrap().is_some());
}

/// CB52 NFR1(a): deleting the ensemble's primary entry node is refused
/// and names the ensemble, the role, and the detaching verbs. Would
/// still pass with the feature removed only if deletion succeeded — it
/// must not.
#[tokio::test]
async fn graph_delete_node_refuses_entry_from_node_referenced_by_ensemble() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

    let result = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams {
            node_id: entry.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(msg.contains("entry_from_node"), "{msg}");
    assert!(msg.contains("Reviewers"), "{msg}");
    assert!(msg.contains(&ensemble_id), "{msg}");
    assert!(msg.contains("remove_entry_from"), "{msg}");
    assert!(msg.contains("from_node"), "{msg}");

    // Nothing was deleted: the node and the ensemble both survive.
    assert!(db.get_graph_node(&entry).unwrap().is_some());
    assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
}

/// CB52 NFR1(a-extra): same refusal for an extra entry source added via
/// `add_entry_from` — role `entry_source`.
#[tokio::test]
async fn graph_delete_node_refuses_extra_entry_source() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let alt = add_agent_node(&handler, &spec.id, "Alt").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

    let added = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
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
            add_entry_from: Some(alt.clone()),
            add_entry_condition: None,
            remove_entry_from: None,
            commit_rights: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let result = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams {
            node_id: alt.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(msg.contains("entry_source"), "{msg}");
    assert!(msg.contains("Reviewers"), "{msg}");
    assert!(msg.contains(&ensemble_id), "{msg}");

    assert!(db.get_graph_node(&alt).unwrap().is_some());
    assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
}

/// CB52 NFR1(b): deleting the pass-exit target is refused as `on_pass_to`.
#[tokio::test]
async fn graph_delete_node_refuses_on_pass_to() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

    let result = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams {
            node_id: arbiter.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(msg.contains("on_pass_to"), "{msg}");
    assert!(msg.contains("Reviewers"), "{msg}");
    assert!(msg.contains(&ensemble_id), "{msg}");

    assert!(db.get_graph_node(&arbiter).unwrap().is_some());
    assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
}

/// CB52 NFR1(b): deleting the fail-exit target is refused as `on_fail_to`.
#[tokio::test]
async fn graph_delete_node_refuses_on_fail_to() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let fallback = add_agent_node(&handler, &spec.id, "Fallback").await;
    let ensemble_id = add_cb52_ensemble(
        &handler,
        &spec.id,
        "Reviewers",
        &entry,
        &arbiter,
        Some(&fallback),
    )
    .await;

    let result = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams {
            node_id: fallback.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(msg.contains("on_fail_to"), "{msg}");
    assert!(msg.contains("Reviewers"), "{msg}");
    assert!(msg.contains(&ensemble_id), "{msg}");

    assert!(db.get_graph_node(&fallback).unwrap().is_some());
    assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
}

/// CB52 NFR1(c): detaching the primary entry via `remove_entry_from`
/// repoints `entry_from_node` at a remaining source (FR3), after which
/// the old primary deletes cleanly and the ensemble survives.
#[tokio::test]
async fn after_remove_entry_from_primary_repoints_and_allows_deletion() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let alt = add_agent_node(&handler, &spec.id, "Alt").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;

    let added = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
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
            add_entry_from: Some(alt.clone()),
            add_entry_condition: None,
            remove_entry_from: None,
            commit_rights: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&added), "{}", text(&added));

    let detached = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            mutex: None,
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
            remove_entry_from: Some(entry.clone()),
            commit_rights: None,

            crew: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&detached), "{}", text(&detached));
    assert_eq!(
        db.get_ensemble(&ensemble_id)
            .unwrap()
            .unwrap()
            .entry_from_node,
        alt
    );

    let deleted = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams {
            node_id: entry.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    assert!(db.get_graph_node(&entry).unwrap().is_none());
    assert!(db.get_ensemble(&ensemble_id).unwrap().is_some());
}

/// CB52: an ensemble's own member is still refused with the owned-node
/// message (FR2 second half) — not with the FR1 reference message.
#[tokio::test]
async fn graph_delete_node_still_refuses_ensemble_owned_nodes() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_cb52_ensemble(&handler, &spec.id, "Reviewers", &entry, &arbiter, None).await;
    let member_id = db
        .get_ensemble_details(&ensemble_id)
        .unwrap()
        .unwrap()
        .members[0]
        .node_id
        .clone();

    let result = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams {
            node_id: member_id.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&result), "{}", text(&result));
    let msg = text(&result);
    assert!(msg.contains("is a member of ensemble"), "{msg}");
    assert!(db.get_graph_node(&member_id).unwrap().is_some());
}

/// CB52 NFR1(d): `graph_preflight` on a graph with an orphan join
/// (simulated pre-CB52 damage: a `join` row with no ensemble row)
/// reports it instead of spending probe quota.
#[tokio::test]
async fn graph_preflight_reports_orphan_join() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    db.insert_graph_node(&GraphNode {
        id: "orphan-join".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "orphaned quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "gone"}),
        position: 99,
        created_at: chrono::Utc::now(),
    })
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
    assert!(msg.contains("belongs to no ensemble"), "{msg}");
    assert!(msg.contains("orphaned quorum"), "{msg}");
    assert!(msg.contains("orphan-join"), "{msg}");
}

/// CB52 FR4: `graph_get` surfaces the same orphan under
/// `orphan_join_warnings` without failing.
#[tokio::test]
async fn graph_get_includes_orphan_join_warnings() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    db.insert_graph_node(&GraphNode {
        id: "orphan-join".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "orphaned quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "gone"}),
        position: 99,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("orphan_join_warnings"), "{body}");
    assert!(body.contains("orphan-join"), "{body}");
    assert!(body.contains("belongs to no ensemble"), "{body}");
}

#[tokio::test]
async fn graph_get_running_without_run_or_schedule_warns() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    db.update_graph_status(&lp.id, GraphStatus::Running, Some(chrono::Utc::now()), None)
        .unwrap();

    let result = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("no node run in progress"), "{body}");
}

#[tokio::test]
async fn graph_get_running_with_live_run_does_not_warn() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    db.insert_graph_node(&GraphNode {
        id: "node".to_string(),
        spec_id: Some(spec.id.clone()),
        graph_id: None,
        name: "node".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.update_graph_status(&lp.id, GraphStatus::Running, Some(chrono::Utc::now()), None)
        .unwrap();
    db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
        id: "run-live".to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec.id.clone(),
        node_id: "node".to_string(),
        status: crate::domain::graphs::GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();

    let result = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("\"consistency_warning\": null"), "{body}");
}

#[tokio::test]
async fn graph_get_running_with_scheduled_resume_does_not_warn() {
    let (dir, db, handler) = endpoint_test_handler();
    // Build a running graph with an autorun schedule directly.
    let lp = Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: uuid::Uuid::new_v4().to_string(),
        name: "Test Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
        status: GraphStatus::Running,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: Some(chrono::Utc::now()),
        completed_at: None,
        autorun_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    };
    db.insert_graph(&lp).unwrap();

    let result = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("\"consistency_warning\": null"), "{body}");
}

#[tokio::test]
async fn topology_mutations_are_rejected_while_the_graph_is_running() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;
    let b = add_agent_node(&handler, &spec.id, "B").await;
    let c = add_agent_node(&handler, &spec.id, "C").await;
    handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: a,
            to_node: b.clone(),
            condition: "pass".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    let edge_id = db.list_graph_edges(&spec.id).unwrap()[0].id.clone();

    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();

    let retarget = handler
        .graph_update_edge(Parameters(GraphUpdateEdgeParams {
            edge_id: edge_id.clone(),
            condition: "pass".to_string(),
            route: None,
            to_node: Some(c),
        }))
        .await
        .unwrap();
    assert!(is_err(&retarget));
    assert!(text(&retarget).contains("running"), "{}", text(&retarget));

    let delete_edge = handler
        .graph_delete_edge(Parameters(GraphDeleteEdgeParams {
            edge_id: edge_id.clone(),
        }))
        .await
        .unwrap();
    assert!(is_err(&delete_edge));
    assert!(
        text(&delete_edge).contains("running"),
        "{}",
        text(&delete_edge)
    );

    let delete_node = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: b.clone() }))
        .await
        .unwrap();
    assert!(is_err(&delete_node));
    assert!(
        text(&delete_node).contains("running"),
        "{}",
        text(&delete_node)
    );

    // Nothing actually mutated while the graph was running.
    assert_eq!(db.get_graph_edge(&edge_id).unwrap().unwrap().to_node, b);
    assert!(db.get_graph_node(&b).unwrap().is_some());
}

#[tokio::test]
async fn cm15_running_graph_pins_node_kind_and_position_accepts_config() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let a = add_agent_node(&handler, &spec.id, "A").await;

    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();

    let kind_res = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: a.clone(),
            name: None,
            kind: Some("check".to_string()),
            config: None,
            config_replace: None,
            position: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&kind_res));
    assert!(text(&kind_res).contains("running"), "{}", text(&kind_res));

    let pos_res = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: a.clone(),
            name: None,
            kind: None,
            config: None,
            config_replace: None,
            position: Some(99),
        }))
        .await
        .unwrap();
    assert!(is_err(&pos_res));
    assert!(text(&pos_res).contains("running"), "{}", text(&pos_res));

    // The unpinned field (config) is still accepted and actually lands.
    let cfg = serde_json::json!({ "platform": "codex" })
        .as_object()
        .unwrap()
        .clone();
    let cfg_res = handler
        .graph_update_node(Parameters(GraphUpdateNodeParams {
            node_id: a.clone(),
            name: None,
            kind: None,
            config: Some(cfg),
            config_replace: None,
            position: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&cfg_res), "{}", text(&cfg_res));
    assert_eq!(
        db.get_graph_node(&a)
            .unwrap()
            .unwrap()
            .config
            .get("platform")
            .and_then(|v| v.as_str()),
        Some("codex"),
    );
}

#[tokio::test]
async fn cm15_running_graph_pins_ensemble_kind_accepts_member_replacement() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ens = add_two_member_ensemble(&handler, &spec.id, "Ens", &entry, &arbiter).await;

    db.update_graph_status(&lp.id, GraphStatus::Running, None, None)
        .unwrap();

    let kind_res = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            kind: Some("cascade".to_string()),
            ..blank_ensemble_update(&ens)
        }))
        .await
        .unwrap();
    assert!(is_err(&kind_res));
    assert!(text(&kind_res).contains("running"), "{}", text(&kind_res));

    // Exit wiring is topology too — retargeting it mid-run is refused,
    // not accepted-and-dropped.
    let exit_res = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            on_pass_to: Some(entry.clone()),
            ..blank_ensemble_update(&ens)
        }))
        .await
        .unwrap();
    assert!(is_err(&exit_res));
    assert!(text(&exit_res).contains("running"), "{}", text(&exit_res));

    // Replacing the dead member set IS allowed while running.
    let members_res = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "opencode".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "opencode".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            ..blank_ensemble_update(&ens)
        }))
        .await
        .unwrap();
    assert!(!is_err(&members_res), "{}", text(&members_res));
}

#[tokio::test]
async fn graph_add_node_router_created_persisted_and_read_back_with_routes() {
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
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&router_result), "{}", text(&router_result));
    let router_id = extract_id(&router_result, "node_id");

    let retry_target = add_agent_node(&handler, &spec.id, "Retry target").await;
    let escalate_target = add_agent_node(&handler, &spec.id, "Escalate target").await;

    for (route, target) in [("retry", &retry_target), ("escalate", &escalate_target)] {
        let edge = handler
            .graph_add_edge(Parameters(GraphAddEdgeParams {
                spec_id: Some(spec.id.clone()),
                graph_id: None,
                from_node: router_id.clone(),
                to_node: target.clone(),
                condition: "route".to_string(),
                route: Some(route.to_string()),
            }))
            .await
            .unwrap();
        assert!(!is_err(&edge), "{}", text(&edge));
    }

    let got = handler
        .graph_get(Parameters(GraphGetParams {
            graph_id: lp.id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let json: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    let nodes = json["specs"][0]["nodes"].as_array().unwrap();
    let router_json = nodes
        .iter()
        .find(|n| n["id"] == router_id)
        .expect("router node present in graph_get");
    assert_eq!(router_json["kind"], "router");
    let routes = router_json["routes"].as_array().expect("routes array");
    assert_eq!(routes.len(), 2);
    let retry_route = routes
        .iter()
        .find(|r| r["label"] == "retry")
        .expect("retry route present");
    assert_eq!(retry_route["fallback"], true);
    assert_eq!(retry_route["to_node"], retry_target);
    let escalate_route = routes
        .iter()
        .find(|r| r["label"] == "escalate")
        .expect("escalate route present");
    assert_eq!(escalate_route["fallback"], false);
    assert_eq!(escalate_route["to_node"], escalate_target);
}

#[tokio::test]
async fn graph_add_node_router_rejects_fewer_than_two_routes_and_missing_fallback() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);

    let too_few = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Router".to_string(),
            kind: Some("router".to_string()),
            config: Some(router_node_config(
                &serde_json::json!([{ "label": "retry", "description": "Retry." }]),
                "retry",
            )),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&too_few));
    assert!(text(&too_few).contains("at least 2 routes"));

    let mut no_fallback_config = router_node_config(&two_routes(), "retry");
    no_fallback_config.remove("fallback");
    let no_fallback = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: Some(spec.id),
            graph_id: None,
            name: "Router".to_string(),
            kind: Some("router".to_string()),
            config: Some(no_fallback_config),
            blueprint: None,
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&no_fallback));
    assert!(text(&no_fallback).contains("fallback"));
}

#[tokio::test]
async fn graph_add_edge_route_condition_rejects_undeclared_route_and_non_router_source() {
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
            node: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let router_id = extract_id(&router_result, "node_id");
    let target = add_agent_node(&handler, &spec.id, "Target").await;

    let undeclared = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: router_id.clone(),
            to_node: target.clone(),
            condition: "route".to_string(),
            route: Some("nonexistent".to_string()),
        }))
        .await
        .unwrap();
    assert!(is_err(&undeclared));
    assert!(text(&undeclared).contains("undeclared route"));

    let non_router = add_agent_node(&handler, &spec.id, "Non-router source").await;
    let wrong_source = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id),
            graph_id: None,
            from_node: non_router,
            to_node: target,
            condition: "route".to_string(),
            route: Some("retry".to_string()),
        }))
        .await
        .unwrap();
    assert!(is_err(&wrong_source));
    assert!(text(&wrong_source).contains("router node"));
}
