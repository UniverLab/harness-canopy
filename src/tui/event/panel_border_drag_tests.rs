use super::*;
use crate::db::Database;
use std::sync::Arc;
use tempfile::tempdir;

/// An app with a selected terminal session and a drawn layout: body 120,
/// sync panel 30 wide at `x = 90`, pane inner `(88, 24)` (chrome 2).
fn drag_app() -> App {
    let tmp = tempfile::NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).expect("create test db"));
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let agent = crate::tui::agent::InteractiveAgent::spawn_terminal(
        "cat",
        "/tmp",
        80,
        24,
        Some("border-drag"),
        &[],
        ratatui::style::Color::White,
    )
    .expect("spawn terminal");
    app.terminal_agents.push(agent);
    app.agents = vec![AgentEntry::Terminal(0)];
    app.selected = 0;
    app.last_body_width = 120;
    app.last_panel_inner = (88, 24);
    app.last_sync_area = Some(ratatui::layout::Rect::new(90, 0, 30, 24));
    app
}

fn press(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn drag_to(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn release(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn press_on_the_border_starts_the_drag() {
    let mut app = drag_app();
    assert!(handle_panel_border_drag(&mut app, &press(90, 5)));
    assert!(app.activity_panel_dragging);
    app.terminal_agents[0].kill();
}

#[test]
fn press_off_the_border_does_not_start_a_drag() {
    let mut app = drag_app();
    // Inside the panel and outside it both miss the border column.
    assert!(!handle_panel_border_drag(&mut app, &press(95, 5)));
    assert!(!handle_panel_border_drag(&mut app, &press(10, 5)));
    assert!(!app.activity_panel_dragging);
    app.terminal_agents[0].kill();
}

#[test]
fn drag_moves_the_user_width() {
    let mut app = drag_app();
    assert!(handle_panel_border_drag(&mut app, &press(90, 5)));
    // Sync right edge is 120; column 80 leaves a 40-wide panel.
    assert!(handle_panel_border_drag(&mut app, &drag_to(80, 5)));
    assert_eq!(app.activity_panel_user_width, Some(40));
    assert!(app.activity_panel_dragging);
    app.terminal_agents[0].kill();
}

#[test]
fn drag_clamps_at_both_ends() {
    let mut app = drag_app();
    assert!(handle_panel_border_drag(&mut app, &press(90, 5)));
    // Past the right edge collapses to the panel minimum, not to zero.
    assert!(handle_panel_border_drag(&mut app, &drag_to(200, 5)));
    assert_eq!(app.activity_panel_user_width, Some(24));
    // Past the left edge stops at body minus 40, so the pane keeps width.
    assert!(handle_panel_border_drag(&mut app, &drag_to(0, 5)));
    assert_eq!(app.activity_panel_user_width, Some(80));
    app.terminal_agents[0].kill();
}

#[test]
fn drag_does_not_resize_until_release() {
    let mut app = drag_app();
    assert!(handle_panel_border_drag(&mut app, &press(90, 5)));
    assert!(handle_panel_border_drag(&mut app, &drag_to(80, 5)));
    assert_eq!(
        (
            app.terminal_agents[0].last_pty_cols,
            app.terminal_agents[0].last_pty_rows
        ),
        (80, 24),
        "the PTY must not resize on a drag step"
    );
    // A tick fires mid-drag: the frame already narrowed the pane to the
    // dragged width (sync area included), but the bulk resizer must still
    // leave the PTY alone.
    app.last_sync_area = Some(ratatui::layout::Rect::new(80, 0, 40, 24));
    app.last_panel_inner = (78, 24);
    app.resize_interactive_agents();
    assert_eq!(
        (
            app.terminal_agents[0].last_pty_cols,
            app.terminal_agents[0].last_pty_rows
        ),
        (80, 24),
        "a tick mid-drag must not resize the PTY either"
    );

    assert!(handle_panel_border_drag(&mut app, &release(80, 5)));
    assert!(!app.activity_panel_dragging);
    // Body 120, user 40, chrome 2: the pane inner is 78. The warp-mode
    // terminal keeps 3 rows for its input box.
    assert_eq!(
        (
            app.terminal_agents[0].last_pty_cols,
            app.terminal_agents[0].last_pty_rows
        ),
        (78, 21),
        "release resizes the PTY once, to the dragged width"
    );
    app.terminal_agents[0].kill();
}

#[test]
fn drag_is_ignored_while_a_remote_owns_the_size() {
    let mut app = drag_app();
    app.terminal_agents[0].remote_size = Some((60, 20));
    assert!(!handle_panel_border_drag(&mut app, &press(90, 5)));
    assert!(!app.activity_panel_dragging);
    assert!(app.activity_panel_user_width.is_none());
    app.terminal_agents[0].kill();
}
