//! Shift+←/→ steps the sidebar tab strip without entering the tab's panel.
//!
//! From a focused session, Shift+Left used to land on the Knowledge tab
//! already inside its project tabs, so the next Shift+arrow cycled project
//! tabs instead of sidebar tabs until Esc was pressed. Shift+arrows are
//! frame navigation: they only select the sidebar tab. Entering a tab's
//! panel is Enter's job.

use super::*;
use crate::db::Database;
use crate::domain::models::Cli;
use crate::tui::agent::InteractiveAgent;
use crate::tui::app::types::ProjectTab;
use ratatui::style::Color;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn spawn_cat_agent(name: &str) -> InteractiveAgent {
    InteractiveAgent::spawn(
        Cli::new("cat"),
        env!("CARGO_MANIFEST_DIR"),
        80,
        24,
        None,
        None,
        Color::Reset,
        Some(name),
        None,
        &[],
        None,
        None,
        None,
        None,
    )
    .expect("spawn cat as a stand-in interactive child")
}

/// One focused interactive session on Live, plus one project so the
/// Knowledge tab has something to enter.
fn app_with_focused_session() -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.interactive_agents = vec![spawn_cat_agent("shift-step")];
    app.agents = vec![AgentEntry::Interactive(0)];
    app.projects = vec![crate::domain::project::Project::new("/tmp/shift-step-proj")];
    app.selected = 0;
    app.selected_project = 0;
    app.sidebar_layer = SidebarLayer::Live;
    app.focus = Focus::Agent;
    app
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    handle_key(app, code, modifiers).expect("handle key");
}

#[test]
fn shift_left_three_times_visits_each_sidebar_layer_without_entering_panels() {
    let mut app = app_with_focused_session();

    // Ring order is Live → Automation → Knowledge, so three steps left
    // from Live visit Knowledge, Automation, then Live again.
    for layer in [
        SidebarLayer::Knowledge,
        SidebarLayer::Automation,
        SidebarLayer::Live,
    ] {
        press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);

        assert_eq!(app.sidebar_layer, layer);
        assert!(
            !matches!(app.focus, Focus::Agent),
            "Shift+arrows select the sidebar tab; they must never enter its panel"
        );
        assert!(
            app.project_focus.is_none(),
            "arriving on a sidebar tab must not open the Knowledge project tabs"
        );
    }
}

#[test]
fn enter_enters_knowledge_project_tabs_and_esc_leaves() {
    let mut app = app_with_focused_session();
    press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
    assert_eq!(app.sidebar_layer, SidebarLayer::Knowledge);
    assert!(matches!(app.focus, Focus::Preview));

    // Enter on the selected tab enters its panel, as before.
    press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(app.project_focus, Some(ProjectTab::Overview));
    assert!(matches!(app.focus, Focus::Agent));

    // Inside the project tabs, Shift+arrows keep cycling them.
    press(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(
        app.sidebar_layer,
        SidebarLayer::Knowledge,
        "in-tab Shift+arrows still cycle the project tabs, not the sidebar"
    );
    assert_eq!(app.project_focus, Some(ProjectTab::Backlog));

    // Esc leaves the panel; the next Shift+arrow steps the sidebar again.
    press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.project_focus.is_none());
    assert!(matches!(app.focus, Focus::Preview));

    press(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
    assert_eq!(app.sidebar_layer, SidebarLayer::Live);
    assert!(matches!(app.focus, Focus::Preview));
}
