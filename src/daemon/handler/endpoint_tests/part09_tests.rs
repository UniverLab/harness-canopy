//! Part 09 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// Member and quorum nodes still refuse direct wiring — and the refusals
/// now name the ensemble-level surface that does the job.
#[tokio::test]
async fn graph_add_edge_still_refuses_ensemble_nodes_with_helpful_message() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;
    let ensemble_id =
        add_two_member_ensemble(&handler, &spec.id, "Guarded", &entry, &arbiter).await;
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    let member_id = details.members[0].node_id.clone();
    let join_id = details.ensemble.join_node_id.clone();

    let into_member = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: entry.clone(),
            to_node: member_id.clone(),
            condition: "always".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&into_member));
    let member_msg = text(&into_member);
    assert!(member_msg.contains("graph_update_ensemble"), "{member_msg}");
    assert!(member_msg.contains("graph_delete_ensemble"), "{member_msg}");

    let out_of_quorum = handler
        .graph_add_edge(Parameters(GraphAddEdgeParams {
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            from_node: join_id.clone(),
            to_node: arbiter.clone(),
            condition: "pass".to_string(),
            route: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&out_of_quorum));
    assert!(text(&out_of_quorum).contains("graph_update_ensemble"));

    let delete_member = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: member_id }))
        .await
        .unwrap();
    assert!(is_err(&delete_member));
    assert!(text(&delete_member).contains("graph_delete_ensemble"));

    let delete_quorum = handler
        .graph_delete_node(Parameters(GraphDeleteNodeParams { node_id: join_id }))
        .await
        .unwrap();
    assert!(is_err(&delete_quorum));
    assert!(text(&delete_quorum).contains("graph_delete_ensemble"));
}

// ── intelligence_upsert / intelligence_search / intelligence_graph_walk

#[tokio::test]
async fn intelligence_upsert_search_and_graph_walk() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-1");

    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("fact-1".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Rust is memory safe".to_string()),
                    body: Some("Ownership rules prevent data races.".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let linked = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("fact-2".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Ownership rules".to_string()),
                    body: Some("One owner at a time.".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: Some(vec![IntelligenceRelationParams {
                        to_node_id: "fact-1".to_string(),
                        relation: "extends".to_string(),
                        weight: Some(1.0),
                    }]),
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&linked), "{}", text(&linked));

    let searched = handler
        .intelligence_search(
            Parameters(IntelligenceSearchParams {
                query: "memory safe".to_string(),
                kind: Some("fact".to_string()),
                limit: Some(5),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(raw_text(&searched).contains("fact-1"));

    let walked = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "fact-2".to_string(),
            depth: Some(2),
            compact: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&walked), "{}", text(&walked));
    assert!(raw_text(&walked).contains("fact-1"));

    let missing_root = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "ghost-node".to_string(),
            depth: None,
            compact: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing_root));
}

// ── intelligence_delete_node / intelligence_delete_relation ──────────

#[tokio::test]
async fn intelligence_upsert_partial_update_via_mcp() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-partial-1");

    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("partial-mcp-1".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Original title".to_string()),
                    body: Some("Original body text".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    // A call carrying only id + title changes the title and leaves
    // everything else exactly as it was.
    let updated = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("partial-mcp-1".to_string()),
                    kind: None,
                    status: None,
                    title: Some("Corrected title".to_string()),
                    body: None,
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&updated)).unwrap();
    assert_eq!(value["created"], false);
    let changed: Vec<String> = serde_json::from_value(value["changed_fields"].clone()).unwrap();
    assert!(
        changed.contains(&"title".to_string()),
        "changed_fields should name title: {changed:?}"
    );
    assert!(
        !changed.contains(&"body".to_string()),
        "changed_fields should not name untouched body: {changed:?}"
    );
    assert_eq!(value["node"]["title"], "Corrected title");
    assert_eq!(value["node"]["body"], "Original body text");
}

