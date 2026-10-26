use super::*;
use std::sync::Arc;
use std::time::Duration;

fn app_for_tick(focus: Focus) -> App {
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
    app.focus = focus;
    app
}

#[test]
fn agent_focus_returns_50ms() {
    let app = app_for_tick(Focus::Agent);
    assert_eq!(tick_duration(&app), Duration::from_millis(50));
}

#[test]
fn preview_focus_returns_100ms() {
    let app = app_for_tick(Focus::Preview);
    assert_eq!(tick_duration(&app), Duration::from_millis(100));
}

#[test]
fn home_without_brain_returns_200ms() {
    let app = app_for_tick(Focus::Home);
    assert_eq!(tick_duration(&app), Duration::from_millis(50));
}

#[test]
fn new_agent_dialog_returns_50ms() {
    let app = app_for_tick(Focus::NewAgentDialog);
    assert_eq!(tick_duration(&app), Duration::from_millis(50));
}

#[test]
fn prompt_template_dialog_returns_50ms() {
    let app = app_for_tick(Focus::PromptTemplateDialog);
    assert_eq!(tick_duration(&app), Duration::from_millis(50));
}
