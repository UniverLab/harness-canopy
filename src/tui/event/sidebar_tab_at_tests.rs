use super::*;
use std::sync::Arc;

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
fn finds_tab_by_position() {
    let mut app = make_app();
    app.sidebar_tab_click_map = vec![
        (SidebarLayer::Live, 0, 0, 11),
        (SidebarLayer::Automation, 0, 11, 22),
        (SidebarLayer::Knowledge, 0, 22, 33),
    ];

    assert_eq!(sidebar_tab_at(&app, 0, 5), Some(SidebarLayer::Live));
    assert_eq!(sidebar_tab_at(&app, 0, 15), Some(SidebarLayer::Automation));
    assert_eq!(sidebar_tab_at(&app, 0, 25), Some(SidebarLayer::Knowledge));
}

#[test]
fn wrong_row_returns_none() {
    let mut app = make_app();
    app.sidebar_tab_click_map = vec![(SidebarLayer::Live, 0, 0, 11)];

    assert!(sidebar_tab_at(&app, 1, 5).is_none());
}
