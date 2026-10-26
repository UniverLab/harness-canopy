use super::*;
use crate::db::Database;
use crate::domain::models::{SplitGroup, SplitOrientation};
use chrono::Utc;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

fn spawn_test_terminal(name: &str) -> InteractiveAgent {
    InteractiveAgent::spawn_terminal(
        "cat",
        "/tmp",
        80,
        24,
        Some(name),
        &[],
        ratatui::style::Color::White,
    )
    .expect("spawn terminal")
}

/// Two terminal sessions in a horizontal split. Sets `last_panel_*` to
/// pretend the *focused* half was just rendered at x=41 (the panel to
/// the right of a 40-column left half), matching what `draw_split_panel`
/// records for whichever side has focus.
fn app_with_split_terminals(right_focused: bool) -> App {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.terminal_agents.push(spawn_test_terminal("left-term"));
    app.terminal_agents.push(spawn_test_terminal("right-term"));
    app.split_groups.push(SplitGroup {
        id: "split-1".to_string(),
        orientation: SplitOrientation::Horizontal,
        session_a: "left-term".to_string(),
        session_b: "right-term".to_string(),
        created_at: Utc::now(),
    });
    app.active_split_id = Some("split-1".to_string());
    app.split_right_focused = right_focused;
    app.focus = Focus::Agent;
    app.last_panel_x = if right_focused { 41 } else { 0 };
    app.last_panel_y = 1;
    app.last_panel_inner = (39, 20);
    app
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn mouse_pty_position_uses_focused_panel_x_offset() {
    let app = app_with_split_terminals(true);

    // Inside the right panel (starts at column 41).
    assert_eq!(
        mouse_pty_position(&app, &mouse(MouseEventKind::Moved, 45, 3)),
        Some((4, 2))
    );
    // Left of the focused panel's recorded x-offset: out of bounds.
    assert_eq!(
        mouse_pty_position(&app, &mouse(MouseEventKind::Moved, 10, 3)),
        None
    );
}

#[test]
fn clamped_pty_position_clamps_to_focused_panel_bounds() {
    let app = app_with_split_terminals(true);

    assert_eq!(
        clamped_pty_position(
            &app,
            &mouse(MouseEventKind::Drag(MouseButton::Left), 999, 999)
        ),
        (38, 19)
    );
}

#[test]
fn focused_terminal_like_follows_split_focus_not_sidebar_selection() {
    let mut app = app_with_split_terminals(false);
    // Sidebar selection points nowhere useful (default `selected = 0`
    // with no AgentEntry list) — the split's focused panel must still
    // resolve correctly.
    assert_eq!(focused_terminal_like(&app), Some((true, 0)));

    app.split_right_focused = true;
    assert_eq!(focused_terminal_like(&app), Some((true, 1)));
}

#[test]
fn drag_selection_in_split_targets_focused_right_panel() {
    let mut app = app_with_split_terminals(true);
    app.terminal_agents[1].replay_scrollback_lines(&["RIGHTPANELTEXT".to_string()]);

    let down = mouse(MouseEventKind::Down(MouseButton::Left), 41, 1);
    assert!(handle_selection_mouse(&mut app, &down));
    let sel = app.terminal_selection.as_ref().expect("selection started");
    assert_eq!(sel.agent, (true, 1));

    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 55, 1);
    assert!(handle_selection_mouse(&mut app, &drag));

    app.show_copied = false;
    let up = mouse(MouseEventKind::Up(MouseButton::Left), 55, 1);
    assert!(handle_selection_mouse(&mut app, &up));

    assert!(app.terminal_selection.is_none());
    assert!(
        app.show_copied,
        "expected the focused (right) panel's real text to be copied"
    );
}

#[test]
fn drag_selection_in_split_targets_focused_left_panel() {
    let mut app = app_with_split_terminals(false);
    app.terminal_agents[0].replay_scrollback_lines(&["LEFTPANELTEXT".to_string()]);

    let down = mouse(MouseEventKind::Down(MouseButton::Left), 0, 1);
    assert!(handle_selection_mouse(&mut app, &down));
    assert_eq!(
        app.terminal_selection
            .as_ref()
            .expect("selection started")
            .agent,
        (true, 0)
    );

    let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 14, 1);
    assert!(handle_selection_mouse(&mut app, &drag));

    app.show_copied = false;
    let up = mouse(MouseEventKind::Up(MouseButton::Left), 14, 1);
    assert!(handle_selection_mouse(&mut app, &up));

    assert!(app.show_copied);
}

#[test]
fn shift_click_copy_in_split_resolves_focused_panel_not_sidebar_selection() {
    let mut app = app_with_split_terminals(true);
    app.terminal_agents[1].replay_scrollback_lines(&["RIGHT SCREEN CONTENT".to_string()]);
    // `app.agents`/`app.selected` are left at their defaults (no sidebar
    // selection at all) — the old `selected_terminal_like`-based lookup
    // would resolve nothing and shift+click copy would silently no-op.

    let text = with_selected_terminal_like(&app, InteractiveAgent::get_plain_text_from_screen)
        .flatten()
        .unwrap_or_default();
    assert!(
        text.contains("RIGHT SCREEN CONTENT"),
        "expected shift+click's resolver to read the split-focused panel, got: {text:?}"
    );

    let handled = handle_copy_click(
        &mut app,
        &MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 41,
            row: 1,
            modifiers: KeyModifiers::SHIFT,
        },
    );
    assert!(handled);
}
