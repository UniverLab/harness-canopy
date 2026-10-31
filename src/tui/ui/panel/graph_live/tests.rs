//! `tests` tests (split from `src/tui/ui/panel/graph_live.rs`, pure move).
use super::*;
use crate::db::Database;
use crate::domain::graphs::{
    GraphEdge, GraphNode, GraphNodeKind, GraphSpecStatus, GraphStatus as DomainGraphStatus,
};
use crate::tui::app::types::App;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::json;
use std::sync::Arc;

fn render_to_text(width: u16, height: u16, draw: impl FnOnce(&mut Frame, Rect)) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| {
            let area = frame.area();
            draw(frame, area);
        })
        .unwrap();

    let buffer = terminal.backend().buffer().clone();
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn team_nodes() -> Vec<GraphNode> {
    let kinds = [
        ("Implement", GraphNodeKind::Agent),
        ("Cargo gates", GraphNodeKind::Check),
        ("Review + commit", GraphNodeKind::Agent),
        ("Check committed", GraphNodeKind::Check),
        ("Resilience", GraphNodeKind::Agent),
    ];
    kinds
        .iter()
        .enumerate()
        .map(|(i, (name, kind))| GraphNode {
            id: format!("n{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: *kind,
            config: json!({}),
            position: i as i64,
            created_at: Utc::now(),
        })
        .collect()
}

fn team_edges() -> Vec<GraphEdge> {
    // Mirrors the real 5-node team graph: cycles back to Implement (n0)
    // on failure at any later stage, and Resilience (n4) graphs back to
    // Implement on its own pass.
    vec![
        ("n0", "n1", GraphEdgeCondition::Pass),
        ("n0", "n4", GraphEdgeCondition::Fail),
        ("n4", "n0", GraphEdgeCondition::Pass),
        ("n1", "n2", GraphEdgeCondition::Pass),
        ("n1", "n0", GraphEdgeCondition::Fail),
        ("n2", "n3", GraphEdgeCondition::Pass),
        ("n2", "n0", GraphEdgeCondition::Fail),
        ("n3", "n2", GraphEdgeCondition::Fail),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, (from, to, condition))| GraphEdge {
        id: format!("e{i}"),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    })
    .collect()
}

fn running_state() -> GraphLiveState {
    GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "canopy-ux-notifications".to_string(),
        graph_status: DomainGraphStatus::Running,
        workdir: "/home/user/Projects/harness-canopy".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        pause_waiting_for: Vec::new(),
        spec_queue: vec![
            SpecQueueEntry {
                spec_id: "s1".to_string(),
                spec_name: "B1 fix".to_string(),
                status: GraphSpecStatus::Completed,
                failure_reason: None,
                spec_code: None,
                spec_type: None,
                spec_scope: None,
            },
            SpecQueueEntry {
                spec_id: "s2".to_string(),
                spec_name: "U1b live view".to_string(),
                status: GraphSpecStatus::Running,
                failure_reason: None,
                spec_code: None,
                spec_type: None,
                spec_scope: None,
            },
            SpecQueueEntry {
                spec_id: "s3".to_string(),
                spec_name: "T1 theme".to_string(),
                status: GraphSpecStatus::Pending,
                failure_reason: None,
                spec_code: None,
                spec_type: None,
                spec_scope: None,
            },
        ],
        done_count: 1,
        total_count: 3,
        current_spec_id: Some("s2".to_string()),
        effective_nodes: team_nodes(),
        effective_edges: team_edges(),
        ensembles: Vec::new(),
        router_taken_routes: HashMap::new(),
        current_node_id: Some("n0".to_string()),
        current_node_status: Some(GraphRunStatus::Running),
        current_node_started_at: Some(Utc::now() - chrono::Duration::seconds(75)),
        current_node_iteration: Some(2),
    }
}

fn ensemble_live_fixture() -> EnsembleLiveInfo {
    EnsembleLiveInfo {
        ensemble_id: "ens1".to_string(),
        name: "Proposers".to_string(),
        join_node_id: "join1".to_string(),
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        members: vec![
            crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                node_id: "m1".to_string(),
                label: "openrouter/deepseek".to_string(),
                status: Some(GraphRunStatus::Pass),
            },
            crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                node_id: "m2".to_string(),
                label: "openrouter/qwen".to_string(),
                status: Some(GraphRunStatus::Running),
            },
            crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                node_id: "m3".to_string(),
                label: "openrouter/llama".to_string(),
                status: None,
            },
        ],
    }
}

fn ensemble_fixture_with_labels(labels: &[&str]) -> EnsembleLiveInfo {
    EnsembleLiveInfo {
        ensemble_id: "ens1".to_string(),
        name: "Implementer".to_string(),
        join_node_id: "join1".to_string(),
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        members: labels
            .iter()
            .enumerate()
            .map(
                |(i, label)| crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
                    node_id: format!("m{i}"),
                    label: label.to_string(),
                    status: Some(GraphRunStatus::Pass),
                },
            )
            .collect(),
    }
}

fn node_fixture(name: &str, kind: GraphNodeKind) -> GraphNode {
    GraphNode {
        id: "n1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: name.to_string(),
        kind,
        config: json!({}),
        position: 0,
        created_at: Utc::now(),
    }
}

fn test_db_and_dir() -> (Arc<Database>, tempfile::TempDir) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).unwrap());
    let data_dir = tempfile::tempdir().unwrap();
    (db, data_dir)
}

fn seed_running_graph(db: &Database) {
    use crate::domain::graphs::{Graph, GraphNodeRun, GraphSpec};

    let lp = Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "lp1".to_string(),
        name: "team graph".to_string(),
        description: None,
        workdir: "/tmp/test".to_string(),
        status: DomainGraphStatus::Running,
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
    };
    db.insert_graph(&lp).unwrap();

    db.insert_graph_spec(&GraphSpec {
        id: "s1".to_string(),
        graph_id: Some("lp1".to_string()),
        name: "spec one".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Running,
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

    for node in team_nodes() {
        db.insert_graph_node(&GraphNode {
            spec_id: Some("s1".to_string()),
            ..node
        })
        .unwrap();
    }
    for edge in team_edges() {
        db.insert_graph_edge(&GraphEdge {
            spec_id: Some("s1".to_string()),
            ..edge
        })
        .unwrap();
    }

    db.insert_graph_run(&GraphNodeRun {
        id: "run1".to_string(),
        graph_id: "lp1".to_string(),
        spec_id: "s1".to_string(),
        node_id: "n0".to_string(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    })
    .unwrap();
}

fn many_nodes(count: usize) -> Vec<GraphNode> {
    (0..count)
        .map(|i| GraphNode {
            id: format!("m{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: format!("Node {i}"),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position: i as i64,
            created_at: Utc::now(),
        })
        .collect()
}

fn many_edges(count: usize) -> Vec<GraphEdge> {
    (0..count.saturating_sub(1))
        .map(|i| GraphEdge {
            id: format!("me{i}"),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: format!("m{i}"),
            to_node: format!("m{}", i + 1),
            condition: GraphEdgeCondition::Pass,
        })
        .collect()
}

mod part01_tests;
mod part02_tests;
