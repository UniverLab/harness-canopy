//! Part 02 of `tests` (split from `src/db/ensembles/mod.rs`, pure move).
use super::*;

/// A member's `prompt_override` round-trips through `insert_ensemble_unit`
/// and `get_ensemble_details` — `Some` for a member that has one, `None`
/// for a member that doesn't (using the shared `prompt_template` exactly
/// as every ensemble did before this field existed).
#[test]
fn insert_ensemble_unit_round_trips_member_prompt_override() {
    let db = test_db();
    insert_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: Utc::now(),
    })
    .unwrap();
    let now = Utc::now();
    let member_nodes = vec![
        GraphNode {
            id: "m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "review for security"}),
            position: 2,
            created_at: now,
        },
        GraphNode {
            id: "m2".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-2".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "draft it"}),
            position: 3,
            created_at: now,
        },
    ];
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "ens1"}),
        position: 4,
        created_at: now,
    };
    let edges = vec![
        GraphEdge {
            id: "kickoff->m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "kickoff->m2".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m2".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m1->join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "m1".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m2->join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "m2".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        },
    ];
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Proposers".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 2,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: Some("review for security".to_string()),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m2".to_string(),
            position: 1,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    let details = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(
        details.members[0].prompt_override.as_deref(),
        Some("review for security")
    );
    assert_eq!(details.members[1].prompt_override, None);
}

/// A member's `timeout_minutes` round-trips through `insert_ensemble_unit`
/// and `get_ensemble_details` — `Some` for a member with its own override,
/// `None` for a member that uses the ensemble's shared `timeout_minutes`
/// (CM23).
#[test]
fn insert_ensemble_unit_round_trips_member_timeout_minutes() {
    let db = test_db();
    insert_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: Utc::now(),
    })
    .unwrap();
    let now = Utc::now();
    let member_nodes = vec![
        GraphNode {
            id: "m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "draft it", "timeout_minutes": 7}),
            position: 2,
            created_at: now,
        },
        GraphNode {
            id: "m2".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-2".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "draft it", "timeout_minutes": 30}),
            position: 3,
            created_at: now,
        },
    ];
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "ens1"}),
        position: 4,
        created_at: now,
    };
    let edges = vec![
        GraphEdge {
            id: "kickoff->m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "kickoff->m2".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m2".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m1->join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "m1".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m2->join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "m2".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        },
    ];
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Proposers".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 2,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: Some(7),
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m2".to_string(),
            position: 1,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    let details = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(details.members[0].timeout_minutes, Some(7));
    assert_eq!(details.members[1].timeout_minutes, None);
}

/// `update_ensemble_member` (the renamed `update_ensemble_member_platform`)
/// sets `prompt_override` outright, in both directions: giving a member
/// that had none its own override, and clearing an existing override back
/// to `None` (the "use the shared prompt" state) — both are ordinary
/// `Some`/`None` writes, never a "leave unchanged" skip, since a
/// replacement member list is always given in full.
#[test]
fn update_ensemble_member_sets_and_clears_prompt_override() {
    let db = test_db();
    insert_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: Utc::now(),
    })
    .unwrap();
    let now = Utc::now();
    let member_nodes = vec![GraphNode {
        id: "m1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "member-1".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude", "prompt_template": "draft it"}),
        position: 2,
        created_at: now,
    }];
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "ens1"}),
        position: 3,
        created_at: now,
    };
    let edges = vec![
        GraphEdge {
            id: "kickoff->m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m1->join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "m1".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        },
    ];
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Solo".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    db.update_ensemble_member("ens1", "m1", "claude", None, Some("new angle"), None)
        .unwrap();
    let after_set = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(
        after_set.members[0].prompt_override.as_deref(),
        Some("new angle")
    );

    db.update_ensemble_member("ens1", "m1", "claude", None, None, None)
        .unwrap();
    let after_clear = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(after_clear.members[0].prompt_override, None);
}

/// `update_ensemble_member`'s new `timeout_minutes` parameter (CM23) is an
/// ordinary `Some`/`None` write, same convention as `prompt_override`:
/// setting a member's own override, then clearing it back to `None` (the
/// "use the ensemble's timeout_minutes" state).
#[test]
fn update_ensemble_member_sets_and_clears_member_timeout() {
    let db = test_db();
    insert_spec(&db, "spec-1");
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: Utc::now(),
    })
    .unwrap();
    let now = Utc::now();
    let member_nodes = vec![GraphNode {
        id: "m1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "member-1".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude", "prompt_template": "draft it"}),
        position: 2,
        created_at: now,
    }];
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "ens1"}),
        position: 3,
        created_at: now,
    };
    let edges = vec![
        GraphEdge {
            id: "kickoff->m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "m1->join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "m1".to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        },
        GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        },
    ];
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "Solo".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    db.update_ensemble_member("ens1", "m1", "claude", None, None, Some(3))
        .unwrap();
    let after_set = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(after_set.members[0].timeout_minutes, Some(3));

    db.update_ensemble_member("ens1", "m1", "claude", None, None, None)
        .unwrap();
    let after_clear = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(after_clear.members[0].timeout_minutes, None);
}

