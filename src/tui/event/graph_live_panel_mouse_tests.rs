use super::*;
use crate::db::Database;
use crate::domain::graphs::{GraphSpecStatus, GraphStatus};
use crate::tui::app::graph_live_state::{GraphLiveState, SpecQueueEntry};
use crate::tui::app::{AutomationKind, GraphLiveFocus};
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn spec_entry(id: &str, status: GraphSpecStatus) -> SpecQueueEntry {
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

/// Builds an App on the live graph view with a 3-spec queue and a marker
/// strip click map matching what `draw_graph_live_view` would have
/// produced for chips at columns `[0,3)`, `[4,7)`, `[8,11)` on row 5.
fn app_on_graph_live_view() -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.focus = Focus::Preview;
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = AutomationKind::Graph;
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
        spec_queue: vec![
            spec_entry("s1", GraphSpecStatus::Completed),
            spec_entry("s2", GraphSpecStatus::Running),
            spec_entry("s3", GraphSpecStatus::Failed),
        ],
        done_count: 1,
        total_count: 3,
        current_spec_id: Some("s2".to_string()),
        effective_nodes: Vec::new(),
        effective_edges: Vec::new(),
        ensembles: Vec::new(),
        router_taken_routes: HashMap::new(),
        current_node_id: None,
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    });

    app.last_panel_x = 0;
    app.last_panel_y = 2;
    app.last_panel_inner = (40, 20);
    app.graph_spec_strip_click_map = vec![
        ("s1".to_string(), 5, 0, 3),
        ("s2".to_string(), 5, 4, 7),
        ("s3".to_string(), 5, 8, 11),
    ];
    app.graph_spec_strip_capacity = 3;
    app
}

#[test]
fn clicking_a_marker_selects_that_spec_and_focuses_the_strip() {
    let mut app = app_on_graph_live_view();
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 9,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.graph_spec_strip_selected.as_deref(), Some("s3"));
    assert_eq!(app.graph_live_focus, GraphLiveFocus::SpecStrip);
}

#[test]
fn clicking_the_gap_between_chips_is_not_consumed() {
    let mut app = app_on_graph_live_view();
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 3,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

    assert!(!consumed);
    assert!(app.graph_spec_strip_selected.is_none());
}

#[test]
fn scroll_over_the_marker_row_pages_the_strip() {
    let mut app = app_on_graph_live_view();
    app.graph_spec_strip_capacity = 1; // force scrolling to matter
    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.graph_spec_strip_scroll, 1);
}

#[test]
fn mouse_ignored_when_not_on_the_graph_live_view() {
    let mut app = app_on_graph_live_view();
    app.automation_kind = AutomationKind::Agent;
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 9,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_graph_live_panel_mouse(&mut app, &mouse);

    assert!(!consumed);
    assert!(app.graph_spec_strip_selected.is_none());
}