#[tokio::test]
async fn intelligence_upsert_body_replace_via_mcp() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-partial-2");

    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("hooks-mcp-1".to_string()),
                    kind: Some("decision".to_string()),
                    status: None,
                    title: Some("Scope".to_string()),
                    body: Some(
                        "Scope notes.\nfuera de esta versión: hooks internos\nMore notes."
                            .to_string(),
                    ),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let updated = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("hooks-mcp-1".to_string()),
                    kind: None,
                    status: None,
                    title: None,
                    body: None,
                    body_replace: Some(crate::daemon::params::BodyReplaceParams {
                        fragment: "fuera de esta versión: hooks internos".to_string(),
                        replacement: "dentro de esta versión: hooks internos".to_string(),
                    }),
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&updated)).unwrap();
    assert_eq!(value["created"], false);
    let changed: Vec<String> = serde_json::from_value(value["changed_fields"].clone()).unwrap();
    assert!(
        changed.contains(&"body".to_string()),
        "changed_fields should name body: {changed:?}"
    );
    assert_eq!(
        value["node"]["body"],
        "Scope notes.\ndentro de esta versión: hooks internos\nMore notes."
    );

    // A fragment that matches twice is refused instead of guessed.
    let err = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("hooks-mcp-1".to_string()),
                    kind: None,
                    status: None,
                    title: None,
                    body: None,
                    body_replace: Some(crate::daemon::params::BodyReplaceParams {
                        fragment: "notes".to_string(),
                        replacement: "remarks".to_string(),
                    }),
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .expect_err("ambiguous fragment must fail the write");
    assert!(
        format!("{err:?}").contains("ambiguous"),
        "refusal should say ambiguous: {err:?}"
    );
}

#[tokio::test]
async fn intelligence_delete_node_removes_node_and_reports_relations() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-delete-1");

    upsert_intel_fact(&handler, "delete-a", "Node A", None).await;
    upsert_intel_fact(
        &handler,
        "delete-b",
        "Node B",
        Some(vec![IntelligenceRelationParams {
            to_node_id: "delete-a".to_string(),
            relation: "extends".to_string(),
            weight: None,
        }]),
    )
    .await;
    upsert_intel_fact(
        &handler,
        "delete-c",
        "Node C",
        Some(vec![IntelligenceRelationParams {
            to_node_id: "delete-b".to_string(),
            relation: "extends".to_string(),
            weight: None,
        }]),
    )
    .await;

    // "delete-b" sits in the middle of a -> b, b -> c: two edges touch it.
    let deleted = handler
        .intelligence_delete_node(
            Parameters(IntelligenceDeleteNodeParams {
                node_id: "delete-b".to_string(),
                project_hash: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));
    let body = raw_text(&deleted);
    assert!(body.contains("delete-b"));
    assert!(body.contains("\"relations_removed\": 2"));

    let searched = handler
        .intelligence_search(
            Parameters(IntelligenceSearchParams {
                query: "Node B".to_string(),
                kind: None,
                limit: Some(5),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!raw_text(&searched).contains("delete-b"));

    // Neither surviving neighbor's graph walk should error or reference
    // the removed middle node.
    let from_a = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "delete-a".to_string(),
            depth: Some(3),
            compact: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&from_a), "{}", text(&from_a));
    assert!(!raw_text(&from_a).contains("delete-b"));

    let from_c = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "delete-c".to_string(),
            depth: Some(3),
            compact: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&from_c), "{}", text(&from_c));
    assert!(!raw_text(&from_c).contains("delete-b"));
}

#[tokio::test]
async fn intelligence_delete_node_missing_id_returns_error_not_silent_success() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-delete-2");

    let deleted = handler
        .intelligence_delete_node(
            Parameters(IntelligenceDeleteNodeParams {
                node_id: "ghost-node".to_string(),
                project_hash: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&deleted));
}

