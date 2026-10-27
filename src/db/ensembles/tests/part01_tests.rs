//! Part 01 of `tests` (split from `src/db/ensembles/mod.rs`, pure move).
use super::*;

/// The DB-layer half of the "one call -> N+1 nodes" contract: given the
/// exact node/edge/ensemble shape `graph_add_ensemble` assembles for 3
/// members, one `insert_ensemble_unit` transaction must leave behind 1
/// join node + 3 member nodes (4 new `graph_nodes` rows total), the
/// entry fan-out edge per member, the member->join fan-in edge per
/// member, and the join's own pass edge — all in one shot.
/// CM52: lowering `graphs.ensemble_max_members_parallel` below an
/// existing ensemble's member count never edits that ensemble — the cap
/// is a bound on new writes, not a migration.
#[test]
fn lowering_the_member_cap_leaves_an_existing_ensemble_untouched() {
    let db = test_db();
    insert_spec(&db, "spec-cap");
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-cap".to_string()),
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
        spec_id: Some("spec-cap".to_string()),
        graph_id: None,
        name: "arbiter".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 10,
        created_at: Utc::now(),
    })
    .unwrap();

    let now = Utc::now();
    let member_ids: Vec<String> = (0..6).map(|i| format!("m{i}")).collect();
    let member_nodes: Vec<GraphNode> = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| GraphNode {
            id: id.clone(),
            spec_id: Some("spec-cap".to_string()),
            graph_id: None,
            name: format!("member-{i}"),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "openrouter", "prompt_template": "draft it"}),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();
    let join_node = GraphNode {
        id: "join-cap".to_string(),
        spec_id: Some("spec-cap".to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "ens-cap"}),
        position: 9,
        created_at: now,
    };
    let mut edges = Vec::new();
    for id in &member_ids {
        edges.push(GraphEdge {
            id: format!("kickoff->{id}"),
            spec_id: Some("spec-cap".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: id.clone(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{id}->join-cap"),
            spec_id: Some("spec-cap".to_string()),
            graph_id: None,
            from_node: id.clone(),
            to_node: "join-cap".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: "join-cap->arbiter".to_string(),
        spec_id: Some("spec-cap".to_string()),
        graph_id: None,
        from_node: "join-cap".to_string(),
        to_node: "arbiter".to_string(),
        condition: GraphEdgeCondition::Pass,
    });
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens-cap".to_string(),
        spec_id: Some("spec-cap".to_string()),
        graph_id: None,
        name: "Six".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: "join-cap".to_string(),
        entry_from_node: "kickoff".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 6,
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
    let members: Vec<EnsembleMember> = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| EnsembleMember {
            ensemble_id: "ens-cap".to_string(),
            node_id: id.clone(),
            position: i as i64,
            platform: "openrouter".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    let before = db.list_ensemble_members("ens-cap").unwrap();
    assert_eq!(before.len(), 6);

    // Lower the cap to 4 (what `config_set` would do). The bound drops,
    // but no code rewrites the stored ensemble.
    let caps = crate::domain::graphs::MemberCaps {
        parallel: 4,
        sequential: 16,
    };
    assert_eq!(
        crate::domain::graphs::member_bounds(EnsembleKind::Parallel, caps).1,
        4
    );
    let after = db.list_ensemble_members("ens-cap").unwrap();
    assert_eq!(
        after.len(),
        6,
        "lowering the cap must not edit an existing ensemble"
    );
    let before_ids: Vec<_> = before.iter().map(|m| m.node_id.clone()).collect();
    let after_ids: Vec<_> = after.iter().map(|m| m.node_id.clone()).collect();
    assert_eq!(after_ids, before_ids);
}

#[test]
fn insert_ensemble_unit_creates_join_and_member_nodes_with_correct_wiring() {
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
    let member_ids = ["m1", "m2", "m3"];
    let member_nodes: Vec<GraphNode> = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| GraphNode {
            id: id.to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "openrouter",
                "prompt_template": "draft it",
            }),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();
    let join_node = GraphNode {
        id: "join1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "ens1"}),
        position: 5,
        created_at: now,
    };
    let mut edges = Vec::new();
    for id in &member_ids {
        edges.push(GraphEdge {
            id: format!("kickoff->{id}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{id}->join1"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: "join1->arbiter".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "arbiter".to_string(),
        condition: GraphEdgeCondition::Pass,
    });
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
        min_pass: 3,
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
    let members: Vec<EnsembleMember> = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: id.to_string(),
            position: i as i64,
            platform: "openrouter".to_string(),
            model: Some(format!("model-{i}")),
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();

    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    // 1 call -> N+1 new graph nodes (join + 3 members), on top of the 2
    // pre-existing (kickoff, arbiter).
    let all_nodes = db.list_graph_nodes("spec-1").unwrap();
    assert_eq!(all_nodes.len(), 6);
    assert!(db.get_graph_node("join1").unwrap().is_some());
    for id in &member_ids {
        assert!(db.get_graph_node(id).unwrap().is_some());
    }

    let all_edges = db.list_graph_edges("spec-1").unwrap();
    // 2 wiring edges per member (entry + join) + 1 join exit edge.
    assert_eq!(all_edges.len(), member_ids.len() * 2 + 1);

    let details = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(details.members.len(), 3);
    // Deterministic member order (position ASC), matching insertion order.
    assert_eq!(
        details
            .members
            .iter()
            .map(|m| m.node_id.as_str())
            .collect::<Vec<_>>(),
        member_ids
    );
}

#[test]
fn replace_ensemble_members_preserves_all_entry_sources_on_resize() {
    let db = test_db();
    insert_cb52_ensemble(&db);
    let now = Utc::now();
    let mut members = db.list_ensemble_members("ens1").unwrap();
    let mut nodes: Vec<GraphNode> = members
        .iter()
        .map(|member| db.get_graph_node(&member.node_id).unwrap().unwrap())
        .collect();
    for (position, id) in [(2, "m3"), (3, "m4")] {
        members.push(EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: id.to_string(),
            position,
            platform: "openrouter".to_string(),
            model: Some(format!("model-{position}")),
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        });
        nodes.push(GraphNode {
            id: id.to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: format!("member-{}", position + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({
                "platform": "openrouter",
                "prompt_template": "draft it",
            }),
            position: 9 + position,
            created_at: now,
        });
    }

    db.replace_ensemble_members("ens1", &members, &nodes)
        .unwrap();
    let four_ids: std::collections::HashSet<_> = members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let entry_edges = db
        .list_graph_edges("spec-1")
        .unwrap()
        .into_iter()
        .filter(|edge| {
            ["kickoff", "alt_entry"].contains(&edge.from_node.as_str())
                && four_ids.contains(edge.to_node.as_str())
        })
        .count();
    assert_eq!(entry_edges, 8);

    let shrunk_members = members[..3].to_vec();
    let shrunk_nodes = nodes[..3].to_vec();
    db.replace_ensemble_members("ens1", &shrunk_members, &shrunk_nodes)
        .unwrap();
    let three_ids: std::collections::HashSet<_> = shrunk_members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let entry_edges = db
        .list_graph_edges("spec-1")
        .unwrap()
        .into_iter()
        .filter(|edge| {
            ["kickoff", "alt_entry"].contains(&edge.from_node.as_str())
                && three_ids.contains(edge.to_node.as_str())
        })
        .count();
    assert_eq!(entry_edges, 6);
    assert!(db.get_graph_node("m4").unwrap().is_none());
}

#[test]
fn replace_ensemble_members_rolls_back_on_insert_failure() {
    let db = test_db();
    insert_cb52_ensemble(&db);
    let before_members = db.list_ensemble_members("ens1").unwrap();
    let before_edges = db.list_graph_edges("spec-1").unwrap();
    let now = Utc::now();
    let mut members = before_members.clone();
    members.push(EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "kickoff".to_string(),
        position: 2,
        platform: "openrouter".to_string(),
        model: Some("bad-member".to_string()),
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    });
    let mut nodes: Vec<GraphNode> = before_members
        .iter()
        .map(|member| db.get_graph_node(&member.node_id).unwrap().unwrap())
        .collect();
    nodes.push(GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "duplicate".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({}),
        position: 99,
        created_at: now,
    });

    assert!(db
        .replace_ensemble_members("ens1", &members, &nodes)
        .is_err());
    let member_ids = |members: Vec<EnsembleMember>| {
        members
            .into_iter()
            .map(|member| (member.node_id, member.position))
            .collect::<Vec<_>>()
    };
    let edge_ids = |edges: Vec<GraphEdge>| {
        edges
            .into_iter()
            .map(|edge| (edge.id, edge.from_node, edge.to_node, edge.condition))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        member_ids(db.list_ensemble_members("ens1").unwrap()),
        member_ids(before_members)
    );
    assert_eq!(
        edge_ids(db.list_graph_edges("spec-1").unwrap()),
        edge_ids(before_edges)
    );
}

#[test]
fn replace_ensemble_members_grows_entry_fan_out_for_new_members() {
    let db = test_db();
    insert_cb52_ensemble(&db);
    let now = Utc::now();
    let mut members = db.list_ensemble_members("ens1").unwrap();
    let mut nodes: Vec<GraphNode> = members
        .iter()
        .map(|member| db.get_graph_node(&member.node_id).unwrap().unwrap())
        .collect();
    members.push(EnsembleMember {
        ensemble_id: "ens1".to_string(),
        node_id: "m3".to_string(),
        position: 2,
        platform: "openrouter".to_string(),
        model: Some("model-2".to_string()),
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    });
    nodes.push(GraphNode {
        id: "m3".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "member-3".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "openrouter",
            "prompt_template": "draft it",
        }),
        position: 11,
        created_at: now,
    });

    db.replace_ensemble_members("ens1", &members, &nodes)
        .unwrap();

    assert!(
        db.get_graph_node("m3").unwrap().is_some(),
        "the new member's graph_nodes row must be inserted"
    );
    let edges = db.list_graph_edges("spec-1").unwrap();
    let edge_exists =
        |from: &str, to: &str| edges.iter().any(|e| e.from_node == from && e.to_node == to);
    assert!(
        edge_exists("kickoff", "m3"),
        "kickoff must fan out to the new member"
    );
    assert!(
        edge_exists("alt_entry", "m3"),
        "alt_entry must fan out to the new member"
    );
    assert!(
        edge_exists("m3", "join1"),
        "the new member must wire to the join node"
    );

    let grown: Vec<(String, i64)> = db
        .list_ensemble_members("ens1")
        .unwrap()
        .into_iter()
        .map(|member| (member.node_id, member.position))
        .collect();
    assert_eq!(
        grown,
        vec![
            ("m1".to_string(), 0),
            ("m2".to_string(), 1),
            ("m3".to_string(), 2),
        ]
    );
    assert!(edge_exists("m1", "join1"), "existing member wiring is kept");
    assert!(edge_exists("m2", "join1"), "existing member wiring is kept");
    assert!(edge_exists("join1", "arbiter"), "join exit routing is kept");
    assert!(
        edge_exists("join1", "fallback"),
        "join exit routing is kept"
    );
}

