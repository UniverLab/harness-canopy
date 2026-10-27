//! Part 01 of `tests` (split from `src/domain/graph_transfer/mod.rs`, pure move).
use super::*;

#[test]
fn export_v2_always_includes_agent_bindings_and_null_model() {
    let (lp, nodes, edges) = simple_graph();
    let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
    assert_eq!(doc.format_version, GRAPH_EXPORT_FORMAT_VERSION);
    let implementer = doc.nodes.iter().find(|n| n.name == "implementer").unwrap();
    assert_eq!(implementer.config["platform"], "claude");
    assert_eq!(implementer.config["model"], "opus");
    assert_eq!(implementer.config["prompt_template"], "implement it");
    // The committer stores no model (platform default): export states
    // it explicitly as null rather than omitting the key.
    let committer = doc.nodes.iter().find(|n| n.name == "committer").unwrap();
    assert_eq!(committer.config["platform"], "claude");
    assert!(committer.config.get("model").is_some());
    assert!(committer.config["model"].is_null());
    // Non-binding keys pass through untouched.
    assert_eq!(committer.config["prompt_template"], "commit it");
}

#[test]
fn export_v2_members_carry_bindings_in_position_order() {
    let lp = make_graph("member-order-graph");
    let kickoff = make_node(
        "kickoff",
        "kickoff",
        GraphNodeKind::Check,
        serde_json::json!({"command": "true"}),
        1,
    );
    let downstream = make_node(
        "downstream",
        "downstream",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "claude", "prompt_template": "wrap up"}),
        10,
    );
    let member1 = make_node(
        "m1",
        "Team [1]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "copilot", "prompt_template": "draft it", "timeout_minutes": 30}),
        2,
    );
    let member2 = make_node(
        "m2",
        "Team [2]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "opencode", "model": "opencode-go/qwen3.7-plus", "prompt_template": "draft it", "timeout_minutes": 30}),
        3,
    );
    let member3 = make_node(
        "m3",
        "Team [3]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "opencode", "model": "opencode/muse-spark", "prompt_template": "custom", "timeout_minutes": 30}),
        4,
    );
    let join = make_node(
        "join1",
        "Team (quorum)",
        GraphNodeKind::Join,
        serde_json::json!({"ensemble_id": "ens1"}),
        5,
    );
    let nodes = vec![kickoff, downstream, member1, member2, member3, join];
    let edges = vec![
        make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
        make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
        make_edge("e3", "kickoff", "m3", GraphEdgeCondition::Always),
        make_edge("e4", "m1", "join1", GraphEdgeCondition::Always),
        make_edge("e5", "m2", "join1", GraphEdgeCondition::Always),
        make_edge("e6", "m3", "join1", GraphEdgeCondition::Always),
        make_edge("e7", "join1", "downstream", GraphEdgeCondition::Pass),
    ];
    // Members stored out of position order on purpose: export must
    // still emit them in position order.
    let mut ensemble_details =
        make_ensemble_details("ens1", "join1", "kickoff", "downstream", &["m1", "m2"]);
    ensemble_details.members.push(EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m3".to_string(),
        position: 2,
        platform: "opencode".to_string(),
        model: Some("opencode/muse-spark".to_string()),
        prompt_override: Some("custom angle".to_string()),
        timeout_minutes: Some(3),
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    });
    ensemble_details.members[0] = EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "copilot".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    ensemble_details.members[1] = EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m2".to_string(),
        position: 1,
        platform: "opencode".to_string(),
        model: Some("opencode-go/qwen3.7-plus".to_string()),
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    };
    // Shuffle the stored order so the test pins the sort, not the input.
    ensemble_details.members.swap(0, 2);
    ensemble_details.ensemble.name = "Team".to_string();

    let doc = build_export_document(&lp, &nodes, &edges, &[ensemble_details]).unwrap();
    let team = doc.ensembles.iter().find(|e| e.name == "Team").unwrap();
    assert_eq!(team.members.len(), 3);
    assert_eq!(team.members[0].platform.as_deref(), Some("copilot"));
    assert_eq!(team.members[0].model, None);
    assert_eq!(team.members[0].prompt_override, None);
    assert_eq!(team.members[1].platform.as_deref(), Some("opencode"));
    assert_eq!(
        team.members[1].model.as_deref(),
        Some("opencode-go/qwen3.7-plus")
    );
    assert_eq!(team.members[2].platform.as_deref(), Some("opencode"));
    assert_eq!(
        team.members[2].model.as_deref(),
        Some("opencode/muse-spark")
    );
    assert_eq!(
        team.members[2].prompt_override.as_deref(),
        Some("custom angle")
    );
    assert_eq!(team.members[2].timeout_minutes, Some(3));
    assert_eq!(team.members[0].timeout_minutes, None);
    // Optional fields serialize as explicit nulls, never omitted keys.
    let raw = serde_json::to_string(&doc).unwrap();
    assert!(raw.contains("\"model\":null"));
    assert!(raw.contains("\"prompt_override\":null"));
    assert!(raw.contains("\"platform\":\"copilot\""));
}

