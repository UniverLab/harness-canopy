use super::*;
use crate::db::Database;
use crate::tui::brians_brain::{BriansBrain, CellState};
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::sync::Arc;
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

fn all_off_brain(rows: usize, cols: usize) -> BriansBrain {
    let mut brain = BriansBrain::new(rows, cols);
    for cell in brain.grid.iter_mut() {
        *cell = CellState::Off;
    }
    brain
}

fn app_with_home_brain() -> App {
    let mut app = test_app();
    app.home_brain = Some(all_off_brain(10, 20));
    app.home_brain_rect = Some(Rect::new(5, 5, 20, 10));
    app
}

fn on_count(app: &App) -> usize {
    app.home_brain.as_ref().map_or(0, |brain| {
        brain
            .grid
            .iter()
            .filter(|cell| **cell == CellState::On)
            .count()
    })
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
fn moved_event_seeds_home_brain() {
    let mut app = app_with_home_brain();
    dispatch_event(&mut app, Event::Mouse(mouse(MouseEventKind::Moved, 8, 7))).unwrap();
    assert!(
        on_count(&app) > 0,
        "a Moved event inside the home brain's rect must seed it"
    );
}

#[test]
fn click_event_does_not_seed() {
    let mut app = app_with_home_brain();
    dispatch_event(
        &mut app,
        Event::Mouse(mouse(MouseEventKind::Down(MouseButton::Left), 8, 7)),
    )
    .unwrap();
    assert_eq!(on_count(&app), 0, "a click must not seed the brain");
}

#[test]
fn moved_event_outside_rect_changes_nothing() {
    let mut app = app_with_home_brain();
    dispatch_event(
        &mut app,
        Event::Mouse(mouse(MouseEventKind::Moved, 100, 100)),
    )
    .unwrap();
    assert_eq!(
        on_count(&app),
        0,
        "a move outside both brain rects must change nothing"
    );
}