#[test]
fn add_ensemble_entry_source_repairs_partial_source_idempotently() {
    let db = test_db();
    insert_cb52_ensemble(&db);
    let missing = db
        .list_graph_edges("spec-1")
        .unwrap()
        .into_iter()
        .find(|edge| edge.from_node == "alt_entry" && edge.to_node == "m2")
        .unwrap();
    assert!(db.delete_graph_edge(&missing.id).unwrap());

    db.add_ensemble_entry_source("ens1", "alt_entry", &GraphEdgeCondition::Always)
        .unwrap();
    db.add_ensemble_entry_source("ens1", "alt_entry", &GraphEdgeCondition::Pass)
        .unwrap();
    let repaired = db
        .list_graph_edges("spec-1")
        .unwrap()
        .into_iter()
        .filter(|edge| edge.from_node == "alt_entry")
        .collect::<Vec<_>>();
    assert_eq!(repaired.len(), 2);
    assert!(repaired.iter().any(|edge| edge.to_node == "m2"));
}

/// CB52 FR1 (DB half): every reference role is reported — primary entry,
/// extra entry source, pass exit, fail exit — and nothing else is.
/// A test that only covered one role would still pass with the other
/// three roles' checks removed.
#[test]
fn ensembles_referencing_node_reports_all_four_roles() {
    let db = test_db();
    insert_cb52_ensemble(&db);

    let roles_for = |node_id: &str| {
        let mut roles: Vec<String> = db
            .ensembles_referencing_node(node_id)
            .unwrap()
            .into_iter()
            .map(|(_, role)| role)
            .collect();
        roles.sort();
        roles
    };
    assert_eq!(roles_for("kickoff"), vec!["entry_from_node"]);
    assert_eq!(roles_for("alt_entry"), vec!["entry_source"]);
    assert_eq!(roles_for("arbiter"), vec!["on_pass_to"]);
    assert_eq!(roles_for("fallback"), vec!["on_fail_to"]);
    assert!(roles_for("unrelated").is_empty());
    // Owned nodes are not "references": the member/join guard owns them.
    assert!(roles_for("m1").is_empty());
    assert!(roles_for("join1").is_empty());

    // Each hit names the ensemble it belongs to.
    let hits = db.ensembles_referencing_node("kickoff").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].0.ensemble.id, "ens1");
    assert_eq!(hits[0].0.ensemble.name, "Proposers");
}