#[test]
fn export_rejects_duplicate_node_names() {
    let (lp, mut nodes, edges) = simple_graph();
    nodes[1].name = "implementer".to_string();
    let err = build_export_document(&lp, &nodes, &edges, &[]).unwrap_err();
    assert!(err.contains("implementer"));
    assert!(err.contains("duplicate"));
}

#[test]
fn format_version_first_key_in_serialized_json() {
    let (lp, nodes, edges) = simple_graph();
    let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
    let raw = serde_json::to_string(&doc).unwrap();
    // struct field order drives serde_json's key order.
    assert!(raw.starts_with("{\"format_version\":6"));
}

#[test]
fn import_rejects_missing_format_version() {
    let value = serde_json::json!({
        "name": "x", "nodes": [], "edges": [], "ensembles": []
    });
    let err = parse_export_document_value(&value).unwrap_err();
    assert!(err.contains("format_version"));
}

#[test]
fn import_rejects_unsupported_format_version() {
    let value = serde_json::json!({
        "format_version": 99, "name": "x", "nodes": [], "edges": [], "ensembles": []
    });
    let err = parse_export_document_value(&value).unwrap_err();
    assert!(err.contains("99"));
}

#[test]
fn import_accepts_format_versions_1_through_6() {
    for version in [1, 2, 3, 4, 5, 6] {
        let value = serde_json::json!({
            "format_version": version, "name": "x", "nodes": [], "edges": [], "ensembles": []
        });
        let doc = parse_export_document_value(&value)
            .unwrap_or_else(|e| panic!("version {version} must parse: {e}"));
        assert_eq!(doc.format_version, version);
    }
    // An old document with no kind/infra/sources imports as a parallel
    // with empty optionals.
    let value = serde_json::json!({
        "format_version": 1,
        "name": "legacy",
        "nodes": [
            {"name": "kickoff", "kind": "check", "position": 1, "config": {"command": "true"}},
            {"name": "next", "kind": "check", "position": 2, "config": {"command": "true"}}
        ],
        "edges": [],
        "ensembles": [
            {
                "name": "team",
                "prompt_template": "go",
                "entry_from_node": "kickoff",
                "entry_condition": "always",
                "on_pass_to": "next",
                "min_pass": 2,
                "timeout_minutes": 30,
                "members": [
                    {"platform": "claude"},
                    {"platform": "claude"}
                ]
            }
        ]
    });
    let document = parse_export_document_value(&value).unwrap();
    let plan = build_import_plan(&document, "new-graph").unwrap();
    assert_eq!(
        plan.ensembles[0].ensemble.kind,
        crate::domain::graphs::EnsembleKind::Parallel
    );
    assert_eq!(plan.ensembles[0].ensemble.infra_retry_limit, None);
    assert_eq!(plan.ensembles[0].ensemble.infra_crash_max_seconds, None);
    assert_eq!(plan.ensembles[0].ensemble.infra_backoff_seconds, None);
    assert_eq!(plan.ensembles[0].members[0].infra_retry_limit, None);
}

#[test]
fn import_v2_break_condition_is_error() {
    let value = serde_json::json!({
        "format_version": 2,
        "name": "legacy",
        "nodes": [
            {"name": "a", "kind": "check", "position": 1, "config": {"command": "true"}},
            {"name": "b", "kind": "check", "position": 2, "config": {"command": "true"}}
        ],
        "edges": [{"from_node": "a", "to_node": "b", "condition": "break"}],
        "ensembles": []
    });
    let document = parse_export_document_value(&value).unwrap();
    assert_eq!(document.edges[0].condition, GraphEdgeCondition::Error);
    let exported = serde_json::to_value(GraphExportDocument {
        format_version: GRAPH_EXPORT_FORMAT_VERSION,
        ..document
    })
    .unwrap();
    assert_eq!(exported["format_version"], 6);
    assert_eq!(exported["edges"][0]["condition"], "error");
}

#[test]
fn import_plan_rejects_edge_naming_nonexistent_node() {
    let doc = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![GraphExportNode {
            name: "only".to_string(),
            kind: GraphNodeKind::Check,
            position: 1,
            config: serde_json::json!({"command": "true"}),
        }],
        edges: vec![GraphExportEdge {
            from_node: "only".to_string(),
            to_node: "ghost".to_string(),
            condition: GraphEdgeCondition::Always,
        }],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(err.contains("ghost"));
}

#[test]
fn import_plan_rejects_join_kind_node() {
    let doc = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![GraphExportNode {
            name: "sneaky".to_string(),
            kind: GraphNodeKind::Join,
            position: 1,
            config: serde_json::json!({}),
        }],
        edges: vec![],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(err.contains("join"));
}

#[test]
fn import_plan_assigns_fresh_ids_and_graph_id() {
    let (lp, nodes, edges) = simple_graph();
    let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
    let plan = build_import_plan(&doc, "brand-new-graph-id").unwrap();
    assert_eq!(plan.nodes.len(), 3);
    for node in &plan.nodes {
        assert_eq!(node.graph_id.as_deref(), Some("brand-new-graph-id"));
        assert!(node.spec_id.is_none());
        assert!(!nodes.iter().any(|n| n.id == node.id), "id must be fresh");
    }
    assert_eq!(plan.edges.len(), 2);
}

