//! `tests` tests (split from `src/domain/graphs.rs`, pure move).
use super::*;
use super::{
    validate_router_edges_declared, validate_router_route_coverage, validate_router_routes,
    GraphEdge, GraphEdgeCondition, GraphNodeKind, GraphResetOutcome, GraphRunStatus,
    GraphSpecStatus, GraphStatus, RouterRoute, SpecAdminStatusOutcome,
};

fn graph_with_trigger(status: GraphStatus, trigger: Option<super::Trigger>) -> super::Graph {
    super::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "wf".to_string(),
        name: "Graph".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status,
        trigger,
        created_at: chrono::Utc::now(),
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

// ── validate_router_routes ───────────────────────────────────────────

fn two_routes() -> Vec<RouterRoute> {
    vec![
        RouterRoute {
            label: "retry".to_string(),
            description: "Retry the current step.".to_string(),
        },
        RouterRoute {
            label: "escalate".to_string(),
            description: "Hand off to a human.".to_string(),
        },
    ]
}

// ── validate_router_edges_declared ───────────────────────────────────

fn route_edge(id: &str, from_node: &str, label: &str) -> GraphEdge {
    GraphEdge {
        id: id.to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: from_node.to_string(),
        to_node: "target".to_string(),
        condition: GraphEdgeCondition::Route(label.to_string()),
    }
}

mod part01_tests;
mod part02_tests;