#[tokio::test]
async fn intelligence_delete_relation_removes_edge_leaves_nodes_intact() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-delete-3");

    upsert_intel_fact(&handler, "rel-a", "Node A", None).await;
    upsert_intel_fact(
        &handler,
        "rel-b",
        "Node B",
        Some(vec![IntelligenceRelationParams {
            to_node_id: "rel-a".to_string(),
            relation: "extends".to_string(),
            weight: None,
        }]),
    )
    .await;

    let walked = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "rel-b".to_string(),
            depth: Some(1),
            compact: None,
        }))
        .await
        .unwrap();
    let edge_id = serde_json::from_str::<serde_json::Value>(&raw_text(&walked))
        .unwrap()
        .get("edges")
        .and_then(|edges| edges.as_array())
        .and_then(|edges| edges.first())
        .and_then(|edge| edge.get("id"))
        .and_then(|id| id.as_i64())
        .expect("edge id present in graph walk response");

    let deleted = handler
        .intelligence_delete_relation(
            Parameters(IntelligenceDeleteRelationParams {
                edge_id,
                project_hash: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&deleted), "{}", text(&deleted));

    let walked_after = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "rel-b".to_string(),
            depth: Some(1),
            compact: None,
        }))
        .await
        .unwrap();
    assert!(!raw_text(&walked_after).contains("rel-a"));

    // Both endpoint nodes survive the relation delete.
    let searched = handler
        .intelligence_search(
            Parameters(IntelligenceSearchParams {
                query: "Node".to_string(),
                kind: Some("fact".to_string()),
                limit: Some(10),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    let search_body = raw_text(&searched);
    assert!(search_body.contains("rel-a"));
    assert!(search_body.contains("rel-b"));

    let missing = handler
        .intelligence_delete_relation(
            Parameters(IntelligenceDeleteRelationParams {
                edge_id,
                project_hash: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&missing));
}

#[tokio::test]
async fn intelligence_get_context_light_and_full() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-ctx-1");

    let bad_scope = handler
        .intelligence_get_context(
            Parameters(IntelligenceGetContextParams {
                scope: "sideways".to_string(),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&bad_scope));

    let light = handler
        .intelligence_get_context(
            Parameters(IntelligenceGetContextParams {
                scope: "light".to_string(),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&light), "{}", text(&light));
    assert!(raw_text(&light).contains("\"scope\": \"light\""));

    let full = handler
        .intelligence_get_context(
            Parameters(IntelligenceGetContextParams {
                scope: "full".to_string(),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&full), "{}", text(&full));
    assert!(raw_text(&full).contains("\"scope\": \"full\""));
}

// ── intelligence_list_projects / intelligence_link_projects ────

#[tokio::test]
async fn intelligence_list_and_link_projects() {
    let (_dir, db, handler) = endpoint_test_handler();
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    db.register_project_path(dir_a.path()).unwrap();
    db.register_project_path(dir_b.path()).unwrap();
    let hash_a = crate::domain::project::workdir_hash(&dir_a.path().to_string_lossy());
    let hash_b = crate::domain::project::workdir_hash(&dir_b.path().to_string_lossy());

    let listed = handler
        .intelligence_list_projects(Parameters(IntelligenceListProjectsParams {
            query: None,
            limit: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&listed));

    let empty_fields = handler
        .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
            from_project_hash: "  ".to_string(),
            to_project_hash: hash_b.clone(),
            relation: None,
            weight: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&empty_fields));

    let linked = handler
        .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
            from_project_hash: hash_a,
            to_project_hash: hash_b,
            relation: Some("depends_on".to_string()),
            weight: Some(0.5),
        }))
        .await
        .unwrap();
    assert!(!is_err(&linked), "{}", text(&linked));
    assert!(raw_text(&linked).contains("depends_on"));
}

#[tokio::test]
async fn intelligence_link_rejects_unknown_relation_via_mcp() {
    let (_dir, db, handler) = endpoint_test_handler();
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();
    db.register_project_path(dir_a.path()).unwrap();
    db.register_project_path(dir_b.path()).unwrap();
    let hash_a = crate::domain::project::workdir_hash(
        &std::fs::canonicalize(dir_a.path())
            .unwrap()
            .to_string_lossy(),
    );
    let hash_b = crate::domain::project::workdir_hash(
        &std::fs::canonicalize(dir_b.path())
            .unwrap()
            .to_string_lossy(),
    );

    let bad = handler
        .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
            from_project_hash: hash_a.clone(),
            to_project_hash: hash_b.clone(),
            relation: Some("blocks".to_string()),
            weight: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&bad), "unknown relation must be an error_result");
    assert!(raw_text(&bad).contains("allowed:"));

    let derived = handler
        .intelligence_link_projects(Parameters(IntelligenceLinkProjectsParams {
            from_project_hash: hash_a,
            to_project_hash: hash_b,
            relation: Some("contains".to_string()),
            weight: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&derived));
    assert!(raw_text(&derived).contains("derived"));
}

