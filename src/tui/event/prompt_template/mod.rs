use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::db::Database;
use crate::tui::app::dialog::{AtPicker, SectionPickerMode, SimplePromptDialog};
use crate::tui::app::types::{AgentEntry, App, Focus};

mod section_picker;
use section_picker::*;
mod at_picker;
use at_picker::*;
mod scheduled_sends;
use scheduled_sends::*;

enum PromptAction {
    None,
    Close,
    Send(String),
    /// Schedule the prompt for later delivery at the given local date-time
    /// (picked in the builder's send control, U11).
    ScheduleSend(String, chrono::NaiveDateTime),
    /// Cancel the soonest pending scheduled send for the current target session.
    CancelNextScheduled,
    /// Recall the current project's last prompt (Ctrl+L).
    RecallLastPrompt,
}

pub fn handle_prompt_template_key(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> Result<()> {
    let field_width = prompt_field_width(app);
    let db = app.db.clone();
    let workdir = resolve_picker_workdir(app);

    // Scheduled-sends list interactions (B33) need app-level DB access, so they
    // run before the dialog is borrowed mutably. They never fire while the
    // recall confirm overlay is up — that overlay owns every key.
    let recall_open = app
        .simple_prompt_dialog
        .as_ref()
        .is_some_and(|d| d.pending_recall.is_some());
    if !recall_open {
        let list_focused = app
            .simple_prompt_dialog
            .as_ref()
            .is_some_and(|d| d.scheduled_list_selected.is_some());
        if list_focused {
            handle_scheduled_list_key(app, code, modifiers);
            return Ok(());
        }
        // Ctrl+P moves focus INTO the pending-sends list (no-op when empty).
        if code == KeyCode::Char('p') && modifiers.contains(KeyModifiers::CONTROL) {
            enter_scheduled_list(app);
            return Ok(());
        }
    }

    let Some(dialog) = app.simple_prompt_dialog.as_mut() else {
        app.focus = Focus::Agent;
        return Ok(());
    };

    // Recall confirm overlay intercepts all keys while it's up (standard
    // confirm pattern: y/Enter confirms, n/Esc cancels, anything else is a
    // no-op).
    if dialog.pending_recall.is_some() {
        match code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(last) = dialog.pending_recall.take() {
                    apply_last_prompt(dialog, &last);
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                dialog.pending_recall = None;
            }
            _ => {}
        }
        return Ok(());
    }

    // Ctrl+E and Shift+←/→ toggle between the Normal and Raw tabs from any
    // focus — except while the date-time picker is open, where lateral arrows
    // move between its fields. Entering the Raw tab (re)computes the
    // composed-prompt preview for its empty state.
    let shift_tab_switch = matches!(code, KeyCode::Left | KeyCode::Right)
        && modifiers.contains(KeyModifiers::SHIFT)
        && dialog.send_edit.is_none();
    if (code == KeyCode::Char('e') && modifiers.contains(KeyModifiers::CONTROL)) || shift_tab_switch
    {
        let entering_raw = dialog.active_tab == crate::tui::app::dialog::PromptTab::Normal;
        dialog.toggle_tab();
        if entering_raw {
            dialog.refresh_raw_preview(&db, &workdir);
        }
        return Ok(());
    }

    let action = if dialog.active_tab == crate::tui::app::dialog::PromptTab::Raw {
        handle_raw_tab_key(
            dialog,
            code,
            modifiers,
            field_width,
            &db,
            &workdir,
            app.keyboard_enhancement_active,
        )
    } else {
        if handle_section_picker_key(dialog, &db, &workdir, code)? {
            return Ok(());
        }

        // Determine the focused section name (None when send_at is focused).
        let section_name = focused_section_name(dialog);

        // Only expand collapsed pastes when entering text or doing edit
        // operations (not for navigation keys like arrows, tab, etc.)
        if should_expand_on_key(code, modifiers) {
            if let Some(ref name) = section_name {
                dialog.expand_collapsed_paste(name);
            }
        }

        if let Some(ref name) = section_name {
            if handle_at_picker_key(dialog, code, modifiers, name, field_width) {
                return Ok(());
            }
        }

        handle_dialog_key(
            dialog,
            code,
            modifiers,
            section_name.as_deref(),
            field_width,
            &db,
            &workdir,
            app.keyboard_enhancement_active,
        )?
    };

    match action {
        PromptAction::None => {}
        PromptAction::Close => app.close_simple_prompt_dialog(),
        PromptAction::Send(prompt) => submit_prompt(app, &prompt),
        PromptAction::ScheduleSend(prompt, when) => {
            schedule_send_prompt(app, &prompt, when);
        }
        PromptAction::CancelNextScheduled => cancel_scheduled_send(app),
        PromptAction::RecallLastPrompt => recall_last_prompt(app),
    }

    Ok(())
}

