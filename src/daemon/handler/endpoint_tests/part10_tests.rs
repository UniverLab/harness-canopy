//! Part 10 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[tokio::test]
async fn intelligence_delete_node_with_prefix() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-intel-del-prefix");

    let full_id = "feedface-0000-0000-0000-000000000001";
    let created = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some(full_id.to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("Delete me by prefix".to_string()),
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

    let deleted = call_resource_action(
        &handler,
        "intelligence_admin",
        "delete_node",
        params_id(IntelligenceDeleteNodeParams {
            node_id: "feedface".to_string(),
            project_hash: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&deleted),
        "prefix delete should succeed: {}",
        text(&deleted)
    );
    let body = raw_text(&deleted);
    assert!(
        body.contains(full_id),
        "delete response should contain the full UUID: {body}"
    );

    let walked = call_resource_action(
        &handler,
        "intelligence_admin",
        "walk",
        params_id(IntelligenceGraphWalkParams {
            node_id: full_id.to_string(),
            depth: Some(1),
            compact: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        is_err(&walked),
        "node should be gone after prefix delete: {}",
        text(&walked)
    );
}

#[tokio::test]
async fn spec_list_default_compact_omits_description() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Compact test".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let listed = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: None,
            include_descriptions: None,
            limit: None,
            offset: None,
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    let body = raw_text(&listed);
    assert!(
        !body.contains("Objective") && !body.contains("<objective>"),
        "compact mode must omit description content"
    );
    assert!(body.contains("Compact test"));
}

#[tokio::test]
async fn spec_list_with_include_descriptions_returns_description() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let desc = valid_spec_description();
    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Verbose test".to_string(),
            description: desc.clone(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let listed = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: None,
            include_descriptions: Some(true),
            limit: None,
            offset: None,
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    let body = raw_text(&listed);
    assert!(
        body.contains("<objective>") || body.contains("Objective"),
        "include_descriptions must include description content"
    );
}

// ── CB28: result budget contract for graph walk + spec_list ──────────

#[tokio::test]
async fn intelligence_upsert_duplicate_candidates_fit_budget_and_omit_body() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cb45-fit");

    // Three large-bodied existing nodes, all citing the same file:line so
    // all three are flagged as duplicates of the node upserted below.
    for i in 0..3 {
        let body = format!(
            "see src/cb45-shared.rs:1 for details. {}",
            "y".repeat(12_000)
        );
        let result = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(format!("cb45-fit-{i}")),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some(format!("CB45 fit node {i}")),
                        body: Some(body),
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
    }

    let new_body = format!("also see src/cb45-shared.rs:1. {}", "z".repeat(12_000));
    let upserted = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("cb45-fit-new".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("CB45 fit new node".to_string()),
                    body: Some(new_body.clone()),
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
    assert!(!is_err(&upserted), "{}", text(&upserted));

    let body = raw_text(&upserted);
    assert!(
        body.len() < MCP_RESULT_BUDGET_BYTES,
        "upsert response ({} bytes) with 3 large duplicate candidates must fit the {}-byte budget",
        body.len(),
        MCP_RESULT_BUDGET_BYTES
    );

    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

    // The written node itself is returned complete — requirement 4.
    assert_eq!(
        parsed["node"]["body"].as_str().unwrap(),
        new_body,
        "the caller's own write must not be truncated"
    );

    let candidates = parsed["duplicate_candidates"].as_array().unwrap();
    assert_eq!(candidates.len(), 3, "expected all 3 duplicates reported");
    for candidate in candidates {
        assert!(candidate.get("id").is_some());
        assert!(candidate.get("title").is_some());
        assert!(candidate.get("kind").is_some());
        assert!(candidate.get("excerpt").is_some());
        assert!(
            candidate.get("body").is_none(),
            "duplicate candidate must never carry a full body field"
        );
        let excerpt = candidate["excerpt"].as_str().unwrap();
        assert!(
            excerpt.len() < 300,
            "excerpt must be short, got {} chars",
            excerpt.len()
        );
        assert!(
            !excerpt.contains(&"y".repeat(1000)),
            "excerpt must not carry the candidate's large body"
        );
    }

    // Existing fields kept as-is — requirement 5.
    assert!(parsed.get("duplicate_warning").is_some());
    assert_eq!(
        parsed["duplicate_detection_note"].as_str().unwrap(),
        crate::db::intelligence::LEXICAL_DUPLICATE_LIMITATION
    );
}

#[tokio::test]
async fn intelligence_upsert_duplicate_candidates_capped_with_omitted_count() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cb45-cap");

    let total = INTELLIGENCE_DUPLICATE_CANDIDATES_CAP + 5;
    for i in 0..total {
        let result = handler
            .intelligence_upsert(
                Parameters(IntelligenceUpsertParams {
                    node_data: IntelligenceNodeParams {
                        id: Some(format!("cb45-cap-{i:03}")),
                        kind: Some("fact".to_string()),
                        status: None,
                        title: Some(format!("CB45 cap node {i:03}")),
                        body: Some(format!("see src/cb45-cap-{i:03}.rs:1 for details.")),
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
    }

    let citations: String = (0..total)
        .map(|i| format!("see src/cb45-cap-{i:03}.rs:1"))
        .collect::<Vec<_>>()
        .join(" ");
    let upserted = handler
        .intelligence_upsert(
            Parameters(IntelligenceUpsertParams {
                node_data: IntelligenceNodeParams {
                    id: Some("cb45-cap-new".to_string()),
                    kind: Some("fact".to_string()),
                    status: None,
                    title: Some("CB45 cap new node".to_string()),
                    body: Some(citations),
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
    assert!(!is_err(&upserted), "{}", text(&upserted));

    let parsed: serde_json::Value = serde_json::from_str(&raw_text(&upserted)).unwrap();
    let candidates = parsed["duplicate_candidates"].as_array().unwrap();
    assert_eq!(
        candidates.len(),
        INTELLIGENCE_DUPLICATE_CANDIDATES_CAP,
        "duplicate_candidates must be capped at the budget-derived limit"
    );
    assert_eq!(parsed["duplicate_candidates_truncated"], true);
    assert_eq!(
        parsed["duplicate_candidates_omitted"].as_u64().unwrap() as usize,
        5,
        "omitted count must equal total minus the cap"
    );
}

#[tokio::test]
async fn intelligence_graph_walk_compact_omits_bodies() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cb28-walk-compact");

    upsert_large_intel_node(&handler, "cb28-a", "CB28 node A", "LARGE-BODY-ALPHA", None).await;
    upsert_large_intel_node(
        &handler,
        "cb28-b",
        "CB28 node B",
        "LARGE-BODY-BETA",
        Some(vec![IntelligenceRelationParams {
            to_node_id: "cb28-a".to_string(),
            relation: "extends".to_string(),
            weight: None,
        }]),
    )
    .await;
    upsert_large_intel_node(
        &handler,
        "cb28-c",
        "CB28 node C",
        "LARGE-BODY-GAMMA",
        Some(vec![IntelligenceRelationParams {
            to_node_id: "cb28-b".to_string(),
            relation: "extends".to_string(),
            weight: None,
        }]),
    )
    .await;

    let walked = call_resource_action(
        &handler,
        "intelligence_admin",
        "walk",
        params_id(IntelligenceGraphWalkParams {
            node_id: "cb28-c".to_string(),
            depth: Some(2),
            compact: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&walked), "{}", text(&walked));
    let body = raw_text(&walked);
    for marker in ["LARGE-BODY-ALPHA", "LARGE-BODY-BETA", "LARGE-BODY-GAMMA"] {
        assert!(
            !body.contains(marker),
            "compact walk must omit node bodies, found {marker}"
        );
    }
    // The nodes are still there — only their bodies are gone.
    for title in ["CB28 node A", "CB28 node B", "CB28 node C"] {
        assert!(body.contains(title), "compact walk must keep {title}");
    }
    assert!(
        body.len() < MCP_RESULT_BUDGET_BYTES,
        "compact walk ({} bytes) must fit the {}-byte budget",
        body.len(),
        MCP_RESULT_BUDGET_BYTES
    );
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        parsed.get("node_count").is_some(),
        "walk must state node_count"
    );
    assert!(
        parsed.get("edge_count").is_some(),
        "walk must state edge_count"
    );
}

#[tokio::test]
async fn intelligence_graph_walk_root_appears_once() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cb28-walk-root");

    upsert_large_intel_node(&handler, "cb28-r1", "CB28 root R1", "ROOT-BODY-R1", None).await;
    upsert_large_intel_node(
        &handler,
        "cb28-r2",
        "CB28 leaf R2",
        "ROOT-BODY-R2",
        Some(vec![IntelligenceRelationParams {
            to_node_id: "cb28-r1".to_string(),
            relation: "extends".to_string(),
            weight: None,
        }]),
    )
    .await;

    let walked = call_resource_action(
        &handler,
        "intelligence_admin",
        "walk",
        params_id(IntelligenceGraphWalkParams {
            node_id: "cb28-r2".to_string(),
            depth: Some(1),
            compact: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&walked), "{}", text(&walked));
    let parsed: serde_json::Value = serde_json::from_str(&raw_text(&walked)).unwrap();
    let root_id = parsed["root"]["id"].as_str().unwrap().to_string();
    let nodes = parsed["nodes"].as_array().unwrap();
    assert!(
        !nodes
            .iter()
            .any(|n| n.get("id").and_then(|id| id.as_str()) == Some(root_id.as_str())),
        "root {root_id} must not appear among nodes"
    );
    assert_eq!(
        parsed["node_count"].as_u64().unwrap() as usize,
        nodes.len(),
        "node_count must match nodes returned"
    );
}

#[tokio::test]
async fn intelligence_graph_walk_compact_false_returns_bodies() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let _agent_guard = AgentIdVar::set("agent-cb28-walk-full");

    upsert_large_intel_node(
        &handler,
        "cb28-f1",
        "CB28 full node",
        "FULL-BODY-DISTINCTIVE",
        None,
    )
    .await;

    let walked = call_resource_action(
        &handler,
        "intelligence_admin",
        "walk",
        params_id(IntelligenceGraphWalkParams {
            node_id: "cb28-f1".to_string(),
            depth: Some(1),
            compact: Some(false),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&walked), "{}", text(&walked));
    assert!(
        raw_text(&walked).contains("FULL-BODY-DISTINCTIVE"),
        "compact: false must restore full bodies"
    );
}

#[tokio::test]
async fn spec_list_pagination_respects_limit_and_offset() {
    let (_dir, db, handler) = endpoint_test_handler();
    for i in 0..10 {
        insert_standalone_spec(&db, &format!("CB28 page spec {i:02}"));
    }

    let first = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: None,
            include_descriptions: None,
            limit: Some(3),
            offset: Some(0),
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    let first_body = raw_text(&first);
    let first_parsed: serde_json::Value = serde_json::from_str(&first_body).unwrap();
    assert_eq!(first_parsed["returned"], 3);
    assert_eq!(first_parsed["total"], 10);
    assert_eq!(first_parsed["truncated"], true);
    assert_eq!(first_parsed["omitted"], 7);
    let first_ids = spec_list_ids(&first_body);
    assert_eq!(first_ids.len(), 3);

    let second = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: None,
            include_descriptions: None,
            limit: Some(3),
            offset: Some(3),
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    let second_body = raw_text(&second);
    let second_ids = spec_list_ids(&second_body);
    assert_eq!(second_ids.len(), 3);
    for id in &second_ids {
        assert!(
            !first_ids.contains(id),
            "offset page must not repeat rows from the first page"
        );
    }

    let last = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: None,
            include_descriptions: None,
            limit: Some(3),
            offset: Some(9),
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    let last_body = raw_text(&last);
    let last_parsed: serde_json::Value = serde_json::from_str(&last_body).unwrap();
    assert_eq!(last_parsed["returned"], 1);
    assert_eq!(last_parsed["total"], 10);
    assert!(
        last_parsed.get("truncated").is_none(),
        "final page must not report truncation: {last_body}"
    );
    let last_ids = spec_list_ids(&last_body);
    assert!(!first_ids.contains(&last_ids[0]));
    assert!(!second_ids.contains(&last_ids[0]));
}

#[tokio::test]
async fn spec_list_default_page_fits_budget() {
    let (_dir, db, handler) = endpoint_test_handler();
    let want = SPEC_LIST_DEFAULT_LIMIT as usize + 10;
    for i in 0..want {
        insert_standalone_spec(&db, &format!("CB28 budget probe spec {i:03}"));
    }

    let listed = call_resource_action(
        &handler,
        "spec",
        "list",
        params_id(SpecListParams {
            workdir: None,
            status: None,
            unassigned_only: None,
            include_descriptions: None,
            limit: None,
            offset: None,
            spec_type: None,
        }),
    )
    .await
    .unwrap();
    let body = raw_text(&listed);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        parsed["returned"].as_u64().unwrap() as usize,
        SPEC_LIST_DEFAULT_LIMIT as usize,
        "default page must return exactly the budget-derived default"
    );
    assert_eq!(parsed["total"].as_u64().unwrap() as usize, want);
    assert_eq!(parsed["truncated"], true);
    assert_eq!(
        parsed["omitted"].as_u64().unwrap() as usize,
        want - SPEC_LIST_DEFAULT_LIMIT as usize
    );
    assert!(
        body.len() <= MCP_RESULT_BUDGET_BYTES,
        "default spec_list page ({} bytes) must fit the {}-byte budget",
        body.len(),
        MCP_RESULT_BUDGET_BYTES
    );
}

#[tokio::test]
async fn graph_node_runs_list_compact_mode() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node = insert_named_node(&db, &spec.id, "builder", 1);
    let now = chrono::Utc::now();
    insert_finalized_node_run(
        &db,
        &lp.id,
        &spec.id,
        &node.id,
        GraphRunStatus::Pass,
        None,
        now,
    );

    let listed = call_graph_action(
        &handler,
        "graph",
        "node_runs",
        params_id(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: None,
            node_id: None,
            limit: None,
            offset: None,
            compact: Some(true),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body = raw_text(&listed);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let runs = parsed["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].get("id").is_some(), "compact mode must include id");
    assert!(
        runs[0].get("spec_id").is_none(),
        "compact mode must omit spec_id"
    );
    assert_eq!(runs[0]["spec_name"], "Test Spec");
    assert_eq!(runs[0]["node_name"], "builder");
}

#[tokio::test]
async fn graph_node_runs_list_compact_id_feeds_graph_node_run_get() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node = insert_named_node(&db, &spec.id, "builder", 1);
    let run = insert_finalized_node_run(
        &db,
        &lp.id,
        &spec.id,
        &node.id,
        GraphRunStatus::Pass,
        Some(serde_json::json!({"reported_output": "ok"})),
        chrono::Utc::now(),
    );

    let listed = call_graph_action(
        &handler,
        "graph",
        "node_runs",
        params_id(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: None,
            node_id: None,
            limit: None,
            offset: None,
            compact: Some(true),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body = raw_text(&listed);
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let runs = parsed["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 1);
    let compact_id = runs[0]
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    assert!(!compact_id.is_empty(), "compact listing must carry an id");
    assert_eq!(compact_id, run.id);
    assert!(
        runs[0].get("input").is_none(),
        "compact mode must omit input"
    );
    assert!(
        runs[0].get("output").is_none(),
        "compact mode must omit output"
    );

    let got = call_graph_action(
        &handler,
        "graph",
        "node_run",
        params_id(GraphNodeRunGetParams {
            run_id: compact_id.to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let detail: serde_json::Value = serde_json::from_str(&raw_text(&got)).unwrap();
    assert_eq!(detail["id"], run.id);
}

#[tokio::test]
async fn graph_node_runs_list_reports_truncation() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let node = insert_named_node(&db, &spec.id, "builder", 1);
    let now = chrono::Utc::now();
    for i in 0..3 {
        insert_finalized_node_run(
            &db,
            &lp.id,
            &spec.id,
            &node.id,
            GraphRunStatus::Pass,
            None,
            now - chrono::Duration::minutes(i),
        );
    }

    let capped = call_graph_action(
        &handler,
        "graph",
        "node_runs",
        params_id(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: None,
            node_id: None,
            limit: Some(1),
            offset: None,
            compact: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&capped), "{}", text(&capped));
    let parsed: serde_json::Value = serde_json::from_str(&raw_text(&capped)).unwrap();
    assert_eq!(parsed["runs"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["total"], 3);
    assert_eq!(parsed["truncated"], true);
    assert_eq!(parsed["omitted"], 2);

    let full = call_graph_action(
        &handler,
        "graph",
        "node_runs",
        params_id(GraphNodeRunsListParams {
            graph_id: lp.id.clone(),
            spec_id: None,
            node_id: None,
            limit: Some(50),
            offset: None,
            compact: None,
        }),
    )
    .await
    .unwrap();
    let parsed_full: serde_json::Value = serde_json::from_str(&raw_text(&full)).unwrap();
    assert_eq!(parsed_full["total"], 3);
    assert!(
        parsed_full.get("truncated").is_none(),
        "a complete result must not claim truncation"
    );
    assert!(parsed_full.get("omitted").is_none());
}

#[tokio::test]
async fn spec_update_with_prefix_resolves() {
    let (_dir, db, handler) = endpoint_test_handler();
    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Prefix test".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let all_specs = db.list_specs(None, None, false, None).unwrap();
    let full_id = all_specs[0].id.clone();
    let prefix = &full_id[..8];

    let updated = call_resource_action(
        &handler,
        "spec_edit",
        "update",
        params_id(SpecUpdateParams {
            spec_id: prefix.to_string(),
            name: Some("Renamed via prefix".to_string()),
            description: None,
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph_spec(&full_id).unwrap().unwrap().name,
        "Renamed via prefix"
    );
}

#[tokio::test]
async fn spec_section_get_returns_section_from_tagged_spec() {
    let (_dir, db, handler) = endpoint_test_handler();
    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Section test".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));

    let spec_id = db.list_specs(None, None, false, None).unwrap()[0]
        .id
        .clone();

    let result = call_resource_action(
        &handler,
        "spec",
        "section",
        params_id(SpecSectionGetParams {
            spec_id: spec_id.clone(),
            section: "objective".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("Ship the feature"));
    assert!(body.contains("\"needs_conversion\": false"));
}

#[tokio::test]
async fn spec_section_get_rejects_unknown_section() {
    let (_dir, db, handler) = endpoint_test_handler();
    let created = call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            name: "Section test 2".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created));

    let spec_id = db.list_specs(None, None, false, None).unwrap()[0]
        .id
        .clone();

    let result = call_resource_action(
        &handler,
        "spec",
        "section",
        params_id(SpecSectionGetParams {
            spec_id,
            section: "bogus".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("unknown section"));
    assert!(text(&result).contains("bogus"));
}

#[tokio::test]
async fn resolve_spec_id_by_prefix_tests() {
    let (_dir, db, _handler) = endpoint_test_handler();
    let spec1 = GraphSpec {
        id: "aaaaaaaa-1111-1111-1111-111111111111".to_string(),
        graph_id: None,
        name: "Spec A".to_string(),
        description: Some("First".to_string()),
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    db.insert_graph_spec(&spec1).unwrap();
    let spec2 = GraphSpec {
        id: "aaaaaaaa-2222-2222-2222-222222222222".to_string(),
        graph_id: None,
        name: "Spec B".to_string(),
        description: Some("Second".to_string()),
        position: 2,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    };
    db.insert_graph_spec(&spec2).unwrap();

    let exact = db.resolve_spec_id_by_prefix(&spec1.id, None).unwrap();
    assert_eq!(exact, Some(spec1.id.clone()));

    let prefix_match = db.resolve_spec_id_by_prefix("aaaaaaaa-1111", None).unwrap();
    assert_eq!(prefix_match, Some(spec1.id));

    let ambiguous = db.resolve_spec_id_by_prefix("aaaaaaaa", None);
    assert!(ambiguous.is_err(), "ambiguous prefix should error");
    assert!(ambiguous.unwrap_err().to_string().contains("Ambiguous"));

    let not_found = db.resolve_spec_id_by_prefix("zzzzzzzz", None).unwrap();
    assert_eq!(not_found, None);
}

#[tokio::test]
async fn resolve_queue_id_by_prefix_tests() {
    let (_dir, db, _handler) = endpoint_test_handler();
    let q1 = Queue {
        id: "q-aaaaaaaa-1111".to_string(),
        name: "Q1".to_string(),
        created_at: chrono::Utc::now(),
    };
    let q2 = Queue {
        id: "q-aaaaaaaa-2222".to_string(),
        name: "Q2".to_string(),
        created_at: chrono::Utc::now(),
    };
    db.insert_queue(&q1).unwrap();
    db.insert_queue(&q2).unwrap();

    let exact = db.resolve_queue_id_by_prefix("q-aaaaaaaa-1111").unwrap();
    assert_eq!(exact, Some("q-aaaaaaaa-1111".to_string()));

    let prefix_match = db.resolve_queue_id_by_prefix("q-aaaaaaaa-111").unwrap();
    assert_eq!(prefix_match, Some("q-aaaaaaaa-1111".to_string()));

    let ambiguous = db.resolve_queue_id_by_prefix("q-aaaaaaaa");
    assert!(ambiguous.is_err());

    let not_found = db.resolve_queue_id_by_prefix("q-zzzz").unwrap();
    assert_eq!(not_found, None);
}

#[tokio::test]
async fn resolve_graph_id_by_prefix_tests() {
    let (dir, db, _handler) = endpoint_test_handler();
    let lp1 = insert_test_graph(&db, dir.path());
    let lp2 = Graph {
        id: "graph-bbbbbbbb-2222".to_string(),
        name: "Graph B".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        auto_continue_at: None,
        auto_continue_action: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
    };
    db.insert_graph(&lp2).unwrap();

    let exact = db.resolve_graph_id_by_prefix(&lp1.id).unwrap();
    assert_eq!(exact, Some(lp1.id));

    let not_found = db.resolve_graph_id_by_prefix("graph-zzzz").unwrap();
    assert_eq!(not_found, None);
}

#[tokio::test]
async fn graph_add_ensemble_with_cascade_kind_rejects_single_member() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let result = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Cascade".to_string(),
            kind: Some("cascade".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(vec![EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }]),
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
    assert!(
        is_err(&result),
        "cascade ensemble with 1 member must now be refused: {:?}",
        result
    );
    assert!(
        text(&result).contains("2-16") && text(&result).contains("cascade"),
        "error must name the sequential bound and the kind: {}",
        text(&result)
    );
}

#[tokio::test]
async fn graph_add_ensemble_with_parallel_kind_rejects_single_member() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let result = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Parallel".to_string(),
            kind: Some("parallel".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(vec![EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }]),
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
    assert!(
        is_err(&result),
        "parallel ensemble with 1 member should fail"
    );
}

#[tokio::test]
async fn graph_add_ensemble_rejects_straggler_timeout_on_cascade() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let result = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Cascade".to_string(),
            kind: Some("cascade".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: Some(5),
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
    assert!(
        is_err(&result),
        "cascade + straggler_timeout_minutes must be rejected"
    );
    assert!(
        text(&result).contains("straggler_timeout_minutes") && text(&result).contains("cascade"),
        "error must name the field and the kind: {}",
        text(&result)
    );
}

#[tokio::test]
async fn graph_update_ensemble_rejects_quorum_grace_on_round_robin() {
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
            kind: Some("round_robin".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
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
    let ensemble_id = extract_id(&created, "ensemble_id");

    let result = call_graph_action(
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
            quorum_grace_minutes: Some(Some(3)),
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
    assert!(
        is_err(&result),
        "round_robin + quorum_grace_minutes must be rejected"
    );
    assert!(
        text(&result).contains("quorum_grace_minutes") && text(&result).contains("round_robin"),
        "error must name the field and the kind: {}",
        text(&result)
    );
}