/// A blueprint row seeded before `prompt_override`/`timeout_minutes`
/// existed stores its `members` JSON as 2-element `[platform, model]` or
/// 3-element `[platform, model, prompt_override]` arrays — an upgrade
/// must still be able to read those rows back (as "no override for any
/// member") without a data migration, alongside the current 4-element
/// shape (CM23).
#[test]
fn parse_ensemble_blueprint_members_tolerates_legacy_two_element_rows() {
    let legacy = r#"[["openrouter","deepseek/deepseek-chat-v3.1:free"],["claude",null]]"#;
    let parsed = parse_ensemble_blueprint_members(legacy).unwrap();
    assert_eq!(
        parsed,
        vec![
            (
                "openrouter".to_string(),
                Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                None,
                None,
                None,
                None,
                None
            ),
            ("claude".to_string(), None, None, None, None, None, None),
        ]
    );

    let three_element =
        r#"[["openrouter","deepseek/deepseek-chat-v3.1:free","review it"],["claude",null,null]]"#;
    let parsed = parse_ensemble_blueprint_members(three_element).unwrap();
    assert_eq!(
        parsed,
        vec![
            (
                "openrouter".to_string(),
                Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                Some("review it".to_string()),
                None,
                None,
                None,
                None
            ),
            ("claude".to_string(), None, None, None, None, None, None),
        ]
    );

    let current = r#"[["openrouter","deepseek/deepseek-chat-v3.1:free","review it",3],["claude",null,null,null]]"#;
    let parsed = parse_ensemble_blueprint_members(current).unwrap();
    assert_eq!(
        parsed,
        vec![
            (
                "openrouter".to_string(),
                Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                Some("review it".to_string()),
                Some(3),
                None,
                None,
                None
            ),
            ("claude".to_string(), None, None, None, None, None, None),
        ]
    );
}

#[test]
fn insert_ensemble_unit_with_cascade_kind_persists_kind() {
    let db = test_db();
    let spec_id = "spec-1".to_string();
    insert_spec(&db, &spec_id);
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let now = chrono::Utc::now();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({}),
        position: 100,
        created_at: now,
    };
    let member_node = GraphNode {
        id: "m1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "member".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 50,
        created_at: now,
    };
    let edge = GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "m1".to_string(),
        condition: GraphEdgeCondition::Always,
    };
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id),
        graph_id: None,
        name: "Cascade".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Cascade,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];

    db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
        .unwrap();

    let loaded = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(loaded.kind, EnsembleKind::Cascade);
    assert_eq!(loaded.round_robin_index, None);
}

#[test]
fn insert_ensemble_unit_with_round_robin_kind_persists_index() {
    let db = test_db();
    let spec_id = "spec-1".to_string();
    insert_spec(&db, &spec_id);
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let now = chrono::Utc::now();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({}),
        position: 100,
        created_at: now,
    };
    let member_node = GraphNode {
        id: "m1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "member".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 50,
        created_at: now,
    };
    let edge = GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "m1".to_string(),
        condition: GraphEdgeCondition::Always,
    };
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id),
        graph_id: None,
        name: "RoundRobin".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::RoundRobin,
        round_robin_index: Some(0),
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];

    db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
        .unwrap();

    let loaded = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(loaded.kind, EnsembleKind::RoundRobin);
    assert_eq!(loaded.round_robin_index, Some(0));
}

#[test]
fn update_ensemble_kind_changes_kind_and_clears_index() {
    let db = test_db();
    let spec_id = "spec-1".to_string();
    insert_spec(&db, &spec_id);
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let now = chrono::Utc::now();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({}),
        position: 100,
        created_at: now,
    };
    let member_node = GraphNode {
        id: "m1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "member".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 50,
        created_at: now,
    };
    let edge = GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "m1".to_string(),
        condition: GraphEdgeCondition::Always,
    };
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id),
        graph_id: None,
        name: "RR".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::RoundRobin,
        round_robin_index: Some(2),
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];

    db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
        .unwrap();

    db.update_ensemble_kind("ens1", Some(EnsembleKind::Cascade), Some(None))
        .unwrap();

    let loaded = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(loaded.kind, EnsembleKind::Cascade);
    assert_eq!(loaded.round_robin_index, None);
}

#[test]
fn round_robin_index_advances_and_wraps() {
    let db = test_db();
    let spec_id = "spec-1".to_string();
    insert_spec(&db, &spec_id);
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "arbiter".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let now = chrono::Utc::now();

    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({}),
        position: 100,
        created_at: now,
    };
    let member_node = GraphNode {
        id: "m1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "member".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 50,
        created_at: now,
    };
    let edge = GraphEdge {
        id: "e1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        from_node: "kickoff".to_string(),
        to_node: "m1".to_string(),
        condition: GraphEdgeCondition::Always,
    };
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some(spec_id),
        graph_id: None,
        name: "RR".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 1,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::RoundRobin,
        round_robin_index: Some(0),
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m1".to_string(),
        position: 0,
        platform: "claude".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];

    db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
        .unwrap();

    db.update_ensemble_kind("ens1", Some(EnsembleKind::RoundRobin), Some(Some(1)))
        .unwrap();
    let loaded = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(loaded.round_robin_index, Some(1));

    db.update_ensemble_kind("ens1", Some(EnsembleKind::RoundRobin), Some(Some(2)))
        .unwrap();
    let loaded = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(loaded.round_robin_index, Some(2));

    db.update_ensemble_kind("ens1", Some(EnsembleKind::RoundRobin), Some(Some(0)))
        .unwrap();
    let loaded = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(loaded.round_robin_index, Some(0));
}
