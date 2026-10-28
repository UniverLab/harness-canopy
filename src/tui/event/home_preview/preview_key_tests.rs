//! `preview_key_tests` tests (split from `src/tui/event/home_preview.rs`, pure move).
use super::*;
use crate::db::Database;
use crate::domain::models::{Agent, Cli, Trigger};
use crate::tui::app::types::App;
use chrono::Utc;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn cron_agent(id: &str) -> Agent {
    Agent {
        id: id.to_string(),
        prompt: "prompt".to_string(),
        trigger: Some(Trigger::Cron {
            schedule_expr: "0 9 * * *".to_string(),
        }),
        cli: Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: Utc::now(),
        log_path: "/tmp/test-preview.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    }
}

fn app_with_agents() -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.agents = vec![AgentEntry::Agent(cron_agent("a1"))];
    app.selected = 0;
    app
}

// ── Graph run-time controls ──────────────────────────────────────

fn graph_with_status(status: crate::domain::graphs::GraphStatus) -> crate::domain::graphs::Graph {
    crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "lp1".to_string(),
        name: "Nightly review".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status,
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

fn app_on_graph(status: crate::domain::graphs::GraphStatus) -> App {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;
    app.graphs = vec![graph_with_status(status)];
    app.selected_graph_id = Some("lp1".to_string());
    app
}

/// The Running-graph live snapshot the CT23/CT24 graph-view key tests
/// navigate: engine on `n1`, effective nodes `n1 -> n2` (pass). Since the
/// temp DB has no graph rows, tests that drive Enter/Esc must install it
/// themselves (a None live state makes Enter a no-op by design — CT24).
fn running_live_state() -> crate::tui::app::graph_live_state::GraphLiveState {
    crate::tui::app::graph_live_state::GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "Nightly review".to_string(),
        graph_status: crate::domain::graphs::GraphStatus::Running,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: None,
        effective_nodes: vec![
            crate::domain::graphs::GraphNode {
                id: "n1".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Implement".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            crate::domain::graphs::GraphNode {
                id: "n2".to_string(),
                spec_id: None,
                graph_id: Some("lp1".to_string()),
                name: "Review".to_string(),
                kind: crate::domain::graphs::GraphNodeKind::Check,
                config: serde_json::json!({}),
                position: 1,
                created_at: Utc::now(),
            },
        ],
        effective_edges: vec![crate::domain::graphs::GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            from_node: "n1".to_string(),
            to_node: "n2".to_string(),
            condition: crate::domain::graphs::GraphEdgeCondition::Pass,
        }],
        ensembles: Vec::new(),
        router_taken_routes: std::collections::HashMap::new(),
        current_node_id: Some("n1".to_string()),
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    }
}

// ── CT3: tail dialog key bindings ──────────────────────────────

fn app_with_open_tail() -> App {
    let mut app = app_on_graph(crate::domain::graphs::GraphStatus::Running);
    app.node_tail_dialog = Some(crate::tui::app::dialog::NodeTailDialog {
        run_id: Some("run1".to_string()),
        node_name: "Node node1".to_string(),
        stdout_lines: vec!["out".to_string()],
        stderr_lines: Vec::new(),
        status: Some(crate::domain::graphs::GraphRunStatus::Running),
        timed_out: false,
        started_at: Some(chrono::Utc::now()),
        scroll: 0,
        member_pos: None,
        platform: None,
        model: None,
        member_cursor: 0,
        member_count: 0,
        no_run_spec_name: None,
        last_resolved: None,
    });
    app
}

// ── CT28: pane follows the highlight ───────────────────────────

/// Seed graph `lp1` + spec `s1` + the given Agent nodes so the pane has
/// runs to resolve (the temp DB the test App holds is otherwise empty).
fn seed_live_graph(db: &Database, node_ids: &[&str]) {
    db.insert_graph(&crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "lp1".to_string(),
        name: "Nightly review".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: crate::domain::graphs::GraphStatus::Running,
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
    })
    .unwrap();
    db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
        id: "s1".to_string(),
        graph_id: Some("lp1".to_string()),
        name: "Spec".to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: crate::domain::graphs::GraphSpecStatus::Running,
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
    for (i, id) in node_ids.iter().enumerate() {
        db.insert_graph_node(&crate::domain::graphs::GraphNode {
            id: id.to_string(),
            spec_id: Some("s1".to_string()),
            graph_id: None,
            name: format!("Node {id}"),
            kind: crate::domain::graphs::GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: i as i64,
            created_at: Utc::now(),
        })
        .unwrap();
    }
}