#[test]
fn agent_nodes_missing_platform_reports_unbound_v1_nodes() {
    // A v1-style document carries no bindings: every agent node is
    // reported. Built by hand (not via export, which always binds).
    let doc = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "implementer".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({"prompt_template": "implement it"}),
            },
            GraphExportNode {
                name: "gate".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({"command": "cargo test"}),
            },
            GraphExportNode {
                name: "committer".to_string(),
                kind: GraphNodeKind::Agent,
                position: 3,
                config: serde_json::json!({"prompt_template": "commit it"}),
            },
        ],
        edges: vec![
            GraphExportEdge {
                from_node: "implementer".to_string(),
                to_node: "gate".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphExportEdge {
                from_node: "gate".to_string(),
                to_node: "committer".to_string(),
                condition: GraphEdgeCondition::Always,
            },
        ],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let plan = build_import_plan(&doc, "graph-2").unwrap();
    let missing = agent_nodes_missing_platform(&plan);
    assert_eq!(missing.len(), 2);
    assert!(missing.contains(&"implementer".to_string()));
    assert!(missing.contains(&"committer".to_string()));
}

#[test]
fn agent_nodes_missing_platform_empty_for_v2_export() {
    let (lp, nodes, edges) = simple_graph();
    let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
    let plan = build_import_plan(&doc, "graph-2").unwrap();
    assert!(agent_nodes_missing_platform(&plan).is_empty());
}

#[test]
fn resolve_unique_graph_name_returns_desired_when_free() {
    let existing = vec!["other".to_string()];
    assert_eq!(resolve_unique_graph_name(&existing, "my-graph"), "my-graph");
}

#[test]
fn resolve_unique_graph_name_suffixes_on_collision() {
    let existing = vec!["my-graph".to_string(), "my-graph (2)".to_string()];
    assert_eq!(
        resolve_unique_graph_name(&existing, "my-graph"),
        "my-graph (3)"
    );
}

/// The full ensemble round trip: build a graph with a kickoff node, a
/// 2-member ensemble, and a downstream node; export it, import it under
/// a new graph id, and export the result again. Requirement 5 (round
/// trip, with bindings included on both ends) and the acceptance
/// criterion ("an ensemble survives the round trip as an ensemble, not
/// as expanded member nodes") both pin on this.
#[test]
fn ensemble_round_trips_as_an_ensemble_not_expanded_nodes() {
    let lp = make_graph("ensemble-graph");
    let kickoff = make_node(
        "kickoff",
        "kickoff",
        GraphNodeKind::Check,
        serde_json::json!({"command": "true"}),
        1,
    );
    let downstream = make_node(
        "downstream",
        "downstream",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "claude", "prompt_template": "wrap up"}),
        10,
    );
    let member1 = make_node(
        "m1",
        "Proposers [1]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "openrouter", "model": "model-0", "prompt_template": "draft it", "timeout_minutes": 30}),
        2,
    );
    let member2 = make_node(
        "m2",
        "Proposers [2]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "openrouter", "model": "model-1", "prompt_template": "draft it", "timeout_minutes": 30}),
        3,
    );
    let join = make_node(
        "join1",
        "Proposers (quorum)",
        GraphNodeKind::Join,
        serde_json::json!({"ensemble_id": "ens1"}),
        4,
    );
    let nodes = vec![kickoff, downstream, member1, member2, join];
    let edges = vec![
        make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
        make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
        make_edge("e3", "m1", "join1", GraphEdgeCondition::Always),
        make_edge("e4", "m2", "join1", GraphEdgeCondition::Always),
        make_edge("e5", "join1", "downstream", GraphEdgeCondition::Pass),
    ];
    let ensembles = vec![make_ensemble_details(
        "ens1",
        "join1",
        "kickoff",
        "downstream",
        &["m1", "m2"],
    )];

    let first_export = build_export_document(&lp, &nodes, &edges, &ensembles).unwrap();

    // Ensemble survives as one ensemble entry; the plain node list
    // excludes the member/join nodes entirely.
    assert_eq!(first_export.ensembles.len(), 1);
    assert_eq!(first_export.nodes.len(), 2);
    assert!(!first_export
        .nodes
        .iter()
        .any(|n| n.name.contains("Proposers")));
    assert_eq!(first_export.ensembles[0].members.len(), 2);

    let plan = build_import_plan(&first_export, "graph-2").unwrap();
    assert_eq!(plan.ensembles.len(), 1);
    assert_eq!(plan.ensembles[0].member_nodes.len(), 2);

    // Re-export the imported plan under a Graph with the same name as
    // the original (import always renames on collision, but here we
    // simulate "no collision" so the round trip is name-for-name) and
    // assert the document is identical except for the name change we
    // deliberately introduce.
    let mut imported_all_nodes = plan.nodes.clone();
    let mut imported_ensemble_details = Vec::new();
    for ens in &plan.ensembles {
        imported_all_nodes.push(ens.join_node.clone());
        imported_all_nodes.extend(ens.member_nodes.iter().cloned());
        imported_ensemble_details.push(EnsembleDetails {
            ensemble: ens.ensemble.clone(),
            members: ens.members.clone(),
        });
    }
    let mut lp2 = make_graph("ensemble-graph");
    lp2.id = "graph-2".to_string();
    let second_export = build_export_document(
        &lp2,
        &imported_all_nodes,
        &plan.edges,
        &imported_ensemble_details,
    )
    .unwrap();

    assert_eq!(first_export.name, second_export.name);
    assert_eq!(first_export.description, second_export.description);
    assert_eq!(first_export.nodes, second_export.nodes);
    assert_eq!(first_export.edges, second_export.edges);
    assert_eq!(first_export.ensembles, second_export.ensembles);
}