/// CB52 FR4 (DB half): only a join node with no ensemble row is listed —
/// a healthy ensemble's own join never is — and the per-graph/per-spec
/// scopes agree with the global one.
#[test]
fn orphan_join_detection_lists_only_unowned_joins() {
    let db = test_db();
    insert_cb52_ensemble(&db);
    assert!(db.list_all_orphan_join_nodes().unwrap().is_empty());

    db.insert_graph_node(&GraphNode {
        id: "orphan-join".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "orphaned quorum".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({"ensemble_id": "gone"}),
        position: 9,
        created_at: Utc::now(),
    })
    .unwrap();

    let all = db.list_all_orphan_join_nodes().unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "orphan-join");
    assert_eq!(all[0].name, "orphaned quorum");

    let for_spec = db.list_orphan_join_nodes_for_spec("spec-1").unwrap();
    assert_eq!(for_spec.len(), 1);
    assert_eq!(for_spec[0].id, "orphan-join");

    // Wrong scopes see nothing.
    assert!(db
        .list_orphan_join_nodes_for_spec("no-such-spec")
        .unwrap()
        .is_empty());
    assert!(db
        .list_orphan_join_nodes_for_graph("no-such-graph")
        .unwrap()
        .is_empty());
}

/// `graph_update_ensemble`'s prompt-propagation contract: updating the
/// ensemble's shared prompt must land on the `ensembles` row (the
/// source of truth `graph_get` reads) *and* on every member node's own
/// `config.prompt_template` — exercising the exact two-step sequence
/// (`update_graph_node_details` per member, then `update_ensemble_prompt`)
/// the handler performs.
#[test]
fn update_ensemble_prompt_propagates_to_every_member_config() {
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
            config: serde_json::json!({"platform": "openrouter", "prompt_template": "old prompt"}),
            position: 2,
            created_at: now,
        },
        GraphNode {
            id: "m2".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-2".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "old prompt"}),
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
        prompt_template: "old prompt".to_string(),
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
            platform: "openrouter".to_string(),
            model: None,
            prompt_override: None,
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

    // Simulates `graph_update_ensemble`'s propagate-without-resize path:
    // rewrite every member's config with the new prompt (preserving its
    // own platform/model), then update the ensemble row itself.
    for member in &members {
        let config = serde_json::json!({
            "platform": member.platform,
            "prompt_template": "new prompt",
        });
        db.update_graph_node_details(&member.node_id, None, None, Some(&config), None)
            .unwrap();
    }
    db.update_ensemble_prompt("ens1", "new prompt").unwrap();

    assert_eq!(
        db.get_ensemble("ens1").unwrap().unwrap().prompt_template,
        "new prompt"
    );
    for member in &members {
        let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
        assert_eq!(
            node.config.get("prompt_template").and_then(|v| v.as_str()),
            Some("new prompt"),
            "member '{}' must carry the propagated prompt",
            member.node_id
        );
    }
}

