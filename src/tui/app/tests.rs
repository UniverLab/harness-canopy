//! `tests` tests (split from `src/tui/app/mod.rs`, pure move).
use super::{
    adaptive_change_score, adaptive_poll_interval_ms, blend_optional_f32, blend_optional_f64,
    build_resumed_session_args, calculate_log_hash, lerp_f32, lerp_u64, log_contains_error,
    log_contains_spawn, log_contains_success, sample_from, SystemSample,
};
use crate::db::session::InteractiveSession;
use crate::db::Database;
use crate::domain::graphs::{GraphSpecStatus, GraphStatus};
use crate::tui::app::graph_live_state::{GraphLiveState, SpecQueueEntry};
use crate::tui::app::types::{
    AgentEntry, App, AutomationKind, Focus, GraphLiveFocus, ProjectTab, SidebarLayer,
};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

// ── Sidebar: graphs/backlog/history sections ─────────────────────

fn make_project(hash: &str, path: &str) -> crate::domain::project::Project {
    crate::domain::project::Project {
        hash: hash.to_string(),
        path: path.to_string(),
        name: hash.to_string(),
        description: None,
        tags: None,
        indexed_at: None,
        created_at: 0,
    }
}

fn make_graph(
    id: &str,
    name: &str,
    status: crate::domain::graphs::GraphStatus,
) -> crate::domain::graphs::Graph {
    crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: name.to_string(),
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
        require_spec_type: false,
    }
}

fn make_backlog_spec(
    id: &str,
    name: &str,
    workdir: Option<&str>,
) -> crate::domain::graphs::GraphSpec {
    crate::domain::graphs::GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: name.to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: crate::domain::graphs::GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: workdir.map(str::to_string),
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    }
}

fn bg_agent(id: &str) -> crate::domain::models::Agent {
    crate::domain::models::Agent {
        id: id.to_string(),
        prompt: String::new(),
        trigger: None,
        cli: crate::domain::models::Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: format!("/tmp/{id}.log"),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn spec_queue_entry(id: &str, status: GraphSpecStatus) -> SpecQueueEntry {
    SpecQueueEntry {
        spec_id: id.to_string(),
        spec_name: format!("Spec {id}"),
        status,
        failure_reason: None,
        spec_code: None,
        spec_type: None,
        spec_scope: None,
    }
}

fn app_with_spec_queue(ids: &[&str]) -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_live_state = Some(GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "graph".to_string(),
        graph_status: GraphStatus::Running,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        pause_waiting_for: Vec::new(),
        spec_queue: ids
            .iter()
            .map(|id| spec_queue_entry(id, GraphSpecStatus::Pending))
            .collect(),
        done_count: 0,
        total_count: ids.len(),
        current_spec_id: None,
        effective_nodes: Vec::new(),
        effective_edges: Vec::new(),
        ensembles: Vec::new(),
        router_taken_routes: HashMap::new(),
        current_node_id: None,
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    });
    app
}

// ── Edges dialog (retarget/delete an ordinary edge) ───────────────