pub(crate) fn prompt_field_width(app: &App) -> usize {
    // Must mirror the render calculation in ui/dialogs/simple_prompt.rs exactly,
    // including the clamp to terminal width, or cursor/scroll math drifts from
    // what is drawn on narrow terminals.
    let term_width = app.term_width;
    let max_dialog_w = term_width.saturating_sub(2).max(1);
    let preferred_dialog_w = term_width.saturating_mul(65) / 100;
    let min_dialog_w = 40u16.min(max_dialog_w);
    let dialog_width = preferred_dialog_w.clamp(min_dialog_w, max_dialog_w);
    (dialog_width.saturating_sub(4) as usize).max(10)
}

fn resolve_picker_workdir(app: &App) -> PathBuf {
    app.selected_agent()
        .and_then(|agent| match agent {
            AgentEntry::Interactive(idx) => app
                .interactive_agents
                .get(*idx)
                .map(|interactive| PathBuf::from(&interactive.working_dir)),
            _ => None,
        })
        .unwrap_or_else(|| app.data_dir.parent().unwrap_or(&app.data_dir).to_path_buf())
}

fn focused_section_name(dialog: &mut SimplePromptDialog) -> Option<String> {
    // Focus index 0 is the send control — it is a real focus target, not a
    // section. The pre-U11 version of this function clamped focus 0 back to
    // section 1 on every keypress, which made the send control impossible
    // to operate (its advertised keys never fired).
    if dialog.focused_section == 0 {
        return None;
    }
    let last_index = dialog.enabled_sections.len().checked_sub(1)?;

    // Clamp a stale over-the-end focus (e.g. after removing a section).
    let section_idx = (dialog.focused_section - 1).min(last_index);
    dialog.focused_section = section_idx + 1; // keep the offset
    dialog.enabled_sections.get(section_idx).cloned()
}

/// Determine if a key press should trigger expansion of a collapsed paste.
/// Only text input and specific edit operations should expand; navigation keys should not.
fn should_expand_on_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    match code {
        // Text input characters should expand
        KeyCode::Char(_) => true,
        // Backspace and Delete should expand (to handle edit operations)
        KeyCode::Backspace | KeyCode::Delete => true,
        // Enter should expand for instruction sections
        KeyCode::Enter => !modifiers.is_empty(),
        // Navigation keys should NOT expand
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => false,
        KeyCode::Tab | KeyCode::BackTab => false,
        // Everything else: don't expand
        _ => false,
    }
}

