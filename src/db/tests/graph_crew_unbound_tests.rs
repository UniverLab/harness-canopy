//! Tests for `Database::graph_crew` with `unbound: true`.

use super::*;
use crate::db::graph_crew::GraphCrewDbParams;
use crate::domain::crews::CrewMember;
use crate::domain::graphs::{
    ArchiveGraphOutcome, CrewRowKind, Ensemble, EnsembleKind, EnsembleMember, GraphCompletionHook,
    GraphHookEvent,
};
use std::collections::BTreeMap;

fn unbound_agent_node(
    graph_id: &str,
    id: &str,
    name: &str,
    position: i64,
    config: serde_json::Value,
) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position,
        created_at: Utc::now(),
    }
}

fn unbound_static_node(graph_id: &str, id: &str, name: &str, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: name.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({ "command": "true", "success_condition": "exit_code_0" }),
        position,
        created_at: Utc::now(),
    }
}

fn unbound_edge(graph_id: &str, from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
    GraphEdge {
        id: format!("{graph_id}:{from}->{to}"),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    }
}

fn unbound_member(
    ensemble_id: &str,
    node_id: &str,
    position: i64,
    platform: &str,
    model: Option<&str>,
) -> EnsembleMember {
    EnsembleMember {
        ensemble_id: ensemble_id.to_string(),
        node_id: node_id.to_string(),
        position,
        platform: platform.to_string(),
        model: model.map(str::to_string),
        prompt_override: None,
        timeout_minutes: None,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
    }
}

#[derive(Clone, Copy)]
struct UnboundEnsembleSpec<'a> {
    graph_id: &'a str,
    ensemble_id: &'a str,
    name: &'a str,
    prefix: &'a str,
    base_pos: i64,
    crew: Option<(&'a str, &'a str, i64)>,
    m1: (&'a str, &'a str),
    m2: (&'a str, &'a str),
}

