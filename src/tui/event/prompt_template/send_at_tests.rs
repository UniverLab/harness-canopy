use crate::tui::app::dialog::{SendChoice, SimplePromptDialog};

#[test]
fn send_control_starts_on_now() {
    let dialog = SimplePromptDialog::new();
    assert_eq!(dialog.send_choice, SendChoice::Now);
    assert!(dialog.send_at.is_none());
    assert!(dialog.send_edit.is_none());
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "now");
}

#[test]
fn lateral_toggle_switches_now_and_date() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    assert_eq!(dialog.send_choice, SendChoice::Date);
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "date");
    // Toggling back to now discards any picked time.
    dialog.send_at = Some(chrono::Local::now().naive_local() + chrono::Duration::hours(2));
    dialog.send_toggle();
    assert_eq!(dialog.send_choice, SendChoice::Now);
    assert!(dialog.send_at.is_none());
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "now");
}

#[test]
fn begin_edit_preseeds_current_local_time() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle(); // → date
    dialog.send_begin_edit();
    let edit = dialog.send_edit.expect("picker open");
    assert_eq!(edit.field, 0);
    let now = chrono::Local::now().naive_local();
    let delta = (now - edit.value).num_seconds().abs();
    assert!(delta < 120, "picker must preseed the current time");
}

#[test]
fn edit_adjust_uses_calendar_math() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    dialog.send_begin_edit();
    let base = chrono::NaiveDate::from_ymd_opt(2026, 1, 31)
        .unwrap()
        .and_hms_opt(10, 0, 0)
        .unwrap();
    dialog.send_edit.as_mut().unwrap().value = base;
    // +1 month from Jan 31 clamps to Feb 28 instead of panicking.
    dialog.send_edit.as_mut().unwrap().field = 1;
    dialog.send_edit_adjust(1);
    let v = dialog.send_edit.unwrap().value;
    assert_eq!(v.date().to_string(), "2026-02-28");
    // Minute adjustment carries across the hour.
    dialog.send_edit.as_mut().unwrap().field = 4;
    dialog.send_edit.as_mut().unwrap().value = base;
    dialog.send_edit_adjust(-1);
    let v = dialog.send_edit.unwrap().value;
    assert_eq!(v.format("%H:%M").to_string(), "09:59");
}

#[test]
fn confirm_rejects_past_times_with_inline_hint() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().value =
        chrono::Local::now().naive_local() - chrono::Duration::hours(1);
    assert!(!dialog.send_edit_confirm());
    assert!(dialog.send_error.is_some());
    assert!(dialog.send_at.is_none());
    assert!(dialog.send_edit.is_some(), "picker stays open to fix it");
}

#[test]
fn confirm_accepts_future_time_and_displays_it() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    dialog.send_begin_edit();
    let future = chrono::Local::now().naive_local() + chrono::Duration::hours(3);
    dialog.send_edit.as_mut().unwrap().value = future;
    assert!(dialog.send_edit_confirm());
    assert_eq!(dialog.send_at, Some(future));
    assert!(dialog.send_edit.is_none());
    assert!(dialog.send_error.is_none());
    assert_eq!(
        dialog.send_display("%Y-%m-%d %H:%M"),
        future.format("%Y-%m-%d %H:%M").to_string()
    );
}

#[test]
fn cancel_without_confirmed_time_returns_to_now() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    dialog.send_begin_edit();
    dialog.send_edit_cancel();
    assert!(dialog.send_edit.is_none());
    assert_eq!(dialog.send_choice, SendChoice::Now);
    // But a previously confirmed time survives a later canceled edit.
    dialog.send_toggle();
    dialog.send_begin_edit();
    let future = chrono::Local::now().naive_local() + chrono::Duration::hours(3);
    dialog.send_edit.as_mut().unwrap().value = future;
    assert!(dialog.send_edit_confirm());
    dialog.send_begin_edit();
    dialog.send_edit_cancel();
    assert_eq!(dialog.send_choice, SendChoice::Date);
    assert_eq!(dialog.send_at, Some(future));
}

#[test]
fn clear_send_at_resets_everything() {
    let mut dialog = SimplePromptDialog::new();
    dialog.send_toggle();
    dialog.send_begin_edit();
    let future = chrono::Local::now().naive_local() + chrono::Duration::hours(3);
    dialog.send_edit.as_mut().unwrap().value = future;
    dialog.send_edit_confirm();
    dialog.clear_send_at();
    assert_eq!(dialog.send_choice, SendChoice::Now);
    assert!(dialog.send_at.is_none());
    assert!(dialog.send_edit.is_none());
    assert_eq!(dialog.send_display("%Y-%m-%d %H:%M"), "now");
}

