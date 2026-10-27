//! Part 02 of `tests` (split from `src/domain/graph_transfer/mod.rs`, pure move).
use super::*;

/// CM36 FR2: a sequential import refusal names the kind, e.g. a 17-member
/// round_robin is refused with `2-16` and `round_robin` in the message.
#[test]
fn import_round_robin_names_kind_when_over_16() {
    let mut doc = make_twelve_member_doc(Some("round_robin"));
    doc.ensembles[0]
        .members
        .extend((0..5).map(|_| GraphExportEnsembleMember {
            platform: Some("claude".to_string()),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }));
    let err = build_import_plan(&doc, "g").unwrap_err();
    assert!(err.contains("2-16 members"), "{err}");
    assert!(err.contains("round_robin"), "{err}");
    assert!(err.contains("17"), "{err}");
}

/// CM24: a negative `quorum_grace_minutes` is refused on import with the
/// same wording pattern as the existing `straggler_timeout_minutes`
/// check — while `0` (terminate immediately) imports fine.
#[test]
fn import_plan_rejects_negative_quorum_grace() {
    let doc = GraphExportDocument {
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
            name: "team".to_string(),
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
            quorum_grace_minutes: Some(-1),
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
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
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(
        err.contains("negative quorum_grace_minutes"),
        "unexpected error: {err}"
    );
}

/// CM24: `quorum_grace_minutes` survives the export→import round trip,
/// including `0`, and an absent field imports as `None` (old documents
/// keep the wait-for-all behaviour).
#[test]
fn import_plan_round_trips_quorum_grace_minutes() {
    let doc = GraphExportDocument {
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
            name: "team".to_string(),
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
            quorum_grace_minutes: Some(0),
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
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
    let plan = build_import_plan(&doc, "new-graph").unwrap();
    assert_eq!(plan.ensembles[0].ensemble.quorum_grace_minutes, Some(0));
}

/// CM23 (FR6, requirement 5's import half): a member's own exported
/// `timeout_minutes` is what gets baked into that member's node config on
/// import — not the ensemble's shared value — while a sibling with no
/// override still falls back to the ensemble's `timeout_minutes`.
#[test]
fn import_plan_bakes_member_timeout_override_into_node_config() {
    let doc = GraphExportDocument {
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
            name: "team".to_string(),
            kind: None,
            prompt_template: "go".to_string(),
            entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
            entry_condition: GraphEdgeCondition::Always,
            extra_entry_sources: vec![],
            on_pass_to: GraphExportEnsembleTarget::Node("next".to_string()),
            on_fail_to: None,
            min_pass: 1,
            timeout_minutes: 20,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: Some(3),
                },
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
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
    let plan = build_import_plan(&doc, "new-graph").unwrap();
    let ensemble_plan = &plan.ensembles[0];
    assert_eq!(ensemble_plan.members[0].timeout_minutes, Some(3));
    assert_eq!(
        ensemble_plan.member_nodes[0].config["timeout_minutes"],
        serde_json::json!(3)
    );
    assert_eq!(ensemble_plan.members[1].timeout_minutes, None);
    assert_eq!(
        ensemble_plan.member_nodes[1].config["timeout_minutes"],
        serde_json::json!(20)
    );
}

#[test]
fn import_plan_rejects_unreachable_node() {
    // A -> B, C self-graph (single entry A, C unreachable)
    let doc = GraphExportDocument {
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "A".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({"platform": "claude", "prompt_template": "a"}),
            },
            GraphExportNode {
                name: "B".to_string(),
                kind: GraphNodeKind::Agent,
                position: 2,
                config: serde_json::json!({"platform": "claude", "prompt_template": "b"}),
            },
            GraphExportNode {
                name: "C".to_string(),
                kind: GraphNodeKind::Agent,
                position: 3,
                config: serde_json::json!({"platform": "claude", "prompt_template": "c"}),
            },
        ],
        edges: vec![
            GraphExportEdge {
                from_node: "A".to_string(),
                to_node: "B".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphExportEdge {
                from_node: "C".to_string(),
                to_node: "C".to_string(),
                condition: GraphEdgeCondition::Always,
            },
        ],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(err.contains('C'), "err should name C: {err}");
    assert!(
        err.to_lowercase().contains("unreachable"),
        "err should mention unreachable: {err}"
    );
}

#[test]
fn import_plan_rejects_multiple_entry_points() {
    // A and B both with no incoming => multiple entries
    let doc = GraphExportDocument {
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "A".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({"platform": "claude", "prompt_template": "a"}),
            },
            GraphExportNode {
                name: "B".to_string(),
                kind: GraphNodeKind::Agent,
                position: 2,
                config: serde_json::json!({"platform": "claude", "prompt_template": "b"}),
            },
            GraphExportNode {
                name: "C".to_string(),
                kind: GraphNodeKind::Agent,
                position: 3,
                config: serde_json::json!({"platform": "claude", "prompt_template": "c"}),
            },
        ],
        edges: vec![GraphExportEdge {
            from_node: "A".to_string(),
            to_node: "C".to_string(),
            condition: GraphEdgeCondition::Always,
        }],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(err.to_lowercase().contains("entry"), "err: {err}");
    assert!(err.contains('A'), "err should name A: {err}");
    assert!(err.contains('B'), "err should name B: {err}");
}

#[test]
fn import_plan_accepts_agent_missing_fail_edge_as_terminal() {
    // Resilience node with only pass edge -> fail is a terminal exit,
    // not an error (same verdict as graph_preflight).
    let doc = GraphExportDocument {
        format_version: 1,
        name: "x".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "resilience".to_string(),
                kind: GraphNodeKind::Agent,
                position: 1,
                config: serde_json::json!({"platform": "claude", "prompt_template": "go"}),
            },
            GraphExportNode {
                name: "next".to_string(),
                kind: GraphNodeKind::Agent,
                position: 2,
                config: serde_json::json!({"platform": "claude", "prompt_template": "next"}),
            },
        ],
        edges: vec![GraphExportEdge {
            from_node: "resilience".to_string(),
            to_node: "next".to_string(),
            condition: GraphEdgeCondition::Pass,
        }],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let plan =
        build_import_plan(&doc, "new-graph").expect("missing fail is a terminal, not an error");
    assert_eq!(plan.nodes.len(), 2);
    assert_eq!(plan.terminals.len(), 3);
    assert!(plan
        .terminals
        .iter()
        .any(|terminal| terminal.starts_with("resilience (")
            && terminal.ends_with(") ends on 'fail'")));
}