/// The send control (focus index 0) key handling, shared by both tabs. When
/// the inline date-time picker is open, arrows edit and Enter confirms;
/// otherwise the selector toggles now/date and navigates back into the fields.
fn handle_send_control_key(
    dialog: &mut SimplePromptDialog,
    code: KeyCode,
    modifiers: KeyModifiers,
    is_shift: bool,
    db: &Database,
    workdir: &Path,
) -> PromptAction {
    // Inline date-time picker open: arrows edit, Enter confirms, Esc abandons.
    if dialog.send_edit.is_some() {
        return match code {
            KeyCode::Esc => {
                dialog.send_edit_cancel();
                PromptAction::None
            }
            // Ctrl+S sends without leaving the picker: the in-progress edit is
            // confirmed first; an invalid time keeps the picker open showing
            // its error instead of sending.
            KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
                dialog.send_edit_confirm();
                if dialog.send_edit.is_some() {
                    PromptAction::None
                } else {
                    build_prompt_action(dialog, db, workdir)
                }
            }
            KeyCode::Up => {
                dialog.send_edit_adjust(1);
                PromptAction::None
            }
            KeyCode::Down => {
                dialog.send_edit_adjust(-1);
                PromptAction::None
            }
            KeyCode::Left | KeyCode::BackTab => {
                dialog.send_edit_move(-1);
                PromptAction::None
            }
            KeyCode::Right | KeyCode::Tab => {
                dialog.send_edit_move(1);
                PromptAction::None
            }
            KeyCode::Enter => {
                dialog.send_edit_confirm();
                PromptAction::None
            }
            KeyCode::Backspace => {
                dialog.clear_send_at();
                PromptAction::None
            }
            // Typed digits write the focused field directly and auto-advance
            // when it fills (U11); arrows keep working.
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(digit) = c.to_digit(10) {
                    dialog.send_edit_type_digit(digit);
                }
                PromptAction::None
            }
            _ => PromptAction::None,
        };
    }

    // Selector: lateral arrows toggle now ↔ date, Enter opens the picker on
    // date, Shift+↑/↓ and Tab return to the fields above.
    match code {
        KeyCode::Esc => PromptAction::Close,
        // Ctrl+S sends from the send control too — it must work from every
        // focus, not force the user back into a section first.
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            build_prompt_action(dialog, db, workdir)
        }
        KeyCode::Left | KeyCode::Right => {
            dialog.send_toggle();
            PromptAction::None
        }
        KeyCode::Enter => {
            if dialog.send_choice == crate::tui::app::dialog::SendChoice::Date {
                dialog.send_begin_edit();
            }
            PromptAction::None
        }
        KeyCode::Backspace => {
            dialog.clear_send_at();
            PromptAction::None
        }
        KeyCode::Char('k') if modifiers.contains(KeyModifiers::CONTROL) => {
            PromptAction::CancelNextScheduled
        }
        KeyCode::Up if is_shift => {
            dialog.focus_prev();
            PromptAction::None
        }
        KeyCode::Down if is_shift => {
            dialog.focus_next();
            PromptAction::None
        }
        KeyCode::Tab => {
            dialog.focus_next();
            PromptAction::None
        }
        KeyCode::BackTab => {
            // wrap to last section
            dialog.focused_section = dialog.total_focusable() - 1;
            PromptAction::None
        }
        _ => PromptAction::None,
    }
}

