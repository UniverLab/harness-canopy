use super::*;
use crate::db::Database;
use crate::tui::gamification::MissionEvent;
use std::sync::Arc;
use tempfile::{tempdir, NamedTempFile};

fn night_app_with_fireflies() -> App {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let db = Arc::new(Database::new(&path).expect("create test db"));
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        db,
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    // Seed the night scene so live fireflies exist to click on.
    app.atmosphere_ctx.hour = 23;
    app.atmosphere.tick(
        ratatui::layout::Rect::new(0, 0, 80, 24),
        &mut app.atmosphere_ctx,
    );
    app.atmosphere_hidden = false;
    app
}

fn find_hit_cell(app: &App) -> (u16, u16) {
    for row in 0..24u16 {
        for col in 0..80u16 {
            if app.atmosphere.catch_at(col, row) {
                return (col, row);
            }
        }
    }
    panic!("seeded night scene must contain a live firefly");
}

fn click(app: &mut App, col: u16, row: u16) {
    dispatch_event(
        app,
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .unwrap();
}

#[test]
fn firefly_click_near_queues_caught() {
    let mut app = night_app_with_fireflies();
    let (col, row) = find_hit_cell(&app);
    click(&mut app, col, row);
    assert!(
        app.mission_pending_events
            .contains(&MissionEvent::FireflyCaught),
        "a click within 1.5 cells of a live firefly must queue FireflyCaught"
    );
}

#[test]
fn firefly_click_far_queues_nothing() {
    let mut app = night_app_with_fireflies();
    let (hit_col, hit_row) = find_hit_cell(&app);
    // Three cells away from the hit: outside the 1.5-cell catch range.
    let miss = [
        (hit_col.saturating_add(3), hit_row),
        (hit_col.saturating_sub(3), hit_row),
        (hit_col, hit_row.saturating_add(3)),
        (hit_col, hit_row.saturating_sub(3)),
    ]
    .into_iter()
    .find(|(col, row)| !app.atmosphere.catch_at(*col, *row))
    .expect("a cell 3 away from the hit must miss every fly");
    click(&mut app, miss.0, miss.1);
    assert!(
        !app.mission_pending_events
            .contains(&MissionEvent::FireflyCaught),
        "a click 3 cells away from every live firefly must not queue FireflyCaught"
    );
}
