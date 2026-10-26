use super::*;
use std::sync::Arc;

#[test]
fn clamps_to_panel_bounds() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    let db = Arc::new(crate::db::Database::new(&path).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        db,
        dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    app.last_panel_x = 10;
    app.last_panel_y = 5;
    app.last_panel_inner = (40, 20);

    let mouse = MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column: 999,
        row: 999,
        modifiers: KeyModifiers::NONE,
    };
    assert_eq!(clamped_pty_position(&app, &mouse), (39, 19));
}