/// Seeds a `Draft` graph with three plain agent nodes `A -> B -> C`
/// (`pass` edges) — the fixture the `Edges` dialog tests in this
/// section start from. `A` has no incoming edge (the graph's entry
/// point); only `A -> B` is wired, so there's a spare target (`C`) to
/// retarget onto.
fn seed_plain_edge_graph(db: &Database) {
    use crate::domain::graphs::{
        Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpec,
    };

    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "plp1".to_string(),
        name: "plain graph".to_string(),
        description: None,
        workdir: "/tmp/plain-edge-test".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "ps1".to_string(),
        graph_id: Some("plp1".to_string()),
        name: "spec one".to_string(),
        description: None,
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
    for (id, name, position) in [("node_a", "A", 0), ("node_b", "B", 1), ("node_c", "C", 2)] {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: Some("ps1".to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }
    db.insert_graph_edge(&GraphEdge {
        id: "e_ab".to_string(),
        spec_id: Some("ps1".to_string()),
        graph_id: None,
        from_node: "node_a".to_string(),
        to_node: "node_b".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
}

fn app_on_plain_node(db: &Arc<Database>, data_dir: &std::path::Path, node_index: usize) -> App {
    let mut app = App::new(
        Arc::clone(db),
        data_dir,
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.refresh_graphs().expect("refresh graphs");
    app.graph_selected_spec = 0;
    app.graph_selected_node = node_index;
    app
}

// ── Router routes dialog (open/pre-fill/save) ────────────────────

/// Seeds a `Draft` graph with one spec containing a 4-route router node
/// ("billing"/"technical"/"sales"/"escalation", fallback "escalation")
/// already wired to `billing`, plus three plain agent target nodes —
/// the fixture every router-dialog test in this section starts from.
fn seed_router_graph(db: &Database) {
    use crate::domain::graphs::{
        Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpec,
    };

    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "rlp1".to_string(),
        name: "router graph".to_string(),
        description: None,
        workdir: "/tmp/router-test".to_string(),
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
        require_spec_type: false,
    })
    .unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "rs1".to_string(),
        graph_id: Some("rlp1".to_string()),
        name: "spec one".to_string(),
        description: None,
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

    db.insert_graph_node(&GraphNode {
        id: "router".to_string(),
        spec_id: Some("rs1".to_string()),
        graph_id: None,
        name: "Classify".to_string(),
        kind: GraphNodeKind::Router,
        config: serde_json::json!({
            "routes": [
                {"label": "billing", "description": "billing desc"},
                {"label": "technical", "description": "technical desc"},
                {"label": "sales", "description": "sales desc"},
                {"label": "escalation", "description": "escalation desc"},
            ],
            "fallback": "escalation",
        }),
        position: 0,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for (id, name, position) in [
        ("billing_agent", "Billing specialist", 1),
        ("technical_agent", "Technical specialist", 2),
        ("sales_agent", "Sales specialist", 3),
    ] {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: Some("rs1".to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }
    db.insert_graph_edge(&GraphEdge {
        id: "e_billing".to_string(),
        spec_id: Some("rs1".to_string()),
        graph_id: None,
        from_node: "router".to_string(),
        to_node: "billing_agent".to_string(),
        condition: GraphEdgeCondition::Route("billing".to_string()),
    })
    .unwrap();
}

fn app_on_router_node(db: &Arc<Database>, data_dir: &std::path::Path) -> App {
    let mut app = App::new(
        Arc::clone(db),
        data_dir,
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.refresh_graphs().expect("refresh graphs");
    app.graph_selected_spec = 0;
    app.graph_selected_node = 0; // "router" is position 0
    assert_eq!(
        app.selected_graph_node().map(|n| n.id.as_str()),
        Some("router"),
        "fixture invariant: router node must be selected"
    );
    app
}
// --- CT2 navigation tests ---

fn graph_state(
    nodes: &[(&str, &str)],
    edges: &[(&str, &str, crate::domain::graphs::GraphEdgeCondition)],
    current: Option<&str>,
) -> GraphLiveState {
    use crate::domain::graphs::{GraphEdge, GraphNode, GraphNodeKind};
    use chrono::Utc;
    use serde_json::json;
    let effective_nodes = nodes
        .iter()
        .enumerate()
        .map(|(i, (id, name))| GraphNode {
            id: id.to_string(),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position: i as i64,
            created_at: Utc::now(),
        })
        .collect::<Vec<_>>();
    let effective_edges = edges
        .iter()
        .enumerate()
        .map(|(i, (from, to, cond))| GraphEdge {
            id: format!("e{i}"),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition: (*cond).clone(),
        })
        .collect::<Vec<_>>();
    GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "graph".to_string(),
        graph_status: GraphStatus::Running,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: Some("s1".to_string()),
        pause_waiting_for: Vec::new(),
        effective_nodes,
        effective_edges,
        ensembles: Vec::new(),
        router_taken_routes: HashMap::new(),
        current_node_id: current.map(|s| s.to_string()),
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    }
}

mod part01_tests;
mod part02_tests;
mod part03_tests;
