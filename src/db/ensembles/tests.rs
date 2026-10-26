//! `tests` tests (split from `src/db/ensembles/mod.rs`, pure move).
use super::*;
use crate::domain::graphs::{GraphNodeKind, GraphSpec, GraphSpecStatus};
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Database {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Database::new(&path).expect("create test db")
}

fn insert_spec(db: &Database, id: &str) {
    db.insert_graph_spec(&GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: Some("Do the thing".to_string()),
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
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    })
    .unwrap();
}

/// CB52 fixture: a spec-scoped ensemble with two entry sources
/// (`kickoff` primary, `alt_entry` extra), `on_pass_to = arbiter`,
/// `on_fail_to = fallback`, plus an unrelated node.
fn insert_cb52_ensemble(db: &Database) {
    insert_spec(db, "spec-1");
    for (id, kind, position) in [
        ("kickoff", GraphNodeKind::Check, 1),
        ("alt_entry", GraphNodeKind::Check, 2),
        ("arbiter", GraphNodeKind::Agent, 6),
        ("fallback", GraphNodeKind::Agent, 7),
        ("unrelated", GraphNodeKind::Agent, 8),
    ] {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: id.to_string(),
            kind,
            config: serde_json::json!({}),
            position,
            created_at: Utc::now(),
        })
        .unwrap();
    }
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
            config: serde_json::json!({
                "platform": "openrouter",
                "prompt_template": "draft it",
            }),
            position: 3 + i as i64,
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
    for entry in ["kickoff", "alt_entry"] {
        for id in &member_ids {
            edges.push(GraphEdge {
                id: format!("{entry}->{id}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: entry.to_string(),
                to_node: id.to_string(),
                condition: GraphEdgeCondition::Always,
            });
        }
    }
    for id in &member_ids {
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
    edges.push(GraphEdge {
        id: "join1->fallback".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: "join1".to_string(),
        to_node: "fallback".to_string(),
        condition: GraphEdgeCondition::Fail,
    });
    let ensemble = Ensemble {
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
        on_fail_to: Some("fallback".to_string()),
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
}

mod part01_tests;
mod part02_tests;
