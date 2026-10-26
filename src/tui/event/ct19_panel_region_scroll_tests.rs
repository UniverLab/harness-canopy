use super::*;
use crate::db::Database;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn test_app() -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app")
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Panel spans (0,0)-(40,20); knowledge list rect sits inside it at
/// (0,10)-(40,20). Scrolling with the pointer inside the list rect must
/// move only the list offset, not `sync_scroll_offset` — the CT19 bug
/// was every region moving the activity offset.
#[test]
fn scroll_inside_knowledge_list_rect_moves_only_list_offset() {
    let mut app = test_app();
    app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
    app.last_activity_rect = Some(ratatui::layout::Rect::new(0, 0, 40, 6));
    app.last_knowledge_list_rect = Some(ratatui::layout::Rect::new(0, 10, 40, 10));
    app.project_knowledge = (0..20)
        .map(|i| crate::db::intelligence::IntelligenceNodeRecord {
            id: format!("k{i}"),
            kind: "fact".to_string(),
            status: "active".to_string(),
            title: format!("node {i}"),
            body: String::new(),
            metadata: None,
            project_hash: None,
            session_id: None,
            created_at: 0,
            updated_at: 0,
        })
        .collect();
    app.sync_scroll_offset = 5;

    let consumed =
        handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 15), -1);

    assert!(consumed);
    assert_eq!(app.sync_scroll_offset, 5, "activity offset must not move");
    assert_eq!(app.knowledge_list_scroll, 1);
    assert!(app.panel_interacting, "scroll must mark panel interaction");
}

/// Pointer inside the Graph face's rect calls the graph-face scroll,
/// clamped by its own bookkeeping (not the main pane's).
#[test]
fn scroll_inside_graph_face_rect_calls_graph_face_scroll() {
    let mut app = test_app();
    app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
    app.last_graph_face_rect = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
    app.graph_face_total_lines = 50;
    app.graph_face_scroll = 0;

    let consumed = handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 5), -1);

    assert!(consumed);
    assert_eq!(app.graph_face_scroll, 3);
    // The main pane's own field is untouched by this path.
    assert_eq!(app.graph_live_view_scroll, 0);
}

/// Pointer inside the panel but outside every recorded region: consumed
/// (never falls through to the sidebar), and changes nothing.
#[test]
fn scroll_inside_panel_outside_every_region_is_consumed_and_noop() {
    let mut app = test_app();
    app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
    app.last_activity_rect = Some(ratatui::layout::Rect::new(0, 0, 40, 6));
    app.last_knowledge_graph_rect = None;
    app.last_knowledge_list_rect = None;
    app.last_graph_face_rect = None;
    app.sync_scroll_offset = 5;
    app.knowledge_list_scroll = 2;

    // Row 15 is inside the panel (0..20) but outside the activity rect
    // (0..6) and no other region is recorded this frame.
    let consumed =
        handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 15), -1);

    assert!(
        consumed,
        "a wheel event inside the panel is always consumed"
    );
    assert_eq!(app.sync_scroll_offset, 5);
    assert_eq!(app.knowledge_list_scroll, 2);
    assert!(app.panel_interacting);
}

/// Pointer inside the knowledge-graph rect moves only the graph offset,
/// clamped to the edge count (functional requirement 5: never blank).
#[test]
fn scroll_inside_knowledge_graph_rect_clamps_to_edge_count() {
    let mut app = test_app();
    app.last_sync_area = Some(ratatui::layout::Rect::new(0, 0, 40, 20));
    app.last_knowledge_graph_rect = Some(ratatui::layout::Rect::new(0, 6, 40, 3));
    app.project_graph_edges = (0..4)
        .map(|i| crate::tui::app::types::ProjectGraphEdge {
            from_name: format!("a{i}"),
            to_name: format!("b{i}"),
            from_hash: format!("ha{i}"),
            to_hash: format!("hb{i}"),
            relation: "relates_to".to_string(),
        })
        .collect();

    // Scroll down repeatedly past the end: must clamp, not grow forever.
    for _ in 0..10 {
        handle_sync_panel_scroll(&mut app, &mouse(MouseEventKind::ScrollDown, 5, 7), -1);
    }

    // 4 edges, 3 visible rows -> max_offset = 1.
    assert_eq!(app.knowledge_graph_scroll, 1);
}