#[test]
fn entry_sources_round_trip_through_export_import_export() {
    let lp = make_graph("committer-loop");
    let nodes = vec![
        make_node(
            "unlock",
            "unlock",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            1,
        ),
        make_node(
            "check_committed",
            "check_committed",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            2,
        ),
        make_node(
            "final",
            "final",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            10,
        ),
        make_node(
            "m1",
            "Committer [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude"}),
            3,
        ),
        make_node(
            "m2",
            "Committer [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "copilot"}),
            4,
        ),
        make_node(
            "join1",
            "Committer (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens1"}),
            5,
        ),
    ];
    let edges = vec![
        make_edge("e1", "unlock", "m1", GraphEdgeCondition::Pass),
        make_edge("e2", "unlock", "m2", GraphEdgeCondition::Pass),
        make_edge("e3", "m1", "join1", GraphEdgeCondition::Always),
        make_edge("e4", "m2", "join1", GraphEdgeCondition::Always),
        make_edge("e5", "join1", "final", GraphEdgeCondition::Pass),
        make_edge("e6", "join1", "check_committed", GraphEdgeCondition::Fail),
        make_edge("e7", "check_committed", "m1", GraphEdgeCondition::Fail),
        make_edge("e8", "check_committed", "m2", GraphEdgeCondition::Fail),
    ];
    let mut details = make_ensemble_details("ens1", "join1", "unlock", "final", &["m1", "m2"]);
    details.ensemble.name = "Committer".to_string();
    details.ensemble.entry_condition = GraphEdgeCondition::Pass;
    details.ensemble.kind = EnsembleKind::Cascade;
    details.ensemble.on_fail_to = Some("check_committed".to_string());

    let first = build_export_document(&lp, &nodes, &edges, &[details]).unwrap();
    assert_eq!(
        first.ensembles[0].extra_entry_sources,
        vec![GraphExportEntrySource {
            from_node: "check_committed".to_string(),
            condition: GraphEdgeCondition::Fail,
        }]
    );

    let plan = build_import_plan(&first, "graph-2").unwrap();
    let unlock_id = plan
        .nodes
        .iter()
        .find(|node| node.name == "unlock")
        .expect("unlock in import plan")
        .id
        .clone();
    let check_committed_id = plan
        .nodes
        .iter()
        .find(|node| node.name == "check_committed")
        .expect("check_committed in import plan")
        .id
        .clone();
    let member_ids: std::collections::HashSet<&str> = plan.ensembles[0]
        .member_nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    let unlock_targets: std::collections::HashSet<&str> = plan
        .edges
        .iter()
        .filter(|edge| edge.from_node == unlock_id && edge.condition == GraphEdgeCondition::Pass)
        .map(|edge| edge.to_node.as_str())
        .collect();
    let check_committed_targets: std::collections::HashSet<&str> = plan
        .edges
        .iter()
        .filter(|edge| {
            edge.from_node == check_committed_id && edge.condition == GraphEdgeCondition::Fail
        })
        .map(|edge| edge.to_node.as_str())
        .collect();
    assert_eq!(unlock_targets, member_ids);
    assert_eq!(check_committed_targets, member_ids);
    assert_eq!(unlock_targets.len(), 2);
    assert_eq!(check_committed_targets.len(), 2);

    let mut imported_all_nodes = plan.nodes.clone();
    let mut imported_ensemble_details = Vec::new();
    for ensemble in &plan.ensembles {
        imported_all_nodes.push(ensemble.join_node.clone());
        imported_all_nodes.extend(ensemble.member_nodes.iter().cloned());
        imported_ensemble_details.push(EnsembleDetails {
            ensemble: ensemble.ensemble.clone(),
            members: ensemble.members.clone(),
        });
    }
    let mut imported_graph = make_graph("committer-loop");
    imported_graph.id = "graph-2".to_string();
    let second = build_export_document(
        &imported_graph,
        &imported_all_nodes,
        &plan.edges,
        &imported_ensemble_details,
    )
    .unwrap();
    assert_eq!(first.ensembles, second.ensembles);
    assert_eq!(
        serde_json::to_value(&first.ensembles).unwrap(),
        serde_json::to_value(&second.ensembles).unwrap()
    );
}

