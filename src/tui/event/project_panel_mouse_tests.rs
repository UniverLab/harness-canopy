use super::*;
use crate::db::Database;
use crate::domain::graphs::{GraphSpec, GraphSpecStatus};
use crate::domain::project::Project;
use crate::tui::app::types::ProjectTab;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn backlog_spec(id: &str, name: &str) -> GraphSpec {
    GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: name.to_string(),
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
        spec_code: None,
        spec_scope: None,
        spec_type: None,
    }
}

/// Builds an App focused on a single project's Backlog tab, with the
/// main panel occupying rows `[2, 22)` / columns `[0, 40)` — matching
/// what `draw_project_tabs_panel` would have set on the last frame —
/// and a tab bar + row click map for 3 backlog specs at rows `[3, 6)`.
fn app_with_project_backlog(count: usize) -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.projects = vec![Project::new("/tmp/project")];
    app.sidebar_layer = crate::tui::app::SidebarLayer::Knowledge;
    app.selected_project = 0;
    app.project_focus = Some(ProjectTab::Backlog);
    app.backlog_specs = (0..count)
        .map(|i| backlog_spec(&format!("spec-{i}"), &format!("Spec {i}")))
        .collect();
    app.selected_backlog = 0;
    app.focus = Focus::Agent;

    app.last_panel_x = 0;
    app.last_panel_y = 2;
    app.last_panel_inner = (40, 20);
    app.project_tab_click_map = vec![
        (ProjectTab::Overview, 0, 9),
        (ProjectTab::Backlog, 9, 17),
        (ProjectTab::Knowledge, 17, 27),
        (ProjectTab::History, 27, 35),
    ];
    app.project_tab_row_click_map = (0..count)
        .map(|i| (i, (3 + i) as u16, (4 + i) as u16))
        .collect();
    app
}

#[test]
fn clicking_tab_bar_switches_active_tab() {
    let mut app = app_with_project_backlog(3);
    assert_eq!(app.project_focus, Some(ProjectTab::Backlog));

    // Row == last_panel_y is the tab-bar row; column 20 falls inside the
    // Knowledge tab's span [17, 27).
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 20,
        row: app.last_panel_y,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.project_focus, Some(ProjectTab::Knowledge));
}

#[test]
fn clicking_a_list_row_selects_it_without_changing_tab() {
    let mut app = app_with_project_backlog(3);

    // Row 5 falls in spec #2's span [5, 6) from `project_tab_row_click_map`.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 5,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.selected_backlog, 2);
    assert_eq!(
        app.project_focus,
        Some(ProjectTab::Backlog),
        "clicking a row must not cycle the tab"
    );
}

#[test]
fn scroll_down_in_panel_advances_the_active_tabs_list() {
    let mut app = app_with_project_backlog(3);
    assert_eq!(app.selected_backlog, 0);

    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 5,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.selected_backlog, 1);
}

#[test]
fn scroll_up_in_panel_retreats_the_active_tabs_list() {
    let mut app = app_with_project_backlog(3);
    app.selected_backlog = 1;

    let mouse = MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 5,
        row: 10,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(consumed);
    assert_eq!(app.selected_backlog, 0);
}

#[test]
fn click_outside_the_panel_rect_is_not_consumed() {
    let mut app = app_with_project_backlog(3);

    // Row 30 is past last_panel_y (2) + last_panel_inner.1 (20) = 22.
    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 5,
        row: 30,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(!consumed);
    assert_eq!(app.selected_backlog, 0);
    assert_eq!(app.project_focus, Some(ProjectTab::Backlog));
}

#[test]
fn click_is_not_consumed_when_no_project_is_focused() {
    let mut app = app_with_project_backlog(3);
    app.project_focus = None;

    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 20,
        row: app.last_panel_y,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(!consumed);
}

#[test]
fn click_is_not_consumed_outside_the_knowledge_sidebar_layer() {
    let mut app = app_with_project_backlog(3);
    app.sidebar_layer = crate::tui::app::SidebarLayer::Live;

    let mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 20,
        row: app.last_panel_y,
        modifiers: KeyModifiers::NONE,
    };
    let consumed = handle_project_panel_mouse(&mut app, &mouse);

    assert!(!consumed);
    assert_eq!(
        app.project_focus,
        Some(ProjectTab::Backlog),
        "leaving Knowledge must not itself change the remembered tab"
    );
}