#[test]
fn build_import_plan_rejects_unknown_node_reference() {
    let doc = GraphExportDocument {
        format_version: GRAPH_EXPORT_FORMAT_VERSION,
        name: "test".to_string(),
        description: None,
        nodes: vec![GraphExportNode {
            name: "Worker".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "test",
                "prompt_template": "See {{output:Nonexistent}}"
            }),
            position: 1,
        }],
        edges: vec![],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(
        err.contains("references unknown node 'Nonexistent'"),
        "err: {err}"
    );
}

#[test]
fn build_import_plan_accepts_valid_node_reference() {
    let doc = GraphExportDocument {
        format_version: GRAPH_EXPORT_FORMAT_VERSION,
        name: "test".to_string(),
        description: None,
        nodes: vec![
            GraphExportNode {
                name: "Architect".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "test"}),
                position: 1,
            },
            GraphExportNode {
                name: "Implementer".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({
                    "platform": "test",
                    "prompt_template": "Follow {{output:Architect}}"
                }),
                position: 2,
            },
            GraphExportNode {
                name: "Resilience".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "test"}),
                position: 3,
            },
        ],
        edges: vec![
            GraphExportEdge {
                from_node: "Architect".to_string(),
                to_node: "Implementer".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
            GraphExportEdge {
                from_node: "Architect".to_string(),
                to_node: "Resilience".to_string(),
                condition: GraphEdgeCondition::Fail,
            },
            GraphExportEdge {
                from_node: "Implementer".to_string(),
                to_node: "Resilience".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
            GraphExportEdge {
                from_node: "Implementer".to_string(),
                to_node: "Resilience".to_string(),
                condition: GraphEdgeCondition::Fail,
            },
        ],
        ensembles: vec![],
        infra_node: None,

        hooks: Default::default(),
    };
    let result = build_import_plan(&doc, "new-graph");
    assert!(
        result.is_ok(),
        "valid {{output:NodeName}} reference must be accepted: {:?}",
        result.err()
    );
}