#[test]
fn export_omits_extra_entry_sources_when_there_are_none() {
    let lp = make_graph("ensemble-graph");
    let kickoff = make_node(
        "kickoff",
        "kickoff",
        GraphNodeKind::Check,
        serde_json::json!({"command": "true"}),
        1,
    );
    let downstream = make_node(
        "downstream",
        "downstream",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "claude", "prompt_template": "wrap up"}),
        10,
    );
    let member1 = make_node(
        "m1",
        "Proposers [1]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "openrouter", "model": "model-0", "prompt_template": "draft it", "timeout_minutes": 30}),
        2,
    );
    let member2 = make_node(
        "m2",
        "Proposers [2]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "openrouter", "model": "model-1", "prompt_template": "draft it", "timeout_minutes": 30}),
        3,
    );
    let join = make_node(
        "join1",
        "Proposers (quorum)",
        GraphNodeKind::Join,
        serde_json::json!({"ensemble_id": "ens1"}),
        4,
    );
    let nodes = vec![kickoff, downstream, member1, member2, join];
    let edges = vec![
        make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
        make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
        make_edge("e3", "m1", "join1", GraphEdgeCondition::Always),
        make_edge("e4", "m2", "join1", GraphEdgeCondition::Always),
        make_edge("e5", "join1", "downstream", GraphEdgeCondition::Pass),
    ];
    let ensembles = vec![make_ensemble_details(
        "ens1",
        "join1",
        "kickoff",
        "downstream",
        &["m1", "m2"],
    )];

    let document = build_export_document(&lp, &nodes, &edges, &ensembles).unwrap();
    assert!(document.ensembles[0].extra_entry_sources.is_empty());
    let raw = serde_json::to_string(&document).unwrap();
    assert!(!raw.contains("extra_entry_sources"));
}

