//! Key and mouse routing for the Hub (`F1`) Calendar tab.

use ratatui::crossterm::event::{KeyCode, MouseButton, MouseEvent, MouseEventKind};

use super::mouse::rect_contains_point;
use crate::tui::app::hub_calendar::CalendarSubView;
use crate::tui::app::types::App;

/// Handle a key press while the Hub Calendar tab is active.
pub(super) fn handle_hub_calendar_key(app: &mut App, code: KeyCode) {
    if app.hub.calendar.add_form.open {
        handle_add_form_key(app, code);
        return;
    }

    match code {
        KeyCode::Up | KeyCode::Char('k') => app.move_calendar_selection(false),
        KeyCode::Down | KeyCode::Char('j') => app.move_calendar_selection(true),
        KeyCode::Char('m') => app.toggle_calendar_sub_view(),
        KeyCode::Char('a') => app.calendar_open_add_form(),
        KeyCode::Char('d') if app.hub.calendar.sub_view == CalendarSubView::Timeline => {
            app.calendar_dismiss_selected();
        }
        KeyCode::Char('s') if app.hub.calendar.sub_view == CalendarSubView::Timeline => {
            app.calendar_snooze_selected();
        }
        KeyCode::Char('r') if app.hub.calendar.sub_view == CalendarSubView::Timeline => {
            app.calendar_run_selected();
        }
        KeyCode::Char(' ') if app.hub.calendar.sub_view == CalendarSubView::Milestones => {
            app.calendar_cycle_milestone_status();
        }
        _ => {}
    }
}

fn handle_add_form_key(app: &mut App, code: KeyCode) {
    let field_count = app.hub.calendar.add_form.field_count();
    match code {
        KeyCode::Esc => app.calendar_cancel_add_form(),
        KeyCode::Enter => app.calendar_submit_add_form(),
        KeyCode::Tab | KeyCode::Down => {
            app.hub.calendar.add_form.field = (app.hub.calendar.add_form.field + 1) % field_count;
        }
        KeyCode::BackTab | KeyCode::Up => {
            app.hub.calendar.add_form.field =
                (app.hub.calendar.add_form.field + field_count - 1) % field_count;
        }
        KeyCode::Backspace => {
            app.hub.calendar.add_form.active_buffer_mut().pop();
        }
        KeyCode::Char(c) => {
            app.hub.calendar.add_form.active_buffer_mut().push(c);
        }
        _ => {}
    }
}

/// Handle mouse clicks inside the Hub Calendar tab.
pub(super) fn handle_hub_calendar_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return false;
    }
    if let Some(rect) = app.hub.calendar.timeline_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            app.set_calendar_sub_view(CalendarSubView::Timeline);
            return true;
        }
    }
    if let Some(rect) = app.hub.calendar.milestones_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            app.set_calendar_sub_view(CalendarSubView::Milestones);
            return true;
        }
    }
    if let Some(rect) = app.hub.calendar.list_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            return select_calendar_list_row(app, (mouse.row - rect.y) as usize);
        }
    }
    false
}

fn select_calendar_list_row(app: &mut App, offset: usize) -> bool {
    match app.hub.calendar.sub_view {
        CalendarSubView::Timeline if offset < app.hub.calendar.timeline.len() => {
            app.hub.calendar.timeline_selected = offset;
            true
        }
        CalendarSubView::Milestones => {
            let Some(Some(idx)) = app.hub.calendar.milestone_row_indices.get(offset).copied()
            else {
                return false;
            };
            if idx < app.hub.calendar.milestones.len() {
                app.hub.calendar.milestone_selected = idx;
                return true;
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::domain::calendar::MilestoneStatus;
    use crate::domain::canopy_config::CanopyConfig;
    use crate::tui::app::hub::HubTab;
    use chrono::{Duration, SecondsFormat, Utc};
    use std::sync::Arc;

    fn test_app() -> App {
        let path = crate::test_support::temp_db_path();
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        App::new(db, data_dir.path(), &CanopyConfig::default()).unwrap()
    }

    #[test]
    fn subview_toggle_and_quick_actions_update_state() {
        let mut app = test_app();
        let at = (Utc::now() + Duration::hours(2)).to_rfc3339_opts(SecondsFormat::Secs, true);
        app.db
            .create_calendar_event("Standup", &at, None, None, None)
            .unwrap();
        app.db
            .create_calendar_milestone("/tmp/proj", "v1", None, None, None)
            .unwrap();

        app.open_hub();
        app.set_hub_tab(HubTab::Calendar);
        assert_eq!(app.hub.calendar.sub_view, CalendarSubView::Timeline);
        assert_eq!(app.hub.calendar.timeline.len(), 1);

        // Snooze 5m
        handle_hub_calendar_key(&mut app, KeyCode::Char('s'));
        assert!(app
            .hub
            .calendar
            .status_message
            .as_deref()
            .unwrap()
            .contains("Snoozed"));

        // Dismiss
        handle_hub_calendar_key(&mut app, KeyCode::Char('d'));
        assert!(app.hub.calendar.timeline.is_empty());

        // Toggle to Milestones and cycle status with Space
        handle_hub_calendar_key(&mut app, KeyCode::Char('m'));
        assert_eq!(app.hub.calendar.sub_view, CalendarSubView::Milestones);
        handle_hub_calendar_key(&mut app, KeyCode::Char(' '));
        assert_eq!(
            app.hub.calendar.milestones[0].status,
            MilestoneStatus::Active
        );
        handle_hub_calendar_key(&mut app, KeyCode::Char(' '));
        assert_eq!(app.hub.calendar.milestones[0].status, MilestoneStatus::Done);
    }
}