/// CB62 requirement 4 (domain half): export → import → export of one
/// ensemble of every kind, each carrying every optional field, compared
/// as whole serialized documents. Derived from the serialized shape: a
/// field added to the export structs later that import does not restore
/// shows up as a Value diff — this test fails until it is handled.
#[test]
fn every_document_field_round_trips_through_the_plan() {
    let lp = make_graph("cb62-domain");
    let nodes = vec![
        make_node(
            "kickoff",
            "kickoff",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            1,
        ),
        make_node(
            "final",
            "final",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            2,
        ),
        make_node(
            "am1",
            "Alpha [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "copilot", "model": "model-a", "prompt_template": "angle a", "timeout_minutes": 15}),
            3,
        ),
        make_node(
            "am2",
            "Alpha [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "opencode", "prompt_template": "draft", "timeout_minutes": 15}),
            4,
        ),
        make_node(
            "aj",
            "Alpha (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens1"}),
            5,
        ),
        make_node(
            "bm1",
            "Beta [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "model": "sonnet", "prompt_template": "review", "timeout_minutes": 30}),
            6,
        ),
        make_node(
            "bm2",
            "Beta [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_template": "review", "timeout_minutes": 30}),
            7,
        ),
        make_node(
            "bj",
            "Beta (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens2"}),
            8,
        ),
        make_node(
            "cm",
            "Gamma [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_template": "check", "timeout_minutes": 20}),
            9,
        ),
        make_node(
            "cn",
            "Gamma [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "copilot", "prompt_template": "check", "timeout_minutes": 20}),
            10,
        ),
        make_node(
            "cj",
            "Gamma (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens3"}),
            11,
        ),
    ];
    let edges = vec![
        make_edge("e1", "kickoff", "am1", GraphEdgeCondition::Always),
        make_edge("e2", "kickoff", "am2", GraphEdgeCondition::Always),
        make_edge("e3", "am1", "aj", GraphEdgeCondition::Always),
        make_edge("e4", "am2", "aj", GraphEdgeCondition::Always),
        // Alpha passes into Beta (chained: fan-out to every Beta member)
        // and fails to final.
        make_edge("e5", "aj", "bm1", GraphEdgeCondition::Pass),
        make_edge("e6", "aj", "final", GraphEdgeCondition::Fail),
        // Beta's own primary entry from kickoff — so Beta's members each
        // have TWO entry sources (kickoff's always + Alpha's pass), the
        // "multiple entry sources" shape.
        make_edge("e7", "kickoff", "bm1", GraphEdgeCondition::Always),
        make_edge("e7b", "kickoff", "bm2", GraphEdgeCondition::Always),
        make_edge("e8", "bm1", "bj", GraphEdgeCondition::Always),
        make_edge("e8b", "bm2", "bj", GraphEdgeCondition::Always),
        make_edge("e5b", "aj", "bm2", GraphEdgeCondition::Pass),
        // Beta passes into Gamma (chained fan-out to both Gamma members).
        make_edge("e9", "bj", "cm", GraphEdgeCondition::Pass),
        make_edge("e10", "bj", "cn", GraphEdgeCondition::Pass),
        // Gamma's own entry + quorum exit.
        make_edge("e11", "kickoff", "cm", GraphEdgeCondition::Always),
        make_edge("e12", "kickoff", "cn", GraphEdgeCondition::Always),
        make_edge("e13", "cm", "cj", GraphEdgeCondition::Always),
        make_edge("e14", "cn", "cj", GraphEdgeCondition::Always),
        make_edge("e15", "cj", "final", GraphEdgeCondition::Pass),
    ];
    // Alpha: cascade, commit rights, member overrides, on_fail_to, straggler.
    let mut alpha = make_ensemble_details("ens1", "aj", "kickoff", "bj", &["am1", "am2"]);
    alpha.ensemble.name = "Alpha".to_string();
    alpha.ensemble.prompt_template = "draft".to_string();
    alpha.ensemble.kind = EnsembleKind::Cascade;
    alpha.ensemble.commit_rights = true;
    alpha.ensemble.min_pass = 1;
    alpha.ensemble.timeout_minutes = 15;
    alpha.ensemble.straggler_timeout_minutes = Some(7);
    alpha.ensemble.on_fail_to = Some("final".to_string());
    alpha.members[0].platform = "copilot".to_string();
    alpha.members[0].model = Some("model-a".to_string());
    alpha.members[0].prompt_override = Some("angle a".to_string());
    alpha.members[0].timeout_minutes = Some(15);
    alpha.members[1].platform = "opencode".to_string();
    alpha.members[1].model = None;
    // Beta: round_robin, 2 members (the per-kind minimum).
    let mut beta = make_ensemble_details("ens2", "bj", "kickoff", "cj", &["bm1", "bm2"]);
    beta.ensemble.name = "Beta".to_string();
    beta.ensemble.prompt_template = "review".to_string();
    beta.ensemble.kind = EnsembleKind::RoundRobin;
    beta.ensemble.min_pass = 1;
    beta.ensemble.timeout_minutes = 30;
    beta.ensemble.straggler_timeout_minutes = Some(5);
    beta.members[0].platform = "claude".to_string();
    beta.members[0].model = Some("sonnet".to_string());
    beta.members[1].platform = "claude".to_string();
    beta.members[1].model = None;
    // Gamma: parallel, grace field (parallel-only) exercised.
    let mut gamma = make_ensemble_details("ens3", "cj", "kickoff", "final", &["cm", "cn"]);
    gamma.ensemble.name = "Gamma".to_string();
    gamma.ensemble.prompt_template = "check".to_string();
    gamma.ensemble.kind = EnsembleKind::Parallel;
    gamma.ensemble.min_pass = 1;
    gamma.ensemble.timeout_minutes = 20;
    gamma.ensemble.quorum_grace_minutes = Some(2);
    gamma.members[0].platform = "claude".to_string();
    gamma.members[0].model = None;
    gamma.members[1].platform = "copilot".to_string();
    gamma.members[1].model = None;
    let first = build_export_document(&lp, &nodes, &edges, &[alpha, beta, gamma]).unwrap();

    let plan = build_import_plan(&first, "graph-2").unwrap();

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
    let mut lp2 = make_graph("cb62-domain");
    lp2.id = "graph-2".to_string();
    let second =
        build_export_document(&lp2, &imported_all_nodes, &plan.edges, &imported_details).unwrap();

    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&second).unwrap(),
        "CB62: document must be identical after an export/import round trip"
    );
}