#[test]
fn import_refuses_duplicate_extra_entry_source() {
    let document = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: GRAPH_EXPORT_FORMAT_VERSION,
        name: "duplicate-entry-source".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "kickoff".to_string(),
                kind: GraphNodeKind::Check,
                position: 1,
                config: serde_json::json!({}),
            },
            GraphExportNode {
                name: "final".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({}),
            },
        ],
        edges: vec![],
        ensembles: vec![GraphExportEnsemble {
            mutex: Vec::new(),
            commit_rights: false,
            name: "Committer".to_string(),
            kind: None,
            prompt_template: "review".to_string(),
            entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
            entry_condition: GraphEdgeCondition::Always,
            extra_entry_sources: vec![GraphExportEntrySource {
                from_node: "kickoff".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            on_pass_to: GraphExportEnsembleTarget::Node("final".to_string()),
            on_fail_to: Some(GraphExportEnsembleTarget::Node("final".to_string())),
            min_pass: 2,
            timeout_minutes: 10,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("x".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
                GraphExportEnsembleMember {
                    platform: Some("x".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
            ],

            crew: None,
        }],
        infra_node: None,

        hooks: Default::default(),
    };

    let error = build_import_plan(&document, "graph-1").unwrap_err();
    assert!(error.contains("Committer"), "unexpected error: {error}");
    assert!(
        error.contains("more than once"),
        "unexpected error: {error}"
    );
}

#[test]
fn import_refuses_extra_entry_source_naming_unknown_node() {
    let document = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: GRAPH_EXPORT_FORMAT_VERSION,
        name: "unknown-entry-source".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "kickoff".to_string(),
                kind: GraphNodeKind::Check,
                position: 1,
                config: serde_json::json!({}),
            },
            GraphExportNode {
                name: "final".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({}),
            },
        ],
        edges: vec![],
        ensembles: vec![GraphExportEnsemble {
            mutex: Vec::new(),
            commit_rights: false,
            name: "Committer".to_string(),
            kind: None,
            prompt_template: "review".to_string(),
            entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
            entry_condition: GraphEdgeCondition::Always,
            extra_entry_sources: vec![GraphExportEntrySource {
                from_node: "ghost".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            on_pass_to: GraphExportEnsembleTarget::Node("final".to_string()),
            on_fail_to: Some(GraphExportEnsembleTarget::Node("final".to_string())),
            min_pass: 2,
            timeout_minutes: 10,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("x".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
                GraphExportEnsembleMember {
                    platform: Some("x".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
            ],

            crew: None,
        }],
        infra_node: None,

        hooks: Default::default(),
    };

    let error = build_import_plan(&document, "graph-1").unwrap_err();
    assert!(
        error.contains("references unknown node 'ghost'"),
        "unexpected error: {error}"
    );
}

#[test]
fn chained_ensembles_export_as_tables_and_import_to_join_fanout() {
    let lp = make_graph("canopy-v4");
    let nodes = vec![
        make_node(
            "kickoff",
            "kickoff",
            GraphNodeKind::Check,
            serde_json::json!({}),
            1,
        ),
        make_node(
            "final",
            "final",
            GraphNodeKind::Check,
            serde_json::json!({}),
            2,
        ),
        make_node(
            "m1",
            "Reviewer 1 [1]",
            GraphNodeKind::Agent,
            serde_json::json!({}),
            3,
        ),
        make_node(
            "m2",
            "Reviewer 1 [2]",
            GraphNodeKind::Agent,
            serde_json::json!({}),
            4,
        ),
        make_node(
            "n1",
            "Reviewer 2 [1]",
            GraphNodeKind::Agent,
            serde_json::json!({}),
            5,
        ),
        make_node(
            "n2",
            "Reviewer 2 [2]",
            GraphNodeKind::Agent,
            serde_json::json!({}),
            6,
        ),
        make_node(
            "j1",
            "Reviewer 1 (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({}),
            7,
        ),
        make_node(
            "j2",
            "Reviewer 2 (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({}),
            8,
        ),
    ];
    let edges = vec![
        make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
        make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
        make_edge("e3", "m1", "j1", GraphEdgeCondition::Always),
        make_edge("e4", "m2", "j1", GraphEdgeCondition::Always),
        make_edge("e5", "j1", "n1", GraphEdgeCondition::Pass),
        make_edge("e6", "j1", "n2", GraphEdgeCondition::Pass),
        make_edge("e7", "n1", "j2", GraphEdgeCondition::Always),
        make_edge("e8", "n2", "j2", GraphEdgeCondition::Always),
        make_edge("e9", "j2", "final", GraphEdgeCondition::Pass),
    ];
    let mut reviewer1 = make_ensemble_details("ens1", "j1", "kickoff", "j2", &["m1", "m2"]);
    reviewer1.ensemble.name = "Reviewer 1".to_string();
    let mut reviewer2 = make_ensemble_details("ens2", "j2", "kickoff", "final", &["n1", "n2"]);
    reviewer2.ensemble.name = "Reviewer 2".to_string();

    let document = build_export_document(&lp, &nodes, &edges, &[reviewer1, reviewer2]).unwrap();
    assert_eq!(
        document.ensembles[0].on_pass_to,
        GraphExportEnsembleTarget::Ensemble {
            ensemble: "Reviewer 2".to_string()
        }
    );
    assert!(serde_json::to_string(&document)
        .unwrap()
        .contains("\"ensemble\":\"Reviewer 2\""));
    assert!(document.edges.iter().all(|edge| {
        edge.from_node != "Reviewer 1 (quorum)"
            && edge.to_node != "Reviewer 1 (quorum)"
            && edge.from_node != "Reviewer 2 (quorum)"
            && edge.to_node != "Reviewer 2 (quorum)"
    }));

    let plan = build_import_plan(&document, "graph-2").unwrap();
    let imported_r1 = plan
        .ensembles
        .iter()
        .find(|ensemble| ensemble.ensemble.name == "Reviewer 1")
        .unwrap();
    let imported_r2 = plan
        .ensembles
        .iter()
        .find(|ensemble| ensemble.ensemble.name == "Reviewer 2")
        .unwrap();
    assert_eq!(
        imported_r1.ensemble.on_pass_to,
        imported_r2.ensemble.join_node_id
    );
    for member in &imported_r2.members {
        assert!(plan.edges.iter().any(|edge| {
            edge.from_node == imported_r1.ensemble.join_node_id
                && edge.to_node == member.node_id
                && edge.condition == GraphEdgeCondition::Pass
        }));
    }
}

#[test]
fn export_refuses_duplicate_ensemble_names() {
    let lp = make_graph("duplicate-ensembles");
    let mut first = make_ensemble_details("ens1", "j1", "kickoff", "final", &["m1", "m2"]);
    let mut second = make_ensemble_details("ens2", "j2", "kickoff", "final", &["n1", "n2"]);
    first.ensemble.name = "Reviewer 1".to_string();
    second.ensemble.name = "Reviewer 1".to_string();
    let error = build_export_document(&lp, &[], &[], &[first, second]).unwrap_err();
    assert!(error.contains("duplicate ensemble name"));
    assert!(error.contains("Reviewer 1"));
}

#[test]
fn import_rejects_unknown_ensemble_target() {
    let document = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 3,
        name: "unknown-target".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "kickoff".to_string(),
                kind: GraphNodeKind::Check,
                position: 1,
                config: serde_json::json!({}),
            },
            GraphExportNode {
                name: "final".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({}),
            },
        ],
        edges: vec![],
        ensembles: vec![GraphExportEnsemble {
            mutex: Vec::new(),
            commit_rights: false,
            name: "Reviewer 1".to_string(),
            kind: None,
            prompt_template: "review".to_string(),
            entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
            entry_condition: GraphEdgeCondition::Always,
            extra_entry_sources: vec![],
            on_pass_to: GraphExportEnsembleTarget::Ensemble {
                ensemble: "Ghost".to_string(),
            },
            on_fail_to: Some(GraphExportEnsembleTarget::Node("final".to_string())),
            min_pass: 2,
            timeout_minutes: 10,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("x".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
                GraphExportEnsembleMember {
                    platform: Some("x".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
            ],

            crew: None,
        }],
        infra_node: None,

        hooks: Default::default(),
    };
    let error = build_import_plan(&document, "graph-1").unwrap_err();
    assert!(error.contains("unknown ensemble") && error.contains("Ghost"));
}

/// Requirement 5's full statement: export -> import -> export again
/// produces an identical document except for the name, for a plain
/// (non-ensemble) graph too.
#[test]
fn plain_graph_round_trips_identically_except_name() {
    let (lp, nodes, edges) = simple_graph();
    let first_export = build_export_document(&lp, &nodes, &edges, &[]).unwrap();

    let plan = build_import_plan(&first_export, "graph-2").unwrap();
    let mut lp2 = make_graph("renamed-on-import");
    lp2.id = "graph-2".to_string();
    let second_export = build_export_document(&lp2, &plan.nodes, &plan.edges, &[]).unwrap();

    assert_ne!(first_export.name, second_export.name);
    assert_eq!(second_export.name, "renamed-on-import");
    assert_eq!(first_export.description, second_export.description);
    assert_eq!(first_export.nodes, second_export.nodes);
    assert_eq!(first_export.edges, second_export.edges);
    assert_eq!(first_export.ensembles, second_export.ensembles);
}

/// Export → import → export of a graph holding one multi-member ensemble
/// with distinct platform/model per member plus a solo agent node:
/// every harness binding must survive identically.
#[test]
fn v2_export_import_export_preserves_bindings() {
    let lp = make_graph("bindings-graph");
    let solo = make_node(
        "solo",
        "solo",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "opencode", "model": "opencode/muse-spark", "prompt_template": "go solo", "timeout_minutes": 15}),
        1,
    );
    let downstream = make_node(
        "downstream",
        "downstream",
        GraphNodeKind::Check,
        serde_json::json!({"command": "true"}),
        10,
    );
    let member1 = make_node(
        "m1",
        "Crew [1]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "copilot", "prompt_template": "draft it", "timeout_minutes": 30}),
        2,
    );
    let member2 = make_node(
        "m2",
        "Crew [2]",
        GraphNodeKind::Agent,
        serde_json::json!({"platform": "opencode", "model": "opencode-go/qwen3.7-plus", "prompt_template": "draft it", "timeout_minutes": 30}),
        3,
    );
    let join = make_node(
        "join1",
        "Crew (quorum)",
        GraphNodeKind::Join,
        serde_json::json!({"ensemble_id": "ens1"}),
        4,
    );
    let nodes = vec![solo, downstream, member1, member2, join];
    let edges = vec![
        make_edge("e1", "solo", "m1", GraphEdgeCondition::Always),
        make_edge("e2", "solo", "m2", GraphEdgeCondition::Always),
        make_edge("e3", "m1", "join1", GraphEdgeCondition::Always),
        make_edge("e4", "m2", "join1", GraphEdgeCondition::Always),
        make_edge("e5", "join1", "downstream", GraphEdgeCondition::Pass),
    ];
    let mut details = make_ensemble_details("ens1", "join1", "solo", "downstream", &["m1", "m2"]);
    details.ensemble.name = "Crew".to_string();
    details.members[0].platform = "copilot".to_string();
    details.members[0].model = None;
    details.members[0].prompt_override = Some("first angle".to_string());
    details.members[1].platform = "opencode".to_string();
    details.members[1].model = Some("opencode-go/qwen3.7-plus".to_string());

    let first = build_export_document(&lp, &nodes, &edges, &[details]).unwrap();
    let plan = build_import_plan(&first, "graph-2").unwrap();
    assert!(agent_nodes_missing_platform(&plan).is_empty());

    let mut imported_all_nodes = plan.nodes.clone();
    let mut imported_details = Vec::new();
    for ens in &plan.ensembles {
        imported_all_nodes.push(ens.join_node.clone());
        imported_all_nodes.extend(ens.member_nodes.iter().cloned());
        imported_details.push(EnsembleDetails {
            ensemble: ens.ensemble.clone(),
            members: ens.members.clone(),
        });
    }
    let mut lp2 = make_graph("bindings-graph");
    lp2.id = "graph-2".to_string();
    let second =
        build_export_document(&lp2, &imported_all_nodes, &plan.edges, &imported_details).unwrap();

    // Harness bindings identical across the round trip, node and member.
    let first_solo = first.nodes.iter().find(|n| n.name == "solo").unwrap();
    let second_solo = second.nodes.iter().find(|n| n.name == "solo").unwrap();
    assert_eq!(
        first_solo.config["platform"],
        second_solo.config["platform"]
    );
    assert_eq!(first_solo.config["model"], second_solo.config["model"]);
    assert_eq!(first.ensembles, second.ensembles);
    assert_eq!(first.nodes, second.nodes);
}