#[tokio::test]
async fn intelligence_get_context_traverses_and_warns() {
    let (_dir, db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cm9-ctx-1");
    let (_base, parent_hash, child_hash) = cm9_seed_parent_child(&db);
    // A third project depending on the parent (impact warning source).
    let dep_dir = tempdir().unwrap();
    let depender = db.register_project_path(dep_dir.path()).unwrap();
    db.link_projects(&depender.hash, &parent_hash, "depends_on", None)
        .unwrap();

    let full = handler
        .intelligence_get_context(
            Parameters(IntelligenceGetContextParams {
                scope: "full".to_string(),
                project_hash: Some(parent_hash.clone()),
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&full), "{}", text(&full));
    let body = raw_text(&full);
    // Parent fact direct + child fact inherited via outbound contains.
    assert!(body.contains("cm9-fact-parent"));
    assert!(body.contains("cm9-fact-child"));
    assert!(body.contains("inherited:contains"));
    assert!(body.contains("\"via_project\""));
    assert!(body.contains("\"projects_reached\""));
    // Impact warning names the depender.
    assert!(body.contains("\"dependency_impact\""));
    assert!(body.contains(&depender.hash));

    let light = handler
        .intelligence_get_context(
            Parameters(IntelligenceGetContextParams {
                scope: "light".to_string(),
                project_hash: Some(child_hash),
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&light), "{}", text(&light));
    let light_body = raw_text(&light);
    assert!(light_body.contains("cm9-fact-child"));
    assert!(
        !light_body.contains("\"dependency_impact\""),
        "light scope must not warn"
    );
}

#[tokio::test]
async fn intelligence_search_scoped() {
    let (_dir, db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cm9-search-1");
    let (_base, parent_hash, child_hash) = cm9_seed_parent_child(&db);

    let scoped = handler
        .intelligence_search(
            Parameters(IntelligenceSearchParams {
                query: "billing rulebook".to_string(),
                kind: Some("fact".to_string()),
                limit: Some(10),
                project_hash: Some(child_hash.clone()),
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&scoped), "{}", text(&scoped));
    let body = raw_text(&scoped);
    assert!(body.contains("cm9-fact-child"));
    assert!(body.contains("cm9-fact-parent"));
    assert!(body.contains("\"via_relation\""));
    assert!(body.contains("\"projects_reached\""));

    // Depth 0 sees only the child project itself.
    let narrow = handler
        .intelligence_search(
            Parameters(IntelligenceSearchParams {
                query: "billing rulebook".to_string(),
                kind: Some("fact".to_string()),
                limit: Some(10),
                project_hash: Some(child_hash),
                depth: Some(0),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    let narrow_body = raw_text(&narrow);
    assert!(narrow_body.contains("cm9-fact-child"));
    assert!(!narrow_body.contains("cm9-fact-parent"));
    let _ = parent_hash;
}

#[tokio::test]
async fn project_register_tool_registers_without_marker() {
    let (_dir, db, handler) = endpoint_test_handler();
    let base = tempdir().unwrap();
    let target = base.path().join("explicit-proj");
    std::fs::create_dir_all(&target).unwrap();
    assert!(!target.join(".canopy-project").exists());

    let registered = handler
        .project_register(Parameters(ProjectRegisterParams {
            path: target.to_string_lossy().to_string(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&registered), "{}", text(&registered));
    assert!(raw_text(&registered).contains("\"workdir_hash\""));

    let canonical = std::fs::canonicalize(&target)
        .unwrap()
        .to_string_lossy()
        .to_string();
    let expected = crate::domain::project::workdir_hash(&canonical);
    assert!(db.get_project(&expected).unwrap().is_some());
}

// ── get_tools ────────────────────────────────────────────────

#[tokio::test]
async fn get_tools_returns_scoped_protocol() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-tools-1");

    let bad_scope = handler
        .get_tools(
            Parameters(GetToolsParams {
                scope: "sideways".to_string(),
                path: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(is_err(&bad_scope));

    let file_write = handler
        .get_tools(
            Parameters(GetToolsParams {
                scope: "file_write".to_string(),
                path: Some("/tmp/a.rs".to_string()),
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&file_write));
    assert!(raw_text(&file_write).contains("/tmp/a.rs"));

    let session_start = handler
        .get_tools(
            Parameters(GetToolsParams {
                scope: "session_start".to_string(),
                path: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&session_start));
    assert!(raw_text(&session_start).contains("session_start"));
}

#[tokio::test]
async fn get_tools_session_start_carries_own_session_id() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-session-start-1");

    let result = handler
        .get_tools(
            Parameters(GetToolsParams {
                scope: "session_start".to_string(),
                path: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert_eq!(value["session_id"].as_str(), Some("agent-session-start-1"));
    assert!(value["session_hint"]
        .as_str()
        .is_some_and(|hint| hint.contains("target_session_id")));
}

#[tokio::test]
async fn get_tools_without_sync_identity_still_returns_protocol() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::clear();

    let result = handler
        .get_tools(
            Parameters(GetToolsParams {
                scope: "session_start".to_string(),
                path: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let value: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert_eq!(value["scope"].as_str(), Some("session_start"));
    assert!(value.get("session_id").is_none());
}

// ── project_search / project_update ─────────────────────────

#[tokio::test]
async fn project_search_and_update() {
    let (_dir, db, handler) = endpoint_test_handler();
    let base = tempdir().unwrap();
    let project_dir = base.path().join("searchable-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let project = db.register_project_path(&project_dir).unwrap();

    let no_match = handler
        .project_search(Parameters(ProjectSearchParams {
            query: "nothing-matches-this-xyz".to_string(),
        }))
        .await
        .unwrap();
    assert!(text(&no_match).contains("No projects found"));

    let found = handler
        .project_search(Parameters(ProjectSearchParams {
            query: "searchable-project".to_string(),
        }))
        .await
        .unwrap();
    assert!(raw_text(&found).contains(&project.hash));

    let updated = handler
        .project_update(Parameters(ProjectUpdateParams {
            project_hash: project.hash.clone(),
            description: Some("A test project".to_string()),
            tags: Some(vec!["rust".to_string()]),
        }))
        .await
        .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let missing = handler
        .project_update(Parameters(ProjectUpdateParams {
            project_hash: "not-a-real-hash".to_string(),
            description: Some("x".to_string()),
            tags: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&missing));
}

#[tokio::test]
async fn project_update_unlocks_gardener_after_five_projects() {
    let (_dir, db, handler) = endpoint_test_handler();
    let base = tempdir().unwrap();
    let mut hashes = Vec::new();
    for i in 0..5 {
        let dir = base.path().join(format!("gardener-project-{i}"));
        std::fs::create_dir_all(&dir).unwrap();
        let project = db.register_project_path(&dir).unwrap();
        hashes.push(project.hash);
    }

    // The same project twice counts once.
    for _ in 0..2 {
        let updated = handler
            .project_update(Parameters(ProjectUpdateParams {
                project_hash: hashes[0].clone(),
                description: Some("A tended garden".to_string()),
                tags: None,
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
    }
    assert_eq!(db.count_gardener_projects().unwrap(), 1);

    // Tags-only updates count too; four more distinct projects unlock.
    for hash in hashes.iter().skip(1) {
        let updated = handler
            .project_update(Parameters(ProjectUpdateParams {
                project_hash: hash.clone(),
                description: None,
                tags: Some(vec!["tended".to_string()]),
            }))
            .await
            .unwrap();
        assert!(!is_err(&updated), "{}", text(&updated));
    }
    assert_eq!(db.count_gardener_projects().unwrap(), 5);
}

// ── project_remap ─────────────────────────────────────────────

#[tokio::test]
async fn project_remap_moves_project_to_new_path() {
    let (_dir, db, handler) = endpoint_test_handler();
    let base = tempdir().unwrap();
    let old_dir = base.path().join("cadforge");
    let new_dir = base.path().join("cadspec");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::create_dir_all(&new_dir).unwrap();
    let project = db.register_project_path(&old_dir).unwrap();
    db.insert_terminal_session("t-1", "t-1", "bash", &project.path)
        .unwrap();

    let result = handler
        .project_remap(Parameters(ProjectRemapParams {
            project_hash: project.hash.clone(),
            new_path: new_dir.to_string_lossy().to_string(),
            dry_run: None,
            force: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("\"kind\": \"move\""));
    assert!(body.contains("\"rows_moved\": 2"));

    assert!(db.get_project(&project.hash).unwrap().is_none());
    let new_canonical = std::fs::canonicalize(&new_dir).unwrap();
    let moved = db
        .get_project_by_path(&new_canonical)
        .unwrap()
        .expect("project moved to new path");
    assert_eq!(
        db.project_dependent_counts(&moved.path)
            .unwrap()
            .terminal_sessions,
        1
    );
}

#[tokio::test]
async fn project_remap_dry_run_changes_nothing() {
    let (_dir, db, handler) = endpoint_test_handler();
    let base = tempdir().unwrap();
    let old_dir = base.path().join("old-loc");
    let new_dir = base.path().join("new-loc");
    std::fs::create_dir_all(&old_dir).unwrap();
    std::fs::create_dir_all(&new_dir).unwrap();
    let project = db.register_project_path(&old_dir).unwrap();

    let result = handler
        .project_remap(Parameters(ProjectRemapParams {
            project_hash: project.hash.clone(),
            new_path: new_dir.to_string_lossy().to_string(),
            dry_run: Some(true),
            force: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(raw_text(&result).contains("\"dry_run\": true"));

    // Nothing actually moved.
    assert!(db.get_project(&project.hash).unwrap().is_some());
}

#[tokio::test]
async fn project_remap_refuses_missing_path_without_force() {
    let (_dir, db, handler) = endpoint_test_handler();
    let base = tempdir().unwrap();
    let old_dir = base.path().join("still-here");
    std::fs::create_dir_all(&old_dir).unwrap();
    let project = db.register_project_path(&old_dir).unwrap();

    let result = handler
        .project_remap(Parameters(ProjectRemapParams {
            project_hash: project.hash.clone(),
            new_path: "/definitely/does/not/exist/anywhere".to_string(),
            dry_run: None,
            force: None,
        }))
        .await
        .unwrap();
    assert!(is_err(&result));
    assert!(db.get_project(&project.hash).unwrap().is_some());
}

// ── skill_list / skill_get ──

#[tokio::test]
async fn skill_list_empty_and_skill_get_unknown() {
    let (_dir, _db, handler) = endpoint_test_handler();

    let listed = handler.skill_list().await.unwrap();
    assert!(!is_err(&listed));

    let missing = handler
        .skill_get(Parameters(SkillGetParams {
            name: "unknown-skill".to_string(),
        }))
        .await;
    assert!(missing.is_err());
}

// ── intelligence prefix resolution + created field ──────────────

#[tokio::test]
async fn intelligence_upsert_returns_created_true_for_new_node() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-created-1");

    let result = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("brand-new-node-1".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("First time".to_string()),
                    body: Some("body".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(
        body.contains("\"created\": true"),
        "expected created:true in: {body}"
    );
    // An explicit id that matched nothing must produce a loud warning, not
    // just `created: true` (which also fires on the auto-id path).
    assert!(
        body.contains("\"warning\":") && body.contains("brand-new-node-1"),
        "explicit unknown id should warn about creating a new node: {body}"
    );
}

#[tokio::test]
async fn intelligence_upsert_returns_created_false_for_update() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-created-2");

    let first = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("update-me-1".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Original".to_string()),
                    body: Some("v1".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&first), "{}", text(&first));
    assert!(
        raw_text(&first).contains("\"created\": true"),
        "first upsert should be created:true: {}",
        raw_text(&first)
    );

    let second = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("update-me-1".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Updated".to_string()),
                    body: Some("v2".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&second), "{}", text(&second));
    assert!(
        raw_text(&second).contains("\"created\": false"),
        "second upsert should be created:false: {}",
        raw_text(&second)
    );
    // Updating an existing node must not carry the "created a new node"
    // warning.
    assert!(
        !raw_text(&second).contains("\"warning\":"),
        "update path should not warn: {}",
        raw_text(&second)
    );
}

#[tokio::test]
async fn intelligence_upsert_with_prefix_updates_existing() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-prefix-1");

    let full_id = "3a476c63-6b4a-4860-9c18-1c784b40a4b2";
    let create = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some(full_id.to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Original".to_string()),
                    body: Some("v1".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&create), "{}", text(&create));

    let via_prefix = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("3a476c63".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Updated via prefix".to_string()),
                    body: Some("v2".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&via_prefix), "{}", text(&via_prefix));
    let body = raw_text(&via_prefix);
    assert!(
        body.contains("\"created\": false"),
        "prefix upsert should update, not create: {body}"
    );
    assert!(
        body.contains(full_id),
        "response should contain the full UUID: {body}"
    );
    assert!(
        body.contains("Updated via prefix"),
        "response should contain the new title: {body}"
    );

    let searched = handler
        .intelligence_search(
            Parameters(IntelligenceSearchParams {
                query: "Updated via prefix".to_string(),
                kind: Some("fact".to_string()),
                limit: Some(10),
                project_hash: None,
                depth: None,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    let search_body = raw_text(&searched);
    assert!(
        search_body.contains(full_id),
        "the node should exist with updated title: {search_body}"
    );
    assert!(
            search_body.contains("\"count\": 1"),
            "exactly one node must match — the prefix upsert must not have created a duplicate: {search_body}"
        );
}

#[tokio::test]
async fn intelligence_upsert_with_ambiguous_prefix_errors() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-prefix-ambig");

    let id1 = "abc12300-0000-0000-0000-000000000001";
    let id2 = "abc45600-0000-0000-0000-000000000002";
    for id in [id1, id2] {
        let r = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(id.to_string()),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some(format!("Node {id}")),
                        body: Some("body".to_string()),
                        body_replace: None,
                        metadata: None,
                        project_hash: None,
                        session_id: None,
                        relations: None,
                    },
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&r), "{}", text(&r));
    }

    let ambiguous = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("abc".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Should fail".to_string()),
                    body: Some("body".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(
        is_err(&ambiguous),
        "ambiguous prefix should error: {}",
        text(&ambiguous)
    );
    let err_body = raw_text(&ambiguous);
    assert!(
        err_body.contains("Ambiguous"),
        "error should mention Ambiguous: {err_body}"
    );
    assert!(
        err_body.contains(id1),
        "error should list candidate {id1}: {err_body}"
    );
    assert!(
        err_body.contains(id2),
        "error should list candidate {id2}: {err_body}"
    );
}

#[tokio::test]
async fn intelligence_graph_walk_with_prefix() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-walk-prefix");

    let full_id = "deadbeef-0000-0000-0000-000000000001";
    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some(full_id.to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Walk me".to_string()),
                    body: Some("body".to_string()),
                    body_replace: None,
                    metadata: None,
                    project_hash: None,
                    session_id: None,
                    relations: None,
                },
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let walked = handler
        .intelligence_graph_walk(Parameters(IntelligenceGraphWalkParams {
            node_id: "deadbeef".to_string(),
            depth: Some(1),
            compact: None,
        }))
        .await
        .unwrap();
    assert!(
        !is_err(&walked),
        "prefix walk should succeed: {}",
        text(&walked)
    );
    let body = raw_text(&walked);
    assert!(
        body.contains(full_id),
        "walk response should contain the full UUID: {body}"
    );
}