/// Key handling for the Raw tab. Two focus targets only: the raw buffer
/// (focus index ≥ 1) and the send control (focus index 0). No sections,
/// pickers, or @-resources — just a plain multi-line text field plus the
/// shared send control. Sending resolves through `resolve_outgoing_prompt`,
/// so a non-empty buffer is sent verbatim and an empty one sends the composed
/// Normal-form prompt.
#[allow(clippy::too_many_arguments)]
fn handle_raw_tab_key(
    dialog: &mut SimplePromptDialog,
    code: KeyCode,
    modifiers: KeyModifiers,
    field_width: usize,
    db: &Database,
    workdir: &Path,
    keyboard_enhancement: bool,
) -> PromptAction {
    let is_shift = modifiers.contains(KeyModifiers::SHIFT);

    // Send control focus. After delegating, clamp focus back into {0, 1}
    // because the raw tab only has those two stops (Normal-tab navigation may
    // have parked the shared handler on a higher section index).
    if dialog.focused_section == 0 {
        let action = handle_send_control_key(dialog, code, modifiers, is_shift, db, workdir);
        if dialog.focused_section > 1 {
            dialog.focused_section = 1;
        }
        return action;
    }

    let raw = crate::tui::app::dialog::RAW_SECTION_ID;
    match code {
        KeyCode::Esc => PromptAction::Close,

        // Ctrl+S always sends; Shift+Enter sends when the terminal can report it.
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            build_prompt_action(dialog, db, workdir)
        }
        KeyCode::Enter if should_send_on_shift_enter(keyboard_enhancement, is_shift) => {
            build_prompt_action(dialog, db, workdir)
        }
        // Plain Enter inserts a newline — the raw field is multi-line.
        KeyCode::Enter if modifiers.is_empty() => {
            dialog.insert_newline_at_cursor(raw, field_width);
            PromptAction::None
        }

        // Navigation: move to the send control (the only other stop).
        KeyCode::Tab | KeyCode::BackTab => {
            dialog.focused_section = 0;
            PromptAction::None
        }
        KeyCode::Up if is_shift => {
            dialog.focused_section = 0;
            PromptAction::None
        }
        KeyCode::Down if is_shift => {
            dialog.focused_section = 0;
            PromptAction::None
        }

        // With an empty buffer the tab shows the read-only preview: vertical
        // keys scroll it instead of moving a cursor there is nothing to edit.
        KeyCode::Up if dialog.raw_is_empty() => {
            dialog.scroll_raw_preview(-1);
            PromptAction::None
        }
        KeyCode::Down if dialog.raw_is_empty() => {
            dialog.scroll_raw_preview(1);
            PromptAction::None
        }
        KeyCode::PageUp if dialog.raw_is_empty() => {
            dialog.scroll_raw_preview(-10);
            PromptAction::None
        }
        KeyCode::PageDown if dialog.raw_is_empty() => {
            dialog.scroll_raw_preview(10);
            PromptAction::None
        }

        // Cursor movement inside the raw buffer.
        KeyCode::Left => {
            dialog.move_cursor_left(raw, field_width);
            dialog.raw_edit_scroll = None;
            PromptAction::None
        }
        KeyCode::Right => {
            dialog.move_cursor_right(raw, field_width);
            dialog.raw_edit_scroll = None;
            PromptAction::None
        }
        KeyCode::Up => {
            dialog.move_cursor_up(raw, field_width);
            dialog.raw_edit_scroll = None;
            PromptAction::None
        }
        KeyCode::Down => {
            dialog.move_cursor_down(raw, field_width);
            dialog.raw_edit_scroll = None;
            PromptAction::None
        }

        // Text input.
        KeyCode::Char(c) => {
            if let Some(ch) = normalize_prompt_char_input(c, modifiers) {
                dialog.insert_char_at_cursor(raw, ch, field_width);
            }
            PromptAction::None
        }
        KeyCode::Backspace => {
            dialog.backspace_at_cursor(raw, field_width);
            PromptAction::None
        }
        _ => PromptAction::None,
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_dialog_key(
    dialog: &mut SimplePromptDialog,
    code: KeyCode,
    modifiers: KeyModifiers,
    section_name: Option<&str>,
    field_width: usize,
    db: &Database,
    workdir: &Path,
    keyboard_enhancement: bool,
) -> Result<PromptAction> {
    // Ctrl+L recalls the project's last prompt regardless of which field is
    // currently focused.
    if code == KeyCode::Char('l') && modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(PromptAction::RecallLastPrompt);
    }

    let is_shift = modifiers.contains(KeyModifiers::SHIFT);
    let is_send_at = dialog.focused_section == 0 && !dialog.enabled_sections.is_empty();

    // ── send control (focus index 0, U11) ───────────────────────────────
    if is_send_at {
        return Ok(handle_send_control_key(
            dialog, code, modifiers, is_shift, db, workdir,
        ));
    }

    // ── Section field keys ──────────────────────────────────────────────
    let section_name = section_name.unwrap_or("");

    match code {
        KeyCode::Esc => Ok(PromptAction::Close),

        // Ctrl+S always works as send (legacy fallback).
        KeyCode::Char('s') if modifiers.contains(KeyModifiers::CONTROL) => {
            Ok(build_prompt_action(dialog, db, workdir))
        }

        // Shift+Enter sends when keyboard enhancement is active.
        KeyCode::Enter if should_send_on_shift_enter(keyboard_enhancement, is_shift) => {
            Ok(build_prompt_action(dialog, db, workdir))
        }

        // Plain Enter: always a newline in text fields; sending is Ctrl+S.
        KeyCode::Enter => handle_enter_key(dialog, modifiers, section_name, field_width),

        // Navigation: Tab / Shift+Tab / Shift+Up/Down move between fields in
        // visual order, wrapping through the send control at the bottom.
        KeyCode::Tab => {
            dialog.focus_next();
            Ok(PromptAction::None)
        }
        KeyCode::BackTab => {
            dialog.focus_prev();
            Ok(PromptAction::None)
        }
        KeyCode::Up if is_shift => {
            dialog.focus_prev();
            Ok(PromptAction::None)
        }
        KeyCode::Down if is_shift => {
            dialog.focus_next();
            Ok(PromptAction::None)
        }

        // Cursor movement within the section.
        KeyCode::Left => {
            if let Some(name) = dialog.focused_section_name().map(str::to_string) {
                dialog.move_cursor_left(&name, field_width);
            }
            Ok(PromptAction::None)
        }
        KeyCode::Right => {
            if let Some(name) = dialog.focused_section_name().map(str::to_string) {
                dialog.move_cursor_right(&name, field_width);
            }
            Ok(PromptAction::None)
        }
        KeyCode::Up => {
            if let Some(name) = dialog.focused_section_name().map(str::to_string) {
                dialog.move_cursor_up(&name, field_width);
            }
            Ok(PromptAction::None)
        }
        KeyCode::Down => {
            if let Some(name) = dialog.focused_section_name().map(str::to_string) {
                dialog.move_cursor_down(&name, field_width);
            }
            Ok(PromptAction::None)
        }

        // Section management.
        KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
            open_add_section_picker_if_available(dialog);
            Ok(PromptAction::None)
        }
        KeyCode::Char('x') if modifiers.contains(KeyModifiers::CONTROL) => {
            open_remove_section_picker_if_available(dialog);
            Ok(PromptAction::None)
        }

        // Text input.
        KeyCode::Char(c) => {
            if let Some(name) = dialog.focused_section_name().map(str::to_string) {
                if dialog.is_locked(&name) {
                    return Ok(PromptAction::None);
                }
                if let Some(ch) = normalize_prompt_char_input(c, modifiers) {
                    handle_section_char_input(dialog, &name, ch, field_width, workdir);
                }
            }
            Ok(PromptAction::None)
        }
        KeyCode::Backspace => {
            if let Some(name) = dialog.focused_section_name().map(str::to_string) {
                if dialog.is_locked(&name) {
                    return Ok(PromptAction::None);
                }
                handle_section_backspace(dialog, &name, field_width);
            }
            Ok(PromptAction::None)
        }
        _ => Ok(PromptAction::None),
    }
}