fn insert_unbound_ensemble(db: &Database, spec: UnboundEnsembleSpec<'_>) {
    let UnboundEnsembleSpec {
        graph_id,
        ensemble_id,
        name,
        prefix,
        base_pos,
        crew,
        m1,
        m2,
    } = spec;
    let entry_id = format!("{prefix}-entry");
    let exit_id = format!("{prefix}-exit");
    let join_id = format!("{prefix}-join");
    let m1_id = format!("{prefix}-m1");
    let m2_id = format!("{prefix}-m2");

    db.insert_graph_node(&unbound_static_node(graph_id, &entry_id, "Entry", base_pos))
        .unwrap();
    db.insert_graph_node(&unbound_static_node(
        graph_id,
        &exit_id,
        "Exit",
        base_pos + 4,
    ))
    .unwrap();

    let ensemble = Ensemble {
        mutex: Vec::new(),
        id: ensemble_id.to_string(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: name.to_string(),
        prompt_template: "review".to_string(),
        join_node_id: join_id.clone(),
        entry_from_node: entry_id.clone(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 2,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: exit_id.clone(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        commit_rights: false,
        read_only: false,
        created_at: Utc::now(),
        crew_id: crew.map(|(id, _, _)| id.to_string()),
        crew_name: crew.map(|(_, cname, _)| cname.to_string()),
        crew_version_applied: crew.map(|(_, _, ver)| ver),
    };
    let members = vec![
        unbound_member(ensemble_id, &m1_id, 0, m1.0, Some(m1.1)),
        unbound_member(ensemble_id, &m2_id, 1, m2.0, Some(m2.1)),
    ];
    let member_nodes = vec![
        unbound_agent_node(
            graph_id,
            &m1_id,
            "M1",
            base_pos + 1,
            serde_json::json!({ "platform": m1.0, "model": m1.1 }),
        ),
        unbound_agent_node(
            graph_id,
            &m2_id,
            "M2",
            base_pos + 2,
            serde_json::json!({ "platform": m2.0, "model": m2.1 }),
        ),
    ];
    let join = GraphNode {
        id: join_id.clone(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: "Join".to_string(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": ensemble_id }),
        position: base_pos + 3,
        created_at: Utc::now(),
    };
    let edges = vec![
        unbound_edge(graph_id, &entry_id, &m1_id, GraphEdgeCondition::Always),
        unbound_edge(graph_id, &entry_id, &m2_id, GraphEdgeCondition::Always),
        unbound_edge(graph_id, &m1_id, &join_id, GraphEdgeCondition::Always),
        unbound_edge(graph_id, &m2_id, &join_id, GraphEdgeCondition::Always),
        unbound_edge(graph_id, &join_id, &exit_id, GraphEdgeCondition::Pass),
    ];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join, &edges)
        .unwrap();
}

fn unbound_fixture() -> (Database, String, String, String) {
    let db = test_db();
    let crew = db
        .crew_create(
            "eager",
            None,
            &[CrewMember {
                position: 0,
                platform: "claude".to_string(),
                model: Some("opus".to_string()),
                model_ref: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }],
        )
        .unwrap();

    let mut mixed = sample_graph("graph-mixed");
    mixed.name = "Mixed".to_string();
    let mut hooks = BTreeMap::new();
    hooks.insert(
        GraphHookEvent::OnCompleted,
        vec![GraphCompletionHook {
            platform: Some("claude".to_string()),
            crew: None,
            spec_ids: None,
            model: Some("haiku".to_string()),
            effort: None,
            prompt: Some("summarize".to_string()),
            command: None,
            target_session_id: None,
            target_session_name: None,
            timeout_minutes: Some(5),
            target_graph_id: None,
            queue_id: None,
            workdir_override: None,
            idea: None,
        }],
    );
    mixed.hooks = hooks;
    db.insert_graph(&mixed).unwrap();
    db.insert_graph_node(&unbound_agent_node(
        &mixed.id,
        "node-bound",
        "BoundNode",
        10,
        serde_json::json!({ "crew": "eager", "prompt_template": "go" }),
    ))
    .unwrap();
    db.insert_graph_node(&unbound_agent_node(
        &mixed.id,
        "node-literal",
        "LiteralNode",
        11,
        serde_json::json!({ "platform": "claude", "model": "opus", "prompt_template": "go" }),
    ))
    .unwrap();
    insert_unbound_ensemble(
        &db,
        UnboundEnsembleSpec {
            graph_id: &mixed.id,
            ensemble_id: "ens-literal",
            name: "LiteralEns",
            prefix: "lit",
            base_pos: 20,
            crew: None,
            m1: ("claude", "sonnet"),
            m2: ("codex", "gpt-5"),
        },
    );
    insert_unbound_ensemble(
        &db,
        UnboundEnsembleSpec {
            graph_id: &mixed.id,
            ensemble_id: "ens-bound",
            name: "BoundEns",
            prefix: "bnd",
            base_pos: 30,
            crew: Some((&crew.id, &crew.name, crew.version)),
            m1: ("claude", "opus"),
            m2: ("claude", "opus"),
        },
    );

    let mut bound = sample_graph("graph-bound");
    bound.name = "FullyBound".to_string();
    db.insert_graph(&bound).unwrap();
    db.insert_graph_node(&unbound_agent_node(
        &bound.id,
        "node-only-bound",
        "OnlyBound",
        1,
        serde_json::json!({ "crew": "eager", "prompt_template": "go" }),
    ))
    .unwrap();

    let mut archived = sample_graph("graph-archived");
    archived.name = "ArchivedUnbound".to_string();
    db.insert_graph(&archived).unwrap();
    db.insert_graph_node(&unbound_agent_node(
        &archived.id,
        "node-arch-lit",
        "ArchLiteral",
        1,
        serde_json::json!({ "platform": "codex", "model": "o3", "prompt_template": "go" }),
    ))
    .unwrap();
    assert_eq!(
        db.archive_graph(&archived.id).unwrap(),
        ArchiveGraphOutcome::Archived
    );

    (db, mixed.id, bound.id, archived.id)
}

#[test]
fn graph_crew_unbound_filters_literal_rows_and_combines_with_other_filters() {
    let (db, mixed_id, bound_id, archived_id) = unbound_fixture();

    let groups = db
        .graph_crew(GraphCrewDbParams {
            unbound: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].id, mixed_id);
    assert_eq!(groups[0].rows.len(), 4);
    assert!(groups[0].rows.iter().all(|r| r.crew.is_none()));
    assert_eq!(
        groups[0]
            .rows
            .iter()
            .filter(|r| r.kind == CrewRowKind::Node)
            .count(),
        1
    );
    assert_eq!(
        groups[0]
            .rows
            .iter()
            .filter(|r| r.kind == CrewRowKind::EnsembleMember)
            .count(),
        2
    );
    assert_eq!(
        groups[0]
            .rows
            .iter()
            .filter(|r| r.kind == CrewRowKind::Hook)
            .count(),
        1
    );

    let with_archived = db
        .graph_crew(GraphCrewDbParams {
            unbound: true,
            include_archived: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(with_archived.len(), 2);
    assert!(with_archived.iter().any(|g| g.id == archived_id));

    let by_platform = db
        .graph_crew(GraphCrewDbParams {
            unbound: true,
            platform: Some("claude"),
            include_archived: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_platform.len(), 1);
    assert_eq!(by_platform[0].rows.len(), 3);
    assert!(by_platform[0].rows.iter().all(|r| r.platform == "claude"));

    let by_model = db
        .graph_crew(GraphCrewDbParams {
            unbound: true,
            model: Some("sonnet"),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(by_model.len(), 1);
    assert_eq!(by_model[0].rows.len(), 1);
    assert_eq!(by_model[0].rows[0].model, "sonnet");

    let only_bound_graph = db
        .graph_crew(GraphCrewDbParams {
            unbound: true,
            graph_id: Some(&bound_id),
            ..Default::default()
        })
        .unwrap();
    assert!(only_bound_graph.is_empty());

    let summary = db
        .graph_crew_summary(GraphCrewDbParams {
            unbound: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(summary.len(), 4);
}