fn insert_live_run(
    db: &Database,
    run_id: &str,
    node_id: &str,
    status: crate::domain::graphs::GraphRunStatus,
    started_at: chrono::DateTime<Utc>,
) {
    db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
        id: run_id.to_string(),
        graph_id: "lp1".to_string(),
        spec_id: "s1".to_string(),
        node_id: node_id.to_string(),
        status,
        input: None,
        output: None,
        started_at,
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

/// Seeds a Completed graph `lp1` with a bound Completed spec `s1` and two
/// graph-level nodes `n1 -> n2` (pass). No runs recorded. Used by the
/// CT24 tests so the refresh path (`refresh_graphs_selection`) can
/// reassemble a live state whose effective nodes still contain the graph.
fn seed_completed_graph_with_nodes(db: &Database) {
    db.insert_graph(&crate::domain::graphs::Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "lp1".to_string(),
        name: "Nightly review".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: crate::domain::graphs::GraphStatus::Completed,
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
    })
    .unwrap();
    db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
        id: "s1".to_string(),
        graph_id: Some("lp1".to_string()),
        name: "Spec".to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: crate::domain::graphs::GraphSpecStatus::Completed,
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
    for (position, id) in ["n1", "n2"].iter().enumerate() {
        db.insert_graph_node(&crate::domain::graphs::GraphNode {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            name: id.to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: position as i64,
            created_at: Utc::now(),
        })
        .unwrap();
    }
    db.insert_graph_edge(&crate::domain::graphs::GraphEdge {
        id: "e1".to_string(),
        spec_id: None,
        graph_id: Some("lp1".to_string()),
        from_node: "n1".to_string(),
        to_node: "n2".to_string(),
        condition: crate::domain::graphs::GraphEdgeCondition::Pass,
    })
    .unwrap();
}

fn app_on_two_graphs() -> App {
    let mut app = app_with_agents();
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;
    app.agents = Vec::new();
    let now = Utc::now();
    let mut g1 = graph_with_status(crate::domain::graphs::GraphStatus::Running);
    g1.id = "g1".to_string();
    g1.created_at = now;
    let mut g2 = graph_with_status(crate::domain::graphs::GraphStatus::Running);
    g2.id = "g2".to_string();
    g2.created_at = now - chrono::Duration::seconds(10);
    app.graphs = vec![g1, g2];
    app.selected_graph_id = Some("g1".to_string());
    app
}

fn spawn_cat_agent(name: &str) -> crate::tui::agent::InteractiveAgent {
    crate::tui::agent::InteractiveAgent::spawn(
        Cli::new("cat"),
        env!("CARGO_MANIFEST_DIR"),
        80,
        24,
        None,
        None,
        ratatui::style::Color::Reset,
        Some(name),
        None,
        &[],
        None,
        None,
        None,
    )
    .expect("spawn cat as a stand-in interactive child")
}

// ── CT6: preview owns plain Up/Down regardless of child claim ────

fn app_with_two_claimed_interactive_sessions() -> App {
    let first = spawn_cat_agent("ct6-first");
    *first.kitty_keyboard_flags.lock().expect("lock") = Some(7);
    let second = spawn_cat_agent("ct6-second");
    second.vt.lock().expect("vt lock").process(b"\x1b[?1049h");
    assert!(first.kitty_keyboard_negotiated());
    assert!(second.in_alternate_screen());

    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.interactive_agents = vec![first, second];
    app.agents = vec![AgentEntry::Interactive(0), AgentEntry::Interactive(1)];
    app.selected = 0;
    app.focus = Focus::Preview;
    app
}

mod part01_tests;
