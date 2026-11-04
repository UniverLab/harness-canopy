use super::*;
use crate::db::Database;
use crate::tui::app::types::ProjectTab;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

#[test]
fn ct14_f2_clears_dangling_project_focus_defensively() {
    // Inject the illegal state directly: a `project_focus` surviving on a
    // non-Knowledge layer. Both wrappers must clear it idempotently.
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    app.sidebar_layer = SidebarLayer::Live;
    app.project_focus = Some(ProjectTab::Backlog);
    cycle_sidebar_layer_and_normalize_focus(&mut app);
    assert!(
        app.project_focus.is_none(),
        "F2 cycle must clear a dangling project focus"
    );

    app.sidebar_layer = SidebarLayer::Live;
    app.project_focus = Some(ProjectTab::Backlog);
    switch_sidebar_tab_and_normalize_focus(&mut app, SidebarLayer::Automation);
    assert!(
        app.project_focus.is_none(),
        "tab click must clear a dangling project focus"
    );
}