/// `min_pass`/`straggler_timeout_minutes`/`timeout_minutes` are each
/// independently optional in an update: passing `None` for a field must
/// leave it unchanged, and `straggler_timeout_minutes` specifically
/// needs `Some(None)` (as opposed to bare `None`) to actually clear an
/// override back to "defer to timeout_minutes" — this exercises both.
#[test]
fn update_ensemble_join_config_leaves_omitted_fields_unchanged() {
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
        config: serde_json::json!({"platform": "openrouter"}),
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
        straggler_timeout_minutes: Some(15),
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
        platform: "openrouter".to_string(),
        model: None,
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    // Only min_pass changes; straggler_timeout_minutes/timeout_minutes omitted.
    db.update_ensemble_join_config("ens1", Some(1), None, None, None, None, None, None)
        .unwrap();
    let after = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(after.min_pass, 1);
    assert_eq!(after.straggler_timeout_minutes, Some(15));
    assert_eq!(after.timeout_minutes, 30);

    // Explicitly clear straggler_timeout_minutes back to "defer to timeout_minutes".
    db.update_ensemble_join_config("ens1", None, Some(None), None, None, None, None, None)
        .unwrap();
    let cleared = db.get_ensemble("ens1").unwrap().unwrap();
    assert_eq!(cleared.straggler_timeout_minutes, None);
    assert_eq!(
        cleared.effective_straggler_timeout_minutes(),
        cleared.timeout_minutes
    );
}