#[test]
fn focus_starts_on_first_section_and_wraps_through_send_control() {
    let mut dialog = SimplePromptDialog::new();
    // Opens on the first section, not on the send control.
    assert_eq!(dialog.focused_section, 1);
    assert_eq!(dialog.total_focusable(), 2); // send control + instruction_1

    // Down from the last section reaches the send control (visually below).
    dialog.focus_next();
    assert_eq!(dialog.focused_section, 0);
    // Down from the send control wraps to the first section.
    dialog.focus_next();
    assert_eq!(dialog.focused_section, 1);
    // Up from the first section wraps down to the send control.
    dialog.focus_prev();
    assert_eq!(dialog.focused_section, 0);
    // Up from the send control goes to the last section.
    dialog.focus_prev();
    assert_eq!(dialog.focused_section, 1);
}

#[test]
fn focused_section_name_returns_none_for_send_control() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 0; // send control
    assert!(dialog.focused_section_name().is_none());

    dialog.focused_section = 1; // instruction_1
    assert_eq!(dialog.focused_section_name(), Some("instruction_1"));
}

use chrono::{Datelike, NaiveDate};

fn picker_at(dialog: &mut SimplePromptDialog, base: NaiveDate) {
    dialog.send_toggle(); // → date
    dialog.send_begin_edit();
    dialog.send_edit.as_mut().unwrap().value = base.and_hms_opt(10, 30, 0).unwrap();
}

fn type_digits(dialog: &mut SimplePromptDialog, digits: &str) {
    for c in digits.chars() {
        dialog.send_edit_type_digit(c.to_digit(10).unwrap());
    }
}

#[test]
fn typing_four_digits_sets_year_and_auto_advances_to_month() {
    let mut dialog = SimplePromptDialog::new();
    picker_at(&mut dialog, NaiveDate::from_ymd_opt(2000, 5, 15).unwrap());
    type_digits(&mut dialog, "2026");
    let edit = dialog.send_edit.unwrap();
    assert_eq!(edit.value.year(), 2026);
    // Year is 4 digits wide → focus moves on to the month field.
    assert_eq!(edit.field, 1);
}

#[test]
fn typing_two_digits_sets_month_and_advances() {
    let mut dialog = SimplePromptDialog::new();
    picker_at(&mut dialog, NaiveDate::from_ymd_opt(2026, 12, 15).unwrap());
    // Jump straight to the month field, then type "07".
    dialog.send_edit.as_mut().unwrap().field = 1;
    dialog.send_edit.as_mut().unwrap().typed = 0;
    dialog.send_edit.as_mut().unwrap().typed_len = 0;
    type_digits(&mut dialog, "07");
    let edit = dialog.send_edit.unwrap();
    assert_eq!(edit.value.month(), 7);
    assert_eq!(edit.field, 2); // advanced to day
}

#[test]
fn partial_digit_then_arrow_still_adjusts() {
    let mut dialog = SimplePromptDialog::new();
    picker_at(&mut dialog, NaiveDate::from_ymd_opt(2020, 5, 15).unwrap());
    // One digit of the 4-wide year: 2020 → 2 (field not full yet).
    type_digits(&mut dialog, "2");
    assert_eq!(dialog.send_edit.unwrap().value.year(), 2);
    // Arrow up on the year field adds a year — arrows keep working.
    dialog.send_edit_adjust(1);
    assert_eq!(dialog.send_edit.unwrap().value.year(), 3);
    // A digit after the arrow starts a fresh number (accumulator cleared).
    type_digits(&mut dialog, "9");
    assert_eq!(dialog.send_edit.unwrap().value.year(), 9);
}

#[test]
fn invalid_month_thirteen_clamps_to_twelve() {
    // Documented choice: an out-of-range typed component is CLAMPED into
    // its valid range (month 13 → 12), matching the arrow adjust math.
    let mut dialog = SimplePromptDialog::new();
    picker_at(&mut dialog, NaiveDate::from_ymd_opt(2026, 6, 15).unwrap());
    dialog.send_edit.as_mut().unwrap().field = 1;
    dialog.send_edit.as_mut().unwrap().typed = 0;
    dialog.send_edit.as_mut().unwrap().typed_len = 0;
    type_digits(&mut dialog, "13");
    assert_eq!(dialog.send_edit.unwrap().value.month(), 12);
}

#[test]
fn typing_full_date_time_sequence_fills_every_field() {
    let mut dialog = SimplePromptDialog::new();
    picker_at(&mut dialog, NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());
    // year(4) month(2) day(2) hour(2) minute(2), auto-advancing each time.
    type_digits(&mut dialog, "2026"); // year
    type_digits(&mut dialog, "07"); // month
    type_digits(&mut dialog, "20"); // day
    type_digits(&mut dialog, "14"); // hour
    type_digits(&mut dialog, "35"); // minute
    let v = dialog.send_edit.unwrap().value;
    assert_eq!(
        v,
        NaiveDate::from_ymd_opt(2026, 7, 20)
            .unwrap()
            .and_hms_opt(14, 35, 0)
            .unwrap()
    );
}
