use super::*;
use crate::application::ports::AgentRepository;
use crate::domain::models::Cli;
use ratatui::style::Color;
use std::sync::Arc;

fn spawn_cat_interactive(name: &str) -> InteractiveAgent {
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

fn spawn_cat_terminal(name: &str) -> InteractiveAgent {
    InteractiveAgent::spawn_terminal("cat", "/tmp", 80, 24, Some(name), &[], Color::White)
        .expect("spawn cat as a stand-in terminal child")
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Previewing an interactive session with the central panel last
/// rendered at (30, 5) of size 80x20 — so (35, 10) is inside it.
fn app_previewing_interactive() -> App {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(crate::db::Database::new(&path).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        db,
        dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .unwrap();
    app.interactive_agents = vec![spawn_cat_interactive("ct6-click")];
    app.agents = vec![AgentEntry::Interactive(0)];
    app.selected = 0;
    app.focus = Focus::Preview;
    app.last_panel_x = 30;
    app.last_panel_y = 5;
    app.last_panel_inner = (80, 20);
    app
}

#[test]
fn central_left_click_on_interactive_session_enters_focus() {
    let mut app = app_previewing_interactive();
    app.log_scroll = 7;

    let consumed = handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10),
    );

    assert!(consumed, "central click must be consumed, not forwarded");
    assert!(matches!(app.focus, Focus::Agent));
    assert_eq!(app.log_scroll, 0, "same reset as keyboard focus entry");
    app.interactive_agents[0].kill();
}

#[test]
fn central_left_click_on_terminal_session_enters_focus() {
    let mut app = app_previewing_interactive();
    app.terminal_agents = vec![spawn_cat_terminal("ct6-click-term")];
    app.agents = vec![AgentEntry::Terminal(0)];

    let consumed = handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10),
    );

    assert!(consumed);
    assert!(matches!(app.focus, Focus::Agent));
    app.terminal_agents[0].kill();
    app.interactive_agents[0].kill();
}

#[test]
fn click_with_agent_focus_does_not_reenter() {
    // Once focused, clicks belong to the child — the helper must not
    // consume them, preserving focused-child mouse behavior.
    let mut app = app_previewing_interactive();
    app.focus = Focus::Agent;

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
    ));
    assert!(matches!(app.focus, Focus::Agent));
    app.interactive_agents[0].kill();
}

#[test]
fn click_outside_the_central_rectangle_is_not_consumed() {
    let mut app = app_previewing_interactive();

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 5, 10)
    ));
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
}

#[test]
fn non_left_clicks_are_not_consumed() {
    let mut app = app_previewing_interactive();

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Right), 35, 10)
    ));
    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::ScrollDown, 35, 10)
    ));
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
}

#[test]
fn background_agent_preview_is_not_consumed() {
    // A background (non-terminal) preview keeps its current behavior.
    let mut app = app_previewing_interactive();
    let db = app.db.clone();
    let agent = crate::domain::models::Agent {
        id: "bg-1".to_string(),
        prompt: "prompt".to_string(),
        trigger: None,
        cli: Cli::new("claude"),
        model: None,
        effort: None,
        working_dir: None,
        enabled: true,
        enable_at: None,
        created_at: chrono::Utc::now(),
        log_path: "/tmp/ct6-click.log".to_string(),
        timeout_minutes: 15,
        expires_at: None,
        last_run_at: None,
        last_run_ok: None,
        last_triggered_at: None,
        trigger_count: 0,
        action: crate::domain::models::AgentAction::Headless,
    };
    db.upsert_agent(&agent).expect("seed agent");
    app.agents = vec![AgentEntry::Agent(agent)];

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
    ));
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
}

#[test]
fn graph_live_view_central_click_is_not_consumed() {
    // The graph owns the centre in the graph view — even with a
    // terminal-like sidebar selection underneath.
    let mut app = app_previewing_interactive();
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = crate::tui::app::AutomationKind::Graph;

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
    ));
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
}

#[test]
fn playground_central_click_is_not_consumed() {
    let mut app = app_previewing_interactive();
    app.playground_active = true;

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
    ));
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
}

#[test]
fn missing_geometry_is_not_consumed() {
    // No frame rendered yet: leave dispatch unchanged.
    let mut app = app_previewing_interactive();
    app.last_panel_inner = (0, 0);

    assert!(!handle_preview_focus_click(
        &mut app,
        &mouse(MouseEventKind::Down(MouseButton::Left), 35, 10)
    ));
    assert!(matches!(app.focus, Focus::Preview));
    app.interactive_agents[0].kill();
}

#[test]
fn central_click_through_handle_mouse_enters_focus() {
    // End-to-end through the real dispatch order: the sidebar must not
    // see the click (hide it), panels above must pass, and the helper
    // consumes before PTY forwarding / selection below.
    let mut app = app_previewing_interactive();
    app.sidebar_visible = false;

    handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 35, 10),
    )
    .expect("handle mouse");

    assert!(matches!(app.focus, Focus::Agent));
    assert!(app.terminal_selection.is_none());
    app.interactive_agents[0].kill();
}