/// Import must not fail when the document names a platform the importing
/// machine has not configured: the node is created as written, and
/// reporting an unusable pair belongs to `graph_preflight`.
#[test]
fn import_v2_with_unconfigured_platform_succeeds() {
    let doc = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 2,
        name: "x".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "odd".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({
                    "platform": "never-configured-xyz",
                    "model": "some-model-abc",
                    "prompt_template": "go",
                }),
            },
            GraphExportNode {
                name: "next".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({"command": "true"}),
            },
        ],
        edges: vec![GraphExportEdge {
            from_node: "odd".to_string(),
            to_node: "next".to_string(),
            condition: GraphEdgeCondition::Always,
        }],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let plan =
        build_import_plan(&doc, "new-graph").expect("unconfigured platform must import, not error");
    let odd = plan.nodes.iter().find(|n| n.name == "odd").unwrap();
    assert_eq!(odd.config["platform"], "never-configured-xyz");
    assert_eq!(odd.config["model"], "some-model-abc");
}

/// CB62 FR3: a field the document carries that the import does not know how
/// to restore is a hard error naming the field, not a silent default —
/// at the document, node, edge, ensemble, and member levels. An unknown
/// field inside a node's free-form `config` map is NOT covered here
/// (config keys have their own validator, validate_known_config_keys).
#[test]
fn unknown_fields_are_rejected_naming_the_field() {
    for value in [
        serde_json::json!({
            "format_version": 2,
            "name": "future-graph",
            "future_field": 123,
            "nodes": [],
            "edges": [],
            "ensembles": []
        }),
        serde_json::json!({
            "format_version": 2,
            "name": "future-graph",
            "nodes": [
                {"name": "a", "kind": "check", "position": 1, "config": {"command": "true"}, "future_node_field": "x"},
                {"name": "b", "kind": "check", "position": 2, "config": {"command": "true"}},
            ],
            "edges": [
                {"from_node": "a", "to_node": "b", "condition": "always", "future_edge_field": 1}
            ],
            "ensembles": []
        }),
        serde_json::json!({
            "format_version": 2,
            "name": "future-graph",
            "nodes": [
                {"name": "a", "kind": "check", "position": 1, "config": {"command": "true"}},
                {"name": "b", "kind": "check", "position": 2, "config": {"command": "true"}},
            ],
            "edges": [],
            "ensembles": [
                {
                    "name": "team",
                    "future_ensemble_field": true,
                    "prompt_template": "go",
                    "entry_from_node": "a",
                    "entry_condition": "always",
                    "on_pass_to": "b",
                    "min_pass": 2,
                    "timeout_minutes": 30,
                    "members": [
                        {"platform": "claude", "model": null, "prompt_override": null, "future_member_field": [1, 2]},
                        {"platform": "claude", "model": "opus", "prompt_override": null}
                    ]
                }
            ]
        }),
    ] {
        let err = parse_export_document_value(&value).unwrap_err();
        assert!(
            err.contains("future"),
            "unknown field must be a hard error naming the field: {err}"
        );
    }
}

