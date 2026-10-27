//! `tests` tests (split from `src/domain/graph_transfer/mod.rs`, pure move).
use super::*;
use chrono::Utc;

fn make_graph(name: &str) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "graph-1".to_string(),
        name: name.to_string(),
        description: Some("A test graph".to_string()),
        workdir: "/tmp/project".to_string(),
        status: crate::domain::graphs::GraphStatus::Draft,
        trigger: None,
        created_at: Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    }
}

fn make_node(id: &str, name: &str, kind: GraphNodeKind, config: Value, position: i64) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: None,
        graph_id: Some("graph-1".to_string()),
        name: name.to_string(),
        kind,
        config,
        position,
        created_at: Utc::now(),
    }
}

fn make_edge(id: &str, from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
    GraphEdge {
        id: id.to_string(),
        spec_id: None,
        graph_id: Some("graph-1".to_string()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    }
}

/// A simple 3-node chain (implementer -> gate -> committer), no
/// ensembles: the base case every other test builds on.
fn simple_graph() -> (Graph, Vec<GraphNode>, Vec<GraphEdge>) {
    let lp = make_graph("simple-graph");
    let nodes = vec![
        make_node(
            "n1",
            "implementer",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "model": "opus", "prompt_template": "implement it"}),
            1,
        ),
        make_node(
            "n2",
            "gate",
            GraphNodeKind::Check,
            serde_json::json!({"command": "cargo test"}),
            2,
        ),
        make_node(
            "n3",
            "committer",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_template": "commit it"}),
            3,
        ),
    ];
    let edges = vec![
        make_edge("e1", "n1", "n2", GraphEdgeCondition::Always),
        make_edge("e2", "n2", "n3", GraphEdgeCondition::Always),
    ];
    (lp, nodes, edges)
}

fn make_ensemble_details(
    ensemble_id: &str,
    join_id: &str,
    entry_from: &str,
    on_pass_to: &str,
    member_ids: &[&str],
) -> EnsembleDetails {
    let ensemble = Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: ensemble_id.to_string(),
        spec_id: None,
        graph_id: Some("graph-1".to_string()),
        name: "Proposers".to_string(),
        prompt_template: "draft it".to_string(),
        join_node_id: join_id.to_string(),
        entry_from_node: entry_from.to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: member_ids.len() as i64,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: on_pass_to.to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: Utc::now(),
        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = member_ids
        .iter()
        .enumerate()
        .map(|(i, id)| EnsembleMember {
            ensemble_id: ensemble_id.to_string(),
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
    EnsembleDetails { ensemble, members }
}

/// CM36: a minimal valid document whose single ensemble carries 12
/// members, so the per-kind ceiling can be exercised at import.
fn make_twelve_member_doc(kind: Option<&str>) -> GraphExportDocument {
    GraphExportDocument {
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
            kind: kind.map(str::to_string),
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
            members: (0..12)
                .map(|_| GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                })
                .collect(),

            crew: None,
        }],
        infra_node: None,

        hooks: Default::default(),
    }
}

fn import_kind_fixture(kind: Option<&str>) -> GraphImportPlan {
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
            name: "team".to_string(),
            kind: kind.map(str::to_string),
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
    build_import_plan(&doc, "new-graph").unwrap()
}

mod part01_tests;
mod part02_tests;
mod part03_tests;
