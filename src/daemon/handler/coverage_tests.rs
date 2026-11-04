//! `coverage_tests` tests (split from `src/daemon/handler.rs`, pure move).
use super::*;
use crate::daemon::params::EnsembleMemberParams;
use crate::db::Database;
use crate::domain::graphs::{
    Ensemble, EnsembleDetails, EnsembleMember, Graph, GraphCompletionHook, GraphCompletionHookRun,
    GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus,
    GraphSpec, GraphSpecStatus, GraphStatus,
};
use crate::domain::models::{Agent, Cli};
use crate::domain::queues::Queue;
use tempfile::tempdir;

fn standalone_spec(id: &str) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: None,
        position: 0,
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
    }
}

fn running_spec(id: &str) -> GraphSpec {
    let mut spec = standalone_spec(id);
    spec.status = GraphSpecStatus::Running;
    spec
}

fn make_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "test prompt".to_string(),
        trigger: None,
        cli: Cli("opencode".to_string()),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/test.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn make_graph(graph_id: &str, status: GraphStatus) -> Graph {
    Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.to_string(),
        name: graph_id.to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    }
}

fn graph_run_row(id: &str, graph_id: &str, spec_id: &str, status: GraphRunStatus) -> GraphNodeRun {
    GraphNodeRun {
        id: id.to_string(),
        graph_id: graph_id.to_string(),
        spec_id: spec_id.to_string(),
        node_id: "node-1".to_string(),
        status,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: (status != GraphRunStatus::Running).then(chrono::Utc::now),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }
}

fn insert_test_graph(db: &Database, id: &str) {
    db.insert_graph(&make_graph(id, GraphStatus::Draft))
        .unwrap();
}

fn insert_test_node(db: &Database, id: &str, spec_id: &str) {
    db.insert_graph_node(&GraphNode {
        id: id.to_string(),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
}

mod part01_tests;
mod part02_tests;