/// Removing a member cascades away its node, its `ensemble_members` row,
/// and both of its wiring edges (entry + join) — the rest of the
/// ensemble is left intact.
#[test]
fn remove_ensemble_member_cascades_node_and_edges() {
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
    let member_ids = ["m1", "m2"];
    let member_nodes: Vec<GraphNode> = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| GraphNode {
            id: id.to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: format!("member-{}", i + 1),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "openrouter"}),
            position: 2 + i as i64,
            created_at: now,
        })
        .collect();
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
    let mut edges = Vec::new();
    for id in &member_ids {
        edges.push(GraphEdge {
            id: format!("kickoff->{id}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: id.to_string(),
            condition: GraphEdgeCondition::Always,
        });
        edges.push(GraphEdge {
            id: format!("{id}->join1"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: id.to_string(),
            to_node: "join1".to_string(),
            condition: GraphEdgeCondition::Always,
        });
    }
    edges.push(GraphEdge {
        id: "join1->arbiter".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "arbiter".to_string(),
        condition: GraphEdgeCondition::Pass,
    });
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
    let members: Vec<EnsembleMember> = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: id.to_string(),
            position: i as i64,
            platform: "openrouter".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    assert!(db.remove_ensemble_member("m2").unwrap());

    assert!(db.get_graph_node("m2").unwrap().is_none());
    let remaining_edges = db.list_graph_edges("spec-1").unwrap();
    assert!(!remaining_edges
        .iter()
        .any(|edge| edge.from_node == "m2" || edge.to_node == "m2"));
    let details = db.get_ensemble_details("ens1").unwrap().unwrap();
    assert_eq!(
        details
            .members
            .iter()
            .map(|m| m.node_id.as_str())
            .collect::<Vec<_>>(),
        vec!["m1"]
    );
    // The surviving member and the join itself are unaffected.
    assert!(db.get_graph_node("m1").unwrap().is_some());
    assert!(db.get_graph_node("join1").unwrap().is_some());
}

#[test]
fn get_ensemble_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let result = db.get_ensemble("nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn list_ensemble_members_empty() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let members = db.list_ensemble_members("nonexistent").unwrap();
    assert!(members.is_empty());
}

#[test]
fn get_ensemble_details_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let result = db.get_ensemble_details("nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn list_ensembles_for_spec_empty() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let ensembles = db.list_ensembles_for_spec("nonexistent").unwrap();
    assert!(ensembles.is_empty());
}

#[test]
fn list_ensembles_for_graph_empty() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let ensembles = db.list_ensembles_for_graph("nonexistent").unwrap();
    assert!(ensembles.is_empty());
}