/// Whether Enter should act as "send" rather than a plain keypress.
/// Most terminals cannot distinguish Shift+Enter from plain Enter without
/// the Kitty keyboard enhancement protocol, so this only fires once that
/// protocol is confirmed active (see `run_tui`'s startup capability probe);
/// otherwise Ctrl+S remains the fallback send key regardless of Shift state.
fn should_send_on_shift_enter(keyboard_enhancement: bool, is_shift: bool) -> bool {
    keyboard_enhancement && is_shift
}

fn normalize_prompt_char_input(c: char, modifiers: KeyModifiers) -> Option<char> {
    if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT {
        return Some(c);
    }

    // AltGr is typically reported as Ctrl+Alt; on some layouts crossterm surfaces
    // the base key (for example 'q'/'2') instead of the produced character.
    if modifiers.contains(KeyModifiers::CONTROL) && modifiers.contains(KeyModifiers::ALT) {
        return Some(normalize_altgr_char(c));
    }

    None
}

fn normalize_altgr_char(c: char) -> char {
    match c {
        'q' | 'Q' | '2' => '@',
        _ => c,
    }
}

fn handle_enter_key(
    dialog: &mut SimplePromptDialog,
    modifiers: KeyModifiers,
    section_name: &str,
    field_width: usize,
) -> Result<PromptAction> {
    if !modifiers.is_empty() {
        return Ok(PromptAction::None);
    }

    dialog.insert_newline_at_cursor(section_name, field_width);
    Ok(PromptAction::None)
}

