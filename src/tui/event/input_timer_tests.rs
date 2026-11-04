use super::*;
use crate::db::Database;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::{tempdir, NamedTempFile};

fn test_app() -> App {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).expect("create test db"));
    let data_dir = tempdir().expect("create data dir");
    App::new(
        db,
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app")
}

/// An app idle past the 45 s pause: any input must look newer than this.
fn idle_app() -> App {
    let mut app = test_app();
    app.focus = Focus::Home;
    app.last_input_at = Instant::now() - Duration::from_secs(60);
    app
}

/// FR2: a key press resets the idle timer.
#[test]
fn key_press_resets_the_idle_timer() {
    let mut app = idle_app();
    let before = app.last_input_at;
    dispatch_event(
        &mut app,
        Event::Key(KeyEvent::new(KeyCode::F(24), KeyModifiers::NONE)),
    )
    .unwrap();
    assert!(
        app.last_input_at > before,
        "a key press must reset the idle timer"
    );
}

/// FR2: mouse input resets the idle timer.
#[test]
fn mouse_move_resets_the_idle_timer() {
    let mut app = idle_app();
    let before = app.last_input_at;
    dispatch_event(
        &mut app,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .unwrap();
    assert!(
        app.last_input_at > before,
        "mouse input must reset the idle timer"
    );
}

/// FR2: pasted input counts as input too.
#[test]
fn paste_resets_the_idle_timer() {
    let mut app = idle_app();
    let before = app.last_input_at;
    dispatch_event(&mut app, Event::Paste("hi".to_string())).unwrap();
    assert!(
        app.last_input_at > before,
        "a paste must reset the idle timer"
    );
}

/// FR2: a resize is not key or mouse input — it must not resume animations.
#[test]
fn resize_does_not_reset_the_idle_timer() {
    let mut app = idle_app();
    let before = app.last_input_at;
    dispatch_event(&mut app, Event::Resize(10, 10)).unwrap();
    assert_eq!(
        app.last_input_at, before,
        "a resize must not reset the idle timer"
    );
}

/// FR2: a key release carries no new input — only presses count.
#[test]
fn key_release_does_not_reset_the_idle_timer() {
    let mut app = idle_app();
    let before = app.last_input_at;
    let mut key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    key.kind = KeyEventKind::Release;
    dispatch_event(&mut app, Event::Key(key)).unwrap();
    assert_eq!(
        app.last_input_at, before,
        "a key release must not reset the idle timer"
    );
}

/// FR2: focus loss pauses without touching the idle timer; focus gain
/// resumes from both pauses.
#[test]
fn focus_events_drive_the_pause() {
    let mut app = idle_app();
    let before = app.last_input_at;

    dispatch_event(&mut app, Event::FocusLost).unwrap();
    assert!(app.focus_lost, "focus lost must set the flag");
    assert_eq!(
        app.last_input_at, before,
        "focus lost must not reset the idle timer"
    );

    dispatch_event(&mut app, Event::FocusGained).unwrap();
    assert!(!app.focus_lost, "focus gained must clear the flag");
    assert!(
        app.last_input_at > before,
        "focus gained must resume the animations"
    );
}
