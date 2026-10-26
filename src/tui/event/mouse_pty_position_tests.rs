use super::*;
use std::sync::Arc;

fn mouse_at(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn make_app() -> App {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let db = Arc::new(crate::db::Database::new(&path).unwrap());
    let dir = tempfile::tempdir().unwrap();
    App::new(
        db,
        dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap()
}

#[test]
fn inside_panel_returns_relative_coords() {
    let mut app = make_app();
    app.last_panel_x = 10;
    app.last_panel_y = 5;
    app.last_panel_inner = (40, 20);

    assert_eq!(mouse_pty_position(&app, &mouse_at(15, 8)), Some((5, 3)));
}

#[test]
fn outside_panel_returns_none() {
    let mut app = make_app();
    app.last_panel_x = 10;
    app.last_panel_y = 5;
    app.last_panel_inner = (40, 20);

    assert!(mouse_pty_position(&app, &mouse_at(3, 8)).is_none());
    assert!(mouse_pty_position(&app, &mouse_at(55, 8)).is_none());
    assert!(mouse_pty_position(&app, &mouse_at(15, 3)).is_none());
    assert!(mouse_pty_position(&app, &mouse_at(15, 30)).is_none());
}
