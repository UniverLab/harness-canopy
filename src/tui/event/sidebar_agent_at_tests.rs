use super::*;
use std::sync::Arc;

#[test]
fn finds_agent_by_row() {
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
    app.sidebar_click_map = vec![(0, 0, 3), (1, 4, 7), (2, 8, 11)];

    assert_eq!(sidebar_agent_at(&app, 1), Some(0));
    assert_eq!(sidebar_agent_at(&app, 5), Some(1));
    assert_eq!(sidebar_agent_at(&app, 9), Some(2));
    assert!(sidebar_agent_at(&app, 3).is_none());
}