#[test]
fn import_restores_each_ensemble_kind() {
    let plan = import_kind_fixture(None);
    assert_eq!(plan.ensembles[0].ensemble.kind, EnsembleKind::Parallel);
    assert_eq!(plan.ensembles[0].ensemble.round_robin_index, None);
    let plan = import_kind_fixture(Some("cascade"));
    assert_eq!(plan.ensembles[0].ensemble.kind, EnsembleKind::Cascade);
    assert_eq!(plan.ensembles[0].ensemble.round_robin_index, None);
    let plan = import_kind_fixture(Some("round_robin"));
    assert_eq!(plan.ensembles[0].ensemble.kind, EnsembleKind::RoundRobin);
    assert_eq!(plan.ensembles[0].ensemble.round_robin_index, Some(0));
}

#[test]
fn import_rejects_unknown_ensemble_kind() {
    let doc = GraphExportDocument {
        format_version: 2,
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
            name: "Reviewers".to_string(),
            kind: Some("hyper".to_string()),
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
            members: vec![
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                },
                GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
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
    let err = build_import_plan(&doc, "new-graph").unwrap_err();
    assert!(err.contains("kind"), "err must name the field: {err}");
    assert!(err.contains("hyper"), "err must name the value: {err}");
    assert!(
        err.contains("Reviewers"),
        "err must name the ensemble: {err}"
    );
}

#[test]
fn import_rejects_unknown_document_field() {
    let value = serde_json::json!({
        "format_version": 2,
        "name": "x",
        "nodes": [],
        "edges": [],
        "ensembles": [],
        "bogus_field": 1
    });
    let err = parse_export_document_value(&value).unwrap_err();
    assert!(
        err.contains("bogus_field"),
        "err must name the field: {err}"
    );
    let value = serde_json::json!({
        "format_version": 2,
        "name": "x",
        "nodes": [],
        "edges": [],
        "ensembles": [
            {
                "name": "team",
                "prompt_template": "go",
                "entry_from_node": "a",
                "entry_condition": "always",
                "on_pass_to": "b",
                "min_pass": 2,
                "timeout_minutes": 30,
                "bogus_field": 1,
                "members": [
                    {"platform": "claude"},
                    {"platform": "claude"}
                ]
            }
        ]
    });
    let err = parse_export_document_value(&value).unwrap_err();
    assert!(
        err.contains("bogus_field"),
        "err must name the field: {err}"
    );
}

/// CM49 import: a crew-bound ensemble travels without members —
/// `graph_export` writes it with `crew` and no `members`, so a document
/// that omits the key entirely must parse rather than fail with a serde
/// "missing field" error.
#[test]
fn import_accepts_crew_bound_ensemble_without_members_field() {
    let value = serde_json::json!({
        "format_version": 5,
        "name": "crew-graph",
        "nodes": [
            {"name": "kickoff", "kind": "check", "position": 1, "config": {"command": "true"}},
            {"name": "final", "kind": "check", "position": 2, "config": {"command": "true"}}
        ],
        "edges": [],
        "ensembles": [{
            "name": "Team",
            "prompt_template": "go",
            "entry_from_node": "kickoff",
            "entry_condition": "always",
            "on_pass_to": "final",
            "min_pass": 2,
            "timeout_minutes": 30,
            "crew": "reviewers"
        }]
    });
    let document =
        parse_export_document_value(&value).expect("crew-bound ensemble may omit members");
    let plan = build_import_plan(&document, "new-graph").expect("crew-bound ensemble imports");
    assert_eq!(
        plan.ensembles[0].ensemble.crew_name.as_deref(),
        Some("reviewers")
    );
    assert!(plan.ensembles[0].members.is_empty());
}

/// An ensemble with neither `crew` nor `members` is still refused, and the
/// refusal names the ensemble — `members` defaults to empty, so this must
/// not slip through as a zero-member ensemble.
#[test]
fn import_refuses_ensemble_without_crew_or_members_naming_it() {
    let value = serde_json::json!({
        "format_version": 5,
        "name": "x",
        "nodes": [
            {"name": "kickoff", "kind": "check", "position": 1, "config": {"command": "true"}},
            {"name": "final", "kind": "check", "position": 2, "config": {"command": "true"}}
        ],
        "edges": [],
        "ensembles": [{
            "name": "Orphan",
            "prompt_template": "go",
            "entry_from_node": "kickoff",
            "entry_condition": "always",
            "on_pass_to": "final",
            "min_pass": 1,
            "timeout_minutes": 30
        }]
    });
    let document = parse_export_document_value(&value).unwrap();
    let err = build_import_plan(&document, "new-graph").unwrap_err();
    assert!(err.contains("Orphan"), "err must name the ensemble: {err}");
    assert!(err.contains("2-8 members"), "unexpected error: {err}");
}

/// A node's `mutex` config and an ensemble's `mutex` list survive an
/// export/import round trip, and the exporter stamps `format_version: 6`.
#[test]
fn round_trip_keeps_mutex() {
    let lp = make_graph("mutex-graph");
    let kickoff = make_node(
        "kickoff",
        "kickoff",
        GraphNodeKind::Check,
        serde_json::json!({
            "command": "cargo test --workspace",
            "success_condition": "exit_code_0",
            "mutex": ["cpu"],
        }),
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
    let mut details =
        make_ensemble_details("ens1", "join1", "kickoff", "downstream", &["m1", "m2"]);
    details.ensemble.mutex = vec!["gpu".to_string()];

    let doc = build_export_document(&lp, &nodes, &edges, &[details]).unwrap();
    assert_eq!(doc.format_version, GRAPH_EXPORT_FORMAT_VERSION);
    assert_eq!(doc.format_version, 6);
    let exported_node = doc.nodes.iter().find(|n| n.name == "kickoff").unwrap();
    assert_eq!(exported_node.config["mutex"], serde_json::json!(["cpu"]));
    assert_eq!(doc.ensembles.len(), 1);
    assert_eq!(doc.ensembles[0].mutex, vec!["gpu".to_string()]);

    let plan = build_import_plan(&doc, "graph-2").unwrap();
    let imported_node = plan.nodes.iter().find(|n| n.name == "kickoff").unwrap();
    assert_eq!(imported_node.config["mutex"], serde_json::json!(["cpu"]));
    assert_eq!(plan.ensembles.len(), 1);
    assert_eq!(plan.ensembles[0].ensemble.mutex, vec!["gpu".to_string()]);
}

/// A `format_version: 5` document — written before ensembles carried
/// `mutex` — still imports, with the field treated as absent.
#[test]
fn v5_document_imports_without_mutex() {
    let raw = serde_json::json!({
        "format_version": 5,
        "name": "old-graph",
        "description": null,
        "hooks": {},
        "infra_node": null,
        "nodes": [
            {
                "name": "kickoff",
                "kind": "check",
                "position": 1,
                "config": {"command": "true"},
            },
            {
                "name": "next",
                "kind": "check",
                "position": 2,
                "config": {"command": "true"},
            },
        ],
        "edges": [],
        "ensembles": [
            {
                "name": "team",
                "kind": "parallel",
                "commit_rights": false,
                "prompt_template": "go",
                "entry_from_node": "kickoff",
                "entry_condition": "always",
                "extra_entry_sources": [],
                "on_pass_to": "next",
                "on_fail_to": null,
                "min_pass": 1,
                "timeout_minutes": 30,
                "straggler_timeout_minutes": null,
                "quorum_grace_minutes": null,
                "crew": null,
                "members": [
                    {"platform": "claude", "model": null, "prompt_override": null, "timeout_minutes": null},
                    {"platform": "codex", "model": null, "prompt_override": null, "timeout_minutes": null},
                ],
            },
        ],
    });
    let doc: GraphExportDocument =
        serde_json::from_value(raw).expect("a v5 document parses without mutex");
    assert!(doc.ensembles[0].mutex.is_empty());
    let plan = build_import_plan(&doc, "graph-2").unwrap();
    assert_eq!(plan.ensembles.len(), 1);
    assert!(plan.ensembles[0].ensemble.mutex.is_empty());
}