#[test]
fn get_ensemble_by_member_node_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let result = db.get_ensemble_by_member_node("nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn get_ensemble_by_join_node_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let result = db.get_ensemble_by_join_node("nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn update_ensemble_prompt_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let updated = db
        .update_ensemble_prompt("nonexistent", "new prompt")
        .unwrap();
    assert!(!updated);
}

#[test]
fn remove_ensemble_member_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let removed = db.remove_ensemble_member("nonexistent").unwrap();
    assert!(!removed);
}

#[test]
fn get_ensemble_blueprint_by_name_not_found() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let result = db.get_ensemble_blueprint_by_name("nonexistent").unwrap();
    assert!(result.is_none());
}

/// An installation whose `ensemble_blueprints` row still carries the old
/// hardcoded free-tier model identities must have them reconciled away
/// (to `None`) on the next startup reseed, and the reseed must be
/// idempotent.
#[test]
fn seed_builtin_ensemble_blueprints_migrates_hardcoded_model_identities() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    db.delete_ensemble_blueprint_by_name("ensemble-proposers")
        .unwrap();
    db.insert_ensemble_blueprint(&EnsembleBlueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "ensemble-proposers".to_string(),
        prompt_template: "Draft it".to_string(),
        members: vec![
            (
                "openrouter".to_string(),
                Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                None,
                None,
                None,
                None,
                None,
            ),
            (
                "openrouter".to_string(),
                Some("qwen/qwen3-coder:free".to_string()),
                None,
                None,
                None,
                None,
                None,
            ),
            (
                "openrouter".to_string(),
                Some("meta-llama/llama-3.3-70b-instruct:free".to_string()),
                None,
                None,
                None,
                None,
                None,
            ),
        ],
        min_pass: None,
        quorum_grace_minutes: None,
        builtin: true,
        created_at: Utc::now(),
    })
    .unwrap();

    db.seed_builtin_ensemble_blueprints().unwrap();

    let migrated = db
        .get_ensemble_blueprint_by_name("ensemble-proposers")
        .unwrap()
        .unwrap();
    assert_eq!(migrated.members.len(), 3);
    for (platform, model, _, _, _, _, _) in &migrated.members {
        assert_eq!(platform, "openrouter");
        assert!(model.is_none(), "model identity must be reconciled away");
    }

    // Idempotent: a second reseed changes nothing further.
    db.seed_builtin_ensemble_blueprints().unwrap();
    let after_second = db
        .get_ensemble_blueprint_by_name("ensemble-proposers")
        .unwrap()
        .unwrap();
    assert_eq!(after_second.members, migrated.members);
}

/// A custom ensemble blueprint that claims the builtin's name must never
/// be overwritten or deleted by the migration.
#[test]
fn seed_builtin_ensemble_blueprints_never_touches_a_custom_blueprint_with_a_builtin_name() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.delete_ensemble_blueprint_by_name("ensemble-proposers")
        .unwrap();

    let custom = EnsembleBlueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "ensemble-proposers".to_string(),
        prompt_template: "my own take".to_string(),
        members: vec![("claude".to_string(), None, None, None, None, None, None)],
        min_pass: Some(1),
        quorum_grace_minutes: None,
        builtin: false,
        created_at: Utc::now(),
    };
    db.insert_ensemble_blueprint(&custom).unwrap();

    db.seed_builtin_ensemble_blueprints().unwrap();

    let fetched = db
        .get_ensemble_blueprint_by_name("ensemble-proposers")
        .unwrap()
        .unwrap();
    assert!(
        !fetched.builtin,
        "custom ensemble blueprint must stay custom"
    );
    assert_eq!(fetched.prompt_template, "my own take");
}