fn build_prompt_action(dialog: &SimplePromptDialog, db: &Database, workdir: &Path) -> PromptAction {
    // Raw tab with a non-empty buffer sends the text verbatim; otherwise the
    // composed Normal-form prompt (see `resolve_outgoing_prompt`).
    let Ok(prompt) = dialog.resolve_outgoing_prompt(db, workdir) else {
        return PromptAction::None;
    };

    match (dialog.send_choice, dialog.send_at) {
        (crate::tui::app::dialog::SendChoice::Date, Some(when)) => {
            PromptAction::ScheduleSend(prompt, when)
        }
        _ => PromptAction::Send(prompt),
    }
}

fn submit_prompt(app: &mut App, prompt: &str) {
    // Capture whether this send carries the session-start protocol block
    // before discarding the dialog — the automatic first turn OR the manual
    // `Canopy protocol` section; a verbatim Raw send carries neither.
    let sends_protocol = app
        .simple_prompt_dialog
        .as_ref()
        .is_some_and(|d| d.sends_protocol());
    let workdir = app.current_workdir();
    let session_key = app.current_prompt_session_key();

    persist_last_prompt(app, &workdir, prompt);
    write_prompt_to_selected_agent(app, prompt);
    app.prompt_builder_sessions.remove(&session_key);
    app.discard_simple_prompt_dialog();

    // Record that the protocol block was sent for this session, so later
    // turns of the same session omit it while still getting per-turn context.
    if sends_protocol {
        record_protocol_sent(app, &session_key);
    }
}

/// Record that a send carried the protocol block for `session_key`: always
/// in memory (project/workdir keys keep that behaviour alone), and — when
/// the key names an interactive session — also persisted as
/// `prompt_protocol_sent:<session id>` so the fact survives restarts and
/// resumes (FR1).
fn record_protocol_sent(app: &mut App, session_key: &str) {
    let session_id = session_key.strip_prefix("interactive:").map(str::to_string);
    app.session_protocol_state
        .entry(session_key.to_string())
        .or_default()
        .protocol_sent = true;
    if let Some(session_id) = session_id {
        app.persist_protocol_sent(&session_id);
    }
}

fn write_prompt_to_selected_agent(app: &mut App, prompt: &str) {
    let Some(idx) = selected_interactive_index(app) else {
        return;
    };
    let Some(agent) = app.interactive_agents.get_mut(idx) else {
        return;
    };

    // Register the prompt in the session history so context transfer can
    // pair it with the agent's response — builder prompts are the main
    // conversation driver, not just direct typing.
    agent.record_prompt(prompt);

    // Delivery must mean submission: the paste and the submit keystroke are
    // written as separate events (see `write_submitted_prompt`), with any
    // per-platform override (delay/key/extra presses) coming from the CLI
    // registry rather than being hardcoded here.
    let spec = agent.cli.paste_submit_spec();
    let _ = agent.submit_prompt_to_pty(prompt, spec);
}

fn selected_interactive_index(app: &App) -> Option<usize> {
    match app.selected_agent() {
        Some(AgentEntry::Interactive(idx)) => Some(*idx),
        _ => None,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod preset_picker_tests;

#[cfg(test)]
mod recall_last_prompt_tests;

#[cfg(test)]
mod session_protocol_tests;

#[cfg(test)]
mod keyboard_capability_tests;

#[cfg(test)]
mod send_at_tests;

#[cfg(test)]
mod should_expand_tests;

#[cfg(test)]
mod focused_section_name_tests;

#[cfg(test)]
mod normalize_altgr_char_tests;

#[cfg(test)]
mod picker_navigation_tests;

#[cfg(test)]
mod utility_function_tests;

#[cfg(test)]
mod enter_key_tests;