/// Pins `docs/graphs.md`'s worked example to the actual format: if this
/// test ever fails to parse/import, the doc's example has drifted from
/// what the code accepts and needs updating alongside it.
#[test]
fn docs_worked_example_parses_and_imports_cleanly() {
    let raw = r#"{
          "format_version": 1,
          "name": "implement-and-review",
          "description": "Implement a spec, get two model opinions, then commit.",
          "nodes": [
            {
              "name": "implementer",
              "kind": "agent",
              "position": 1,
              "config": {
                "prompt_template": "Implement: {{spec_content}}",
                "timeout_minutes": 30
              }
            },
            {
              "name": "committer",
              "kind": "agent",
              "position": 4,
              "config": {
                "prompt_template": "Review the feedback and commit if satisfied.",
                "commit_rights": true,
                "timeout_minutes": 15
              }
            }
          ],
          "edges": [],
          "ensembles": [
            {
              "name": "reviewers",
              "prompt_template": "Review this diff for correctness: {{previous_feedback}}",
              "entry_from_node": "implementer",
              "entry_condition": "always",
              "on_pass_to": "committer",
              "min_pass": 2,
              "timeout_minutes": 20,
              "members": [
                {},
                {}
              ]
            }
          ]
        }"#;

    let document = parse_export_document_str(raw).expect("doc example must parse");
    let plan = build_import_plan(&document, "graph-from-docs").expect("doc example must import");
    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(plan.ensembles.len(), 1);
    assert_eq!(plan.ensembles[0].member_nodes.len(), 2);
    let missing = agent_nodes_missing_platform(&plan);
    assert_eq!(
        missing.len(),
        4,
        "implementer, committer, and both reviewer members are missing a platform: {missing:?}"
    );
}

#[test]
fn import_plan_rejects_ensemble_with_too_few_members() {
    let doc = GraphExportDocument {
        prompt_presets: Vec::new(),
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "kickoff".to_string(),
                kind: GraphNodeKind::Check,
                position: 1,
                config: serde_json::json!({"command": "true"}),
            },
            GraphExportNode {
                name: "next".to_string(),
                kind: GraphNodeKind::Check,
                position: 2,
                config: serde_json::json!({"command": "true"}),
            },
        ],
        edges: vec![],
        ensembles: vec![GraphExportEnsemble {
            mutex: Vec::new(),
            commit_rights: false,
            name: "solo".to_string(),
            kind: None,
            prompt_template: "go".to_string(),
            entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
            entry_condition: GraphEdgeCondition::Always,
            extra_entry_sources: vec![],
            on_pass_to: GraphExportEnsembleTarget::Node("next".to_string()),
            on_fail_to: None,
            min_pass: 1,
            timeout_minutes: 30,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: vec![GraphExportEnsembleMember {
                platform: Some("claude".to_string()),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
            }],

            crew: None,
        }],
        infra_node: None,

        hooks: Default::default(),
    };
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(err.contains("2-8 members"));
}

/// CM36: a round_robin ensemble dispatches one member at a time, so 12
/// members import fine (the parallel 8 cap must not apply to it).
#[test]
fn import_round_robin_accepts_12_members() {
    let doc = make_twelve_member_doc(Some("round_robin"));
    build_import_plan(&doc, "g").expect("round_robin accepts 12 members");
}

/// CM36: parallel still caps at 8 — the same shape with kind parallel is
/// refused once it exceeds 8 members, and the error names the kind.
#[test]
fn import_parallel_rejects_9_members() {
    let mut doc = make_twelve_member_doc(Some("parallel"));
    doc.ensembles[0].members.truncate(9);
    let err = build_import_plan(&doc, "g").unwrap_err();
    assert!(err.contains("2-8 members"), "{err}");
    assert!(err.contains("parallel"), "{err}");
}
