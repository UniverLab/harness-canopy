use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use crate::tui::app::dialog::{BackgroundTrigger, DialogStep, NewAgentDialog, NewTaskType};
use crate::tui::app::types::App;
use crate::tui::ui::dialogs::new_agent_dialog::SESSION_RESUME_PICKER_VISIBLE;

mod prompt_text;

use prompt_text::handle_prompt_field;

use crate::tui::app::dialog::new_agent::mode_step;

pub(crate) use prompt_text::insert_prompt_text;
#[cfg(test)]
pub(crate) use prompt_text::prompt_field_width_for;
#[cfg(test)]
use prompt_text::{
    backspace_prompt, char_to_byte_index, delete_prompt_forward, end_of_visual_line,
    end_of_visual_line_at, move_prompt_visual, start_of_visual_line, start_of_visual_line_at,
    visual_line_of_char,
};

// ── Dialog: new agent creation ──────────────────────────────────────
//
// Flow: ↑↓ switch fields, ←→ choose CLI/type, ↑↓ in dir browser,
//       Space enter directory, Enter confirm — an Interactive create form
//       then opens the Mode step (New / Resume), Esc cancel.

/// Raw row count fetched from the DB before the Mode step filters it down
/// to the sessions that apply to the chosen harness and directory (FR 4).
const RESUME_CANDIDATE_FETCH_LIMIT: usize = 200;

pub fn handle_dialog_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> Result<()> {
    // The Mode step owns every key until it hands control back (FR 2).
    if app
        .new_agent_dialog
        .as_ref()
        .is_some_and(|d| matches!(d.step, DialogStep::Mode))
    {
        return crate::tui::event::new_agent_mode_step::handle_mode_step_key(app, code);
    }

    {
        let Some(dialog) = app.new_agent_dialog.as_mut() else {
            return Ok(());
        };

        if handle_picker_key(dialog, code) {
            return Ok(());
        }
    }

    match code {
        KeyCode::Esc => app.close_new_agent_dialog(),
        KeyCode::Enter if !modifiers.contains(KeyModifiers::SHIFT) => handle_dialog_enter(app),
        _ => {
            let term_width = app.term_width;
            let Some(dialog) = app.new_agent_dialog.as_mut() else {
                return Ok(());
            };
            handle_dialog_field_key(term_width, dialog, code, modifiers);
        }
    }

    Ok(())
}

#[derive(Clone, Copy)]
struct DialogFields {
    is_interactive: bool,
    is_terminal: bool,
    is_background: bool,
    cli_field: usize,
    identity_field: usize,
    model_field: usize,
    prompt_field: usize,
    extra_field: usize,
    dir_field: usize,
    yolo_field: usize,
    sandbox_field: usize,
}

impl DialogFields {
    fn from(dialog: &NewAgentDialog) -> Self {
        let is_interactive = matches!(dialog.task_type, NewTaskType::Interactive);
        let is_terminal = matches!(dialog.task_type, NewTaskType::Terminal);
        let is_background = matches!(dialog.task_type, NewTaskType::Background);

        // Interactive (CT34: the Mode row is gone, its step follows the form):
        // 0 Type · 1 Harness · 2 Identity · 3 Yolo · 4 Sandbox · 5 Dir.
        // Terminal/Background keep their own numbering.
        Self {
            is_interactive,
            is_terminal,
            is_background,
            cli_field: if is_interactive {
                1
            } else if is_background {
                2
            } else {
                0
            },
            identity_field: if is_interactive { 2 } else { 0 },
            model_field: 3,
            prompt_field: 4,
            extra_field: 5,
            dir_field: if is_interactive {
                5
            } else if is_terminal {
                1
            } else {
                6
            },
            yolo_field: if is_interactive { 3 } else { 4 },
            sandbox_field: if is_interactive { 4 } else { 0 },
        }
    }

    fn is_watch_dir_field(self, dialog: &NewAgentDialog) -> bool {
        self.is_background
            && dialog.field == self.extra_field
            && matches!(dialog.background_trigger, BackgroundTrigger::Watch)
    }

    fn cli_up_target(self) -> usize {
        // Interactive: Up from the harness row lands on Type (there is no
        // Mode row above it any more); Background still lands on Trigger.
        if self.is_background {
            1
        } else {
            0
        }
    }

    /// The field Up from the prompt lands on. In send mode (CM53) the model row
    /// is not rendered, so it is the target picker on the CLI slot instead.
    fn prompt_up_target(self, dialog: &NewAgentDialog) -> usize {
        if self.is_background && dialog.is_send_to_session() {
            self.cli_field
        } else {
            self.model_field
        }
    }

    fn previous_dir_field(self, is_watch_dir: bool) -> usize {
        if is_watch_dir {
            self.prompt_field
        } else if self.is_interactive {
            self.sandbox_field
        } else if self.is_terminal {
            0
        } else {
            self.extra_field
        }
    }

    fn next_dir_field(self, current_field: usize) -> usize {
        if self.is_interactive {
            self.sandbox_field
        } else if self.is_terminal {
            2
        } else {
            current_field
        }
    }
}

fn handle_picker_key(dialog: &mut NewAgentDialog, code: KeyCode) -> bool {
    if dialog.session_resume_picker.is_some() {
        handle_session_resume_picker_key(dialog, code);
        return true;
    }

    if dialog.session_picker_open {
        handle_session_picker_key(dialog, code);
        return true;
    }

    if dialog.cli_picker_open {
        handle_cli_picker_key(dialog, code);
        return true;
    }

    false
}

/// Key handling for the canopy-native session-resume picker (FR 1-4, 7).
/// Shared with the Mode step (CT34), which now opens the list.
pub(crate) fn handle_session_resume_picker_key(dialog: &mut NewAgentDialog, code: KeyCode) {
    match code {
        KeyCode::Down => move_session_resume_picker(dialog, true),
        KeyCode::Up => move_session_resume_picker(dialog, false),
        KeyCode::Enter => confirm_session_resume_picker(dialog),
        // Cancelling starts nothing and returns to the dialog as it was
        // before the picker opened — the picker just closes.
        KeyCode::Esc | KeyCode::Backspace => dialog.session_resume_picker = None,
        _ => {}
    }
}

fn move_session_resume_picker(dialog: &mut NewAgentDialog, forward: bool) {
    if let Some(picker) = dialog.session_resume_picker.as_mut() {
        picker.move_selection(forward, SESSION_RESUME_PICKER_VISIBLE);
    }
}

/// Confirm the native resume picker's highlighted session and mark the
/// dialog as resuming (called by the Mode step on Enter).
pub(crate) fn confirm_session_resume_picker(dialog: &mut NewAgentDialog) {
    let Some(picker) = dialog.session_resume_picker.take() else {
        return;
    };
    if let Some(session) = picker.selected().cloned() {
        dialog.apply_resume_choice(session);
    }
}

/// Key handling for the CLI's own session list (`session_list_cmd`), opened
/// from the Mode step's Resume branch (CT34).
pub(crate) fn handle_session_picker_key(dialog: &mut NewAgentDialog, code: KeyCode) {
    match code {
        KeyCode::Down => move_session_picker(dialog, true),
        KeyCode::Up => move_session_picker(dialog, false),
        KeyCode::Enter => dialog.confirm_session_pick(),
        KeyCode::Esc | KeyCode::Backspace => dialog.session_picker_open = false,
        _ => {}
    }
}

fn move_session_picker(dialog: &mut NewAgentDialog, forward: bool) {
    let next = crate::tui::selection::move_index(
        dialog.session_picker_idx,
        dialog.session_entries.len(),
        forward,
    );
    dialog.session_picker_idx = next;
}

fn handle_cli_picker_key(dialog: &mut NewAgentDialog, code: KeyCode) {
    match code {
        KeyCode::Down => dialog.move_cli_picker_next(),
        KeyCode::Up => dialog.move_cli_picker_prev(),
        KeyCode::Enter => confirm_cli_picker(dialog),
        KeyCode::Esc => dialog.close_cli_picker(),
        KeyCode::Backspace => dialog.pop_cli_picker_filter(),
        KeyCode::Char(c) => dialog.push_cli_picker_filter(c),
        _ => {}
    }
}

fn confirm_cli_picker(dialog: &mut NewAgentDialog) {
    let filtered = dialog.filtered_cli_indices();
    let Some(&idx) = filtered.get(dialog.cli_picker_idx) else {
        dialog.close_cli_picker();
        return;
    };

    dialog.set_cli_index(idx);
    dialog.close_cli_picker();
}

fn handle_dialog_enter(app: &mut App) {
    // An Interactive create-mode form confirms into the Mode step (New or
    // Resume) instead of launching; the resumable sessions are fetched once,
    // here, so the Resume row already knows what it can offer (FR 2, 4).
    let opens_mode_step = app
        .new_agent_dialog
        .as_ref()
        .is_some_and(mode_step::can_open_mode_step);
    if opens_mode_step {
        let sessions = app
            .db
            .get_resumable_sessions(RESUME_CANDIDATE_FETCH_LIMIT)
            .unwrap_or_default();
        if let Some(dialog) = app.new_agent_dialog.as_mut() {
            mode_step::open_mode_step(dialog, sessions);
        }
        return;
    }

    let _ = app.launch_new_agent();
}

fn handle_dialog_field_key(
    term_width: u16,
    dialog: &mut NewAgentDialog,
    code: KeyCode,
    modifiers: KeyModifiers,
) {
    let fields = DialogFields::from(dialog);

    match dialog.field {
        0 => handle_type_field(dialog, code),
        1 if fields.is_background => handle_trigger_field(dialog, code, fields),
        n if n == fields.cli_field && !fields.is_terminal => handle_cli_field(dialog, code, fields),
        n if n == fields.identity_field && fields.is_interactive => {
            handle_identity_field(dialog, code, fields);
        }
        n if n == fields.model_field && fields.is_background => {
            handle_model_field(dialog, code, fields);
        }
        4 if fields.is_background => {
            handle_prompt_field(term_width, dialog, code, fields, modifiers);
        }
        5 if fields.is_background
            && matches!(dialog.background_trigger, BackgroundTrigger::Cron) =>
        {
            handle_cron_field(dialog, code, fields);
        }
        n if n == fields.dir_field
            || (n == fields.extra_field && fields.is_watch_dir_field(dialog)) =>
        {
            handle_directory_field(dialog, code, fields, fields.is_watch_dir_field(dialog));
        }
        2 if fields.is_terminal => handle_shell_field(dialog, code, fields),
        n if n == fields.yolo_field && fields.is_interactive => {
            handle_yolo_field(dialog, code, fields);
        }
        n if n == fields.sandbox_field && fields.is_interactive => {
            handle_sandbox_field(dialog, code, fields);
        }
        _ => {}
    }
}

fn handle_type_field(dialog: &mut NewAgentDialog, code: KeyCode) {
    match code {
        KeyCode::Left => cycle_task_type(dialog, false),
        KeyCode::Right => cycle_task_type(dialog, true),
        // Field 1 is the first row below Type for every type: harness
        // (Interactive), trigger (Background), directory (Terminal).
        KeyCode::Down | KeyCode::Tab => dialog.field = 1,
        _ => {}
    }
}

fn cycle_task_type(dialog: &mut NewAgentDialog, forward: bool) {
    dialog.task_type = match (dialog.task_type, forward) {
        (NewTaskType::Interactive, true) => NewTaskType::Terminal,
        (NewTaskType::Terminal, true) => NewTaskType::Background,
        (NewTaskType::Background, true) => NewTaskType::Interactive,
        (NewTaskType::Interactive, false) => NewTaskType::Background,
        (NewTaskType::Terminal, false) => NewTaskType::Interactive,
        (NewTaskType::Background, false) => NewTaskType::Terminal,
    };
    dialog.field = 0;
    dialog.refresh_dir_entries();
}

fn handle_trigger_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        // CM53: Space toggles headless vs send-to-session. Edit mode keeps the
        // existing action, so the toggle is inert there.
        KeyCode::Char(' ') if !dialog.is_edit_mode() => {
            dialog.background_action = dialog.background_action.next();
        }
        KeyCode::Left | KeyCode::Right => toggle_background_trigger(dialog),
        KeyCode::Down | KeyCode::Tab => dialog.field = fields.cli_field,
        KeyCode::Up | KeyCode::BackTab => dialog.field = 0,
        _ => {}
    }
}

fn toggle_background_trigger(dialog: &mut NewAgentDialog) {
    dialog.background_trigger = match dialog.background_trigger {
        BackgroundTrigger::Cron => BackgroundTrigger::Watch,
        BackgroundTrigger::Watch => BackgroundTrigger::Cron,
    };
    dialog.refresh_dir_entries();
}

fn handle_cli_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    // CM53: in send mode this row is the target-session picker, not the harness.
    // The model row is not rendered in send mode, so navigation steps over it
    // straight to the prompt.
    if dialog.is_send_to_session() {
        match code {
            KeyCode::Left => dialog.move_send_target(false),
            KeyCode::Right => dialog.move_send_target(true),
            KeyCode::Down | KeyCode::Tab => dialog.field = fields.prompt_field,
            KeyCode::Up | KeyCode::BackTab => dialog.field = fields.cli_up_target(),
            _ => {}
        }
        return;
    }
    match code {
        KeyCode::Char(' ') => dialog.open_cli_picker(),
        KeyCode::Char(c) => {
            dialog.open_cli_picker();
            dialog.push_cli_picker_filter(c);
        }
        KeyCode::Left | KeyCode::Right => {
            step_cli_selection(dialog, matches!(code, KeyCode::Right));
        }
        KeyCode::Down => {
            dialog.field = if fields.is_interactive {
                fields.identity_field
            } else {
                fields.model_field
            };
        }
        KeyCode::Up => dialog.field = fields.cli_up_target(),
        _ => {}
    }
}

fn step_cli_selection(dialog: &mut NewAgentDialog, forward: bool) {
    let next =
        crate::tui::selection::move_index(dialog.cli_index, dialog.available_clis.len(), forward);
    dialog.set_cli_index(next);
}

fn handle_model_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        KeyCode::Char(' ') => reopen_model_picker(dialog, true),
        KeyCode::Char(c) => {
            dialog.model.push(c);
            reopen_model_picker(dialog, true);
        }
        KeyCode::Backspace => {
            dialog.model.pop();
            reopen_model_picker(dialog, !dialog.model.is_empty());
        }
        KeyCode::Down if dialog.model_picker_open => move_model_picker(dialog, true),
        KeyCode::Up if dialog.model_picker_open => move_model_picker(dialog, false),
        KeyCode::Enter if dialog.model_picker_open => confirm_model_picker(dialog),
        KeyCode::Esc | KeyCode::Left if dialog.model_picker_open => {
            dialog.model_picker_open = false;
        }
        KeyCode::Up => {
            dialog.model_picker_open = false;
            dialog.field = fields.cli_field;
        }
        KeyCode::Down => {
            dialog.model_picker_open = false;
            dialog.field = fields.prompt_field;
        }
        _ => {}
    }
}

fn reopen_model_picker(dialog: &mut NewAgentDialog, open: bool) {
    dialog.model_picker_open = open;
    dialog.model_suggestion_idx = 0;
    dialog.refresh_model_suggestions();
}

fn move_model_picker(dialog: &mut NewAgentDialog, forward: bool) {
    let next = crate::tui::selection::move_index(
        dialog.model_suggestion_idx,
        dialog.model_suggestions.len(),
        forward,
    );
    dialog.model_suggestion_idx = next;
}

fn confirm_model_picker(dialog: &mut NewAgentDialog) {
    dialog.accept_model_suggestion();
    dialog.model_picker_open = false;
}

fn handle_cron_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        KeyCode::Char(c) => dialog.cron_expr.push(c),
        KeyCode::Backspace => {
            dialog.cron_expr.pop();
        }
        KeyCode::Up => dialog.field = fields.prompt_field,
        KeyCode::Down => dialog.field = fields.dir_field,
        _ => {}
    }
}

fn handle_directory_field(
    dialog: &mut NewAgentDialog,
    code: KeyCode,
    fields: DialogFields,
    is_watch_dir: bool,
) {
    match code {
        KeyCode::Up => move_directory_up(dialog, fields, is_watch_dir),
        KeyCode::Down => move_directory_down(dialog),
        KeyCode::BackTab => dialog.field = fields.previous_dir_field(is_watch_dir),
        KeyCode::Tab => dialog.field = fields.next_dir_field(dialog.field),
        KeyCode::Right => dialog.navigate_to_selected(),
        KeyCode::Left => dialog.go_up(),
        KeyCode::Backspace if !dialog.dir_filter.is_empty() => {
            dialog.dir_filter.pop();
            dialog.dir_selected = 0;
        }
        KeyCode::Char(c) => {
            dialog.dir_filter.push(c);
            dialog.dir_selected = 0;
        }
        _ => {}
    }
}

fn move_directory_up(dialog: &mut NewAgentDialog, fields: DialogFields, is_watch_dir: bool) {
    if dialog.dir_selected == 0 {
        dialog.field = fields.previous_dir_field(is_watch_dir);
        return;
    }
    dialog.dir_selected = crate::tui::selection::move_index(
        dialog.dir_selected,
        dialog.filtered_dir_entries().len(),
        false,
    );
    dialog.update_dir_preview();
}

fn move_directory_down(dialog: &mut NewAgentDialog) {
    let filtered_len = dialog.filtered_dir_entries().len();
    if filtered_len == 0 {
        return;
    }
    dialog.dir_selected =
        crate::tui::selection::move_index(dialog.dir_selected, filtered_len, true);
    dialog.update_dir_preview();
}

fn handle_shell_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        KeyCode::Left | KeyCode::Right => {
            step_shell_selection(dialog, matches!(code, KeyCode::Right));
        }
        KeyCode::Up | KeyCode::BackTab => dialog.field = fields.dir_field,
        _ => {}
    }
}

fn step_shell_selection(dialog: &mut NewAgentDialog, forward: bool) {
    let next = crate::tui::selection::move_index(
        dialog.shell_index,
        dialog.available_shells.len(),
        forward,
    );
    dialog.shell_index = next;
}

fn handle_yolo_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        KeyCode::Char(' ') if dialog.selected_yolo_flag().is_some() => {
            dialog.yolo_mode = !dialog.yolo_mode;
        }
        KeyCode::Char(' ') => {}
        KeyCode::Up | KeyCode::BackTab => dialog.field = fields.identity_field,
        KeyCode::Down | KeyCode::Tab => dialog.field = fields.sandbox_field,
        _ => {}
    }
}

fn handle_sandbox_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        KeyCode::Char(' ') => {
            dialog.sandbox_mode = !dialog.sandbox_mode;
        }
        KeyCode::Up | KeyCode::BackTab => dialog.field = fields.yolo_field,
        KeyCode::Down | KeyCode::Tab => dialog.field = fields.dir_field,
        _ => {}
    }
}

fn handle_identity_field(dialog: &mut NewAgentDialog, code: KeyCode, fields: DialogFields) {
    match code {
        KeyCode::Left => step_seed_selection(dialog, false),
        KeyCode::Right => step_seed_selection(dialog, true),
        KeyCode::Up | KeyCode::BackTab => dialog.field = fields.cli_field,
        KeyCode::Down | KeyCode::Tab => dialog.field = fields.yolo_field,
        _ => {}
    }
}

fn step_seed_selection(dialog: &mut NewAgentDialog, forward: bool) {
    let next =
        crate::tui::selection::move_index(dialog.seed_index, dialog.seed_options.len(), forward);
    dialog.seed_index = next;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::dialog::new_agent::NewAgentDialog;

    fn dialog_with(prompt: &str) -> NewAgentDialog {
        let mut d = NewAgentDialog::new(Some("."));
        d.prompt = prompt.to_string();
        d.prompt_cursor = prompt.chars().count();
        d
    }

    #[test]
    fn insert_text_appends_and_moves_cursor() {
        let mut d = dialog_with("foo");
        insert_prompt_text(&mut d, "bar");
        assert_eq!(d.prompt, "foobar");
        assert_eq!(d.prompt_cursor, 6);
    }

    #[test]
    fn insert_text_at_middle_splits_string() {
        let mut d = dialog_with("fooo"); // cursor at end (4)
        d.prompt_cursor = 2;
        insert_prompt_text(&mut d, "X");
        assert_eq!(d.prompt, "foXoo");
        assert_eq!(d.prompt_cursor, 3);
    }

    #[test]
    fn backspace_removes_previous_char() {
        let mut d = dialog_with("foo");
        backspace_prompt(&mut d);
        assert_eq!(d.prompt, "fo");
        assert_eq!(d.prompt_cursor, 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut d = dialog_with("foo");
        d.prompt_cursor = 0;
        backspace_prompt(&mut d);
        assert_eq!(d.prompt, "foo");
        assert_eq!(d.prompt_cursor, 0);
    }

    #[test]
    fn delete_forward_removes_next_char() {
        let mut d = dialog_with("foo");
        d.prompt_cursor = 1;
        delete_prompt_forward(&mut d);
        assert_eq!(d.prompt, "fo");
        assert_eq!(d.prompt_cursor, 1);
    }

    #[test]
    fn paste_with_newlines_is_preserved() {
        let mut d = dialog_with("");
        insert_prompt_text(&mut d, "line1\nline2\nline3");
        assert_eq!(d.prompt, "line1\nline2\nline3");
        assert_eq!(d.prompt_cursor, 17);
    }

    #[test]
    fn paste_with_trailing_space_is_preserved() {
        let mut d = dialog_with("");
        insert_prompt_text(&mut d, "hello ");
        assert_eq!(d.prompt, "hello ");
        assert_eq!(d.prompt_cursor, 6);
    }

    #[test]
    fn visual_line_of_char_handles_soft_wrap() {
        let d = dialog_with("abcdefghij");
        assert_eq!(visual_line_of_char(&d, 0, 4), 0);
        assert_eq!(visual_line_of_char(&d, 4, 4), 1);
        assert_eq!(visual_line_of_char(&d, 9, 4), 2);
    }

    #[test]
    fn visual_line_of_char_handles_hard_newline() {
        let d = dialog_with("abc\ndef");
        assert_eq!(visual_line_of_char(&d, 2, 30), 0); // 'c' on line 0
        assert_eq!(visual_line_of_char(&d, 3, 30), 0); // '\n' position still on line 0
        assert_eq!(visual_line_of_char(&d, 4, 30), 1); // 'd' on line 1
    }

    #[test]
    fn move_visual_clamps_to_shorter_line() {
        let d = dialog_with("abcdefghij\nab");
        // cursor at 'j' on line 2 (pos 9, col 1 of line 2)
        // moving down should clamp to col 2 of line 3 (end of "ab")
        let new_cursor = move_prompt_visual(&d, 9, 4, true).expect("should move down");
        assert_eq!(new_cursor, 12); // "ab" ends at char index 12
    }

    #[test]
    fn move_visual_returns_none_at_first_line_up() {
        let d = dialog_with("hello");
        assert!(move_prompt_visual(&d, 3, 30, false).is_none());
    }

    #[test]
    fn move_visual_returns_none_at_last_line_down() {
        let d = dialog_with("hello");
        assert!(move_prompt_visual(&d, 3, 30, true).is_none());
    }

    #[test]
    fn home_and_end_jump_to_line_bounds() {
        let d = dialog_with("abcdefghij");
        // at pos 7, line 1, col 3
        let start = start_of_visual_line(&d, 7, 4);
        assert_eq!(start, 4);
        let end = end_of_visual_line(&d, 7, 4, 10);
        assert_eq!(end, 8);
    }

    #[test]
    fn char_to_byte_index_ascii() {
        assert_eq!(char_to_byte_index("hello", 0), 0);
        assert_eq!(char_to_byte_index("hello", 3), 3);
        assert_eq!(char_to_byte_index("hello", 5), 5);
    }

    #[test]
    fn char_to_byte_index_beyond_end() {
        assert_eq!(char_to_byte_index("hi", 10), 2);
    }

    #[test]
    fn char_to_byte_index_empty() {
        assert_eq!(char_to_byte_index("", 0), 0);
    }

    #[test]
    fn char_to_byte_index_multibyte() {
        let s = "café"; // é is 2 bytes in UTF-8
        assert_eq!(char_to_byte_index(s, 0), 0);
        assert_eq!(char_to_byte_index(s, 3), 3); // start of é
        assert_eq!(char_to_byte_index(s, 4), 5); // after é
    }

    #[test]
    fn visual_line_of_char_single_line() {
        let d = dialog_with("hello");
        assert_eq!(visual_line_of_char(&d, 0, 10), 0);
        assert_eq!(visual_line_of_char(&d, 4, 10), 0);
    }

    #[test]
    fn visual_line_of_char_at_end() {
        let d = dialog_with("abc");
        assert_eq!(visual_line_of_char(&d, 3, 10), 0);
    }

    #[test]
    fn visual_line_of_char_empty() {
        let d = dialog_with("");
        assert_eq!(visual_line_of_char(&d, 0, 10), 0);
    }

    #[test]
    fn visual_line_of_char_exact_boundary() {
        let d = dialog_with("abcdefgh");
        // field_width=4: chars 0-3 on line 0, chars 4-7 on line 1
        assert_eq!(visual_line_of_char(&d, 3, 4), 0);
        assert_eq!(visual_line_of_char(&d, 4, 4), 1);
    }

    #[test]
    fn start_of_visual_line_first_line() {
        let d = dialog_with("hello");
        assert_eq!(start_of_visual_line(&d, 2, 10), 0);
    }

    #[test]
    fn start_of_visual_line_wrapped() {
        let d = dialog_with("abcdefghij");
        // field_width=4: line 0 starts at 0, line 1 starts at 4
        assert_eq!(start_of_visual_line(&d, 5, 4), 4);
    }

    #[test]
    fn start_of_visual_line_at_beginning() {
        let d = dialog_with("hello");
        assert_eq!(start_of_visual_line(&d, 0, 10), 0);
    }

    #[test]
    fn end_of_visual_line_first_line() {
        let d = dialog_with("hello");
        assert_eq!(end_of_visual_line(&d, 2, 10, 5), 5);
    }

    #[test]
    fn end_of_visual_line_wrapped() {
        let d = dialog_with("abcdefghij");
        // field_width=4: line 0 ends at 4, line 1 ends at 8
        assert_eq!(end_of_visual_line(&d, 5, 4, 10), 8);
    }

    #[test]
    fn end_of_visual_line_at_end() {
        let d = dialog_with("abc");
        assert_eq!(end_of_visual_line(&d, 0, 10, 3), 3);
    }

    #[test]
    fn move_visual_down_within_line() {
        let d = dialog_with("abcdefghij");
        // cursor at col 0 of line 0, move down goes to col 0 of line 1
        let new_cursor = move_prompt_visual(&d, 0, 4, true).expect("should move");
        assert_eq!(new_cursor, 4);
    }

    #[test]
    fn move_visual_up_from_second_line() {
        let d = dialog_with("abcdefghij");
        // cursor at col 0 of line 1 (pos 4), move up goes to col 0 of line 0
        let new_cursor = move_prompt_visual(&d, 4, 4, false).expect("should move");
        assert_eq!(new_cursor, 0);
    }

    #[test]
    fn prompt_field_width_for_wide_terminal() {
        let width = prompt_field_width_for(200);
        assert!(width > 40);
        assert!(width < 200);
    }

    #[test]
    fn prompt_field_width_for_narrow_terminal() {
        let width = prompt_field_width_for(20);
        assert!(width >= 10);
    }

    #[test]
    fn prompt_field_width_for_minimum() {
        let width = prompt_field_width_for(0);
        assert!(width >= 10);
    }

    #[test]
    fn insert_text_empty_into_empty() {
        let mut d = dialog_with("");
        insert_prompt_text(&mut d, "");
        assert_eq!(d.prompt, "");
        assert_eq!(d.prompt_cursor, 0);
    }

    #[test]
    fn delete_forward_at_end() {
        let mut d = dialog_with("abc");
        d.prompt_cursor = 3;
        delete_prompt_forward(&mut d);
        assert_eq!(d.prompt, "abc");
    }

    #[test]
    fn delete_forward_empty() {
        let mut d = dialog_with("");
        delete_prompt_forward(&mut d);
        assert_eq!(d.prompt, "");
    }

    #[test]
    fn backspace_empty() {
        let mut d = dialog_with("");
        backspace_prompt(&mut d);
        assert_eq!(d.prompt, "");
    }

    #[test]
    fn insert_text_unicode() {
        let mut d = dialog_with("");
        insert_prompt_text(&mut d, "café");
        assert_eq!(d.prompt, "café");
        assert_eq!(d.prompt_cursor, 4);
    }

    #[test]
    fn visual_line_of_char_multibyte() {
        let d = dialog_with("caféxyz");
        // field_width=4: c(0) a(1) f(2) é(3) on line 0, x(4) y(5) z(6) on line 1
        assert_eq!(visual_line_of_char(&d, 4, 4), 1); // x is on line 1
    }

    #[test]
    fn visual_line_of_char_at_line_zero() {
        let d = dialog_with("hello world");
        assert_eq!(start_of_visual_line_at(&d, 0, 30), 0);
    }

    #[test]
    fn visual_line_of_char_at_with_wrap() {
        let d = dialog_with("abcdefghij");
        // field_width=4: line 0 starts at 0, line 1 starts at 4, line 2 starts at 8
        assert_eq!(start_of_visual_line_at(&d, 0, 4), 0);
        assert_eq!(start_of_visual_line_at(&d, 1, 4), 4);
        assert_eq!(start_of_visual_line_at(&d, 2, 4), 8);
    }

    #[test]
    fn end_of_visual_line_at_line_zero() {
        let d = dialog_with("hello world");
        assert_eq!(end_of_visual_line_at(&d, 0, 30, 11), 11);
    }

    #[test]
    fn end_of_visual_line_at_with_wrap() {
        let d = dialog_with("abcdefghij");
        assert_eq!(end_of_visual_line_at(&d, 0, 4, 10), 4);
        assert_eq!(end_of_visual_line_at(&d, 1, 4, 10), 8);
        assert_eq!(end_of_visual_line_at(&d, 2, 4, 10), 10);
    }

    #[test]
    fn start_of_visual_line_at_beyond_last_line() {
        let d = dialog_with("abc");
        // field_width=30 → 1 line. Asking for line 1 returns start of string (no wrapping).
        assert_eq!(start_of_visual_line_at(&d, 1, 30), 0);
    }

    #[test]
    fn end_of_visual_line_at_beyond_last_line() {
        let d = dialog_with("abc");
        assert_eq!(end_of_visual_line_at(&d, 1, 30, 3), 3);
    }

    #[test]
    fn prompt_field_width_for_80_col() {
        let width = prompt_field_width_for(80);
        // Should be reasonable: between 10 and 80
        assert!(width >= 10);
        assert!(width < 80);
    }

    #[test]
    fn prompt_field_width_for_40_col() {
        let width = prompt_field_width_for(40);
        assert!(width >= 10);
    }

    #[test]
    fn prompt_field_width_for_1_col() {
        let width = prompt_field_width_for(1);
        assert!(width >= 10);
    }

    #[test]
    fn prompt_field_width_for_100_col() {
        let width = prompt_field_width_for(100);
        assert!(width >= 10);
        assert!(width < 100);
    }

    #[test]
    fn visual_line_of_char_with_hard_newlines_and_wrap() {
        let d = dialog_with("ab\ncdef");
        // field_width=3: "ab" on line 0, "\n" at pos 2, "c" at pos 3 is line 1
        assert_eq!(visual_line_of_char(&d, 0, 3), 0); // 'a'
        assert_eq!(visual_line_of_char(&d, 2, 3), 0); // '\n'
        assert_eq!(visual_line_of_char(&d, 3, 3), 1); // 'c'
        assert_eq!(visual_line_of_char(&d, 4, 3), 1); // 'd'
    }

    #[test]
    fn move_prompt_visual_up_from_first_line_returns_none() {
        let d = dialog_with("hello");
        assert!(move_prompt_visual(&d, 0, 30, false).is_none());
    }

    #[test]
    fn move_prompt_visual_down_from_last_line_returns_none() {
        let d = dialog_with("ab");
        assert!(move_prompt_visual(&d, 1, 30, true).is_none());
    }

    #[test]
    fn move_prompt_visual_down_through_wrap() {
        let d = dialog_with("abcdefghij");
        // cursor at col 2 of line 0 (pos 2), move down → line 1 col 2 (pos 6)
        let new_cursor = move_prompt_visual(&d, 2, 4, true).expect("should move");
        assert_eq!(new_cursor, 6);
    }

    #[test]
    fn move_prompt_visual_up_through_wrap() {
        let d = dialog_with("abcdefghij");
        // cursor at col 2 of line 1 (pos 6), move up → line 0 col 2 (pos 2)
        let new_cursor = move_prompt_visual(&d, 6, 4, false).expect("should move");
        assert_eq!(new_cursor, 2);
    }

    #[test]
    fn end_of_visual_line_multiline() {
        let d = dialog_with("line1\nline2");
        // at char 7 (pos in "line2"), field_width=30
        let end = end_of_visual_line(&d, 7, 30, 11);
        assert_eq!(end, 11);
    }

    #[test]
    fn start_of_visual_line_multiline() {
        let d = dialog_with("line1\nline2");
        // at char 7 (in "line2")
        let start = start_of_visual_line(&d, 7, 30);
        assert_eq!(start, 6); // after the '\n'
    }

    // ── Mode step (CT34) ─────────────────────────────────────────

    fn interactive_form(dir: &str) -> NewAgentDialog {
        let mut d = NewAgentDialog::new(Some(dir));
        d.task_type = NewTaskType::Interactive;
        d.available_clis = vec![
            crate::domain::models::Cli::new("claude"),
            crate::domain::models::Cli::new("codex"),
        ];
        d.cli_configs = vec![None, None];
        d.cli_index = 0;
        d.working_dir = dir.to_string();
        d
    }

    fn app_with(dialog: NewAgentDialog) -> App {
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let path = db_file.path().to_path_buf();
        std::mem::forget(db_file); // keep the sqlite file alive for the test
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        app.new_agent_dialog = Some(dialog);
        app
    }

    fn session_in(
        id: &str,
        cli: &str,
        dir: &str,
        started_at: &str,
    ) -> crate::db::session::InteractiveSession {
        crate::db::session::InteractiveSession {
            id: id.to_string(),
            name: format!("session-{id}"),
            cli: cli.to_string(),
            working_dir: dir.to_string(),
            args: None,
            started_at: started_at.to_string(),
            status: "completed".to_string(),
            session_type: "interactive".to_string(),
            pid: None,
            boot_id: None,
        }
    }

    /// FR 7: Enter on an Interactive create form opens the Mode step and
    /// launches nothing.
    #[test]
    fn enter_on_interactive_form_opens_mode_step_without_launching() {
        let mut app = app_with(interactive_form("/tmp/proj"));

        handle_dialog_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();

        let dialog = app.new_agent_dialog.as_ref().expect("form stays open");
        assert_eq!(dialog.step, DialogStep::Mode);
        assert!(
            app.launchpad_dialog.is_none(),
            "the mission step must not open yet"
        );
        assert!(app.pending_launch_dialog.is_none());
        assert!(app.interactive_agents.is_empty(), "nothing launched");
    }

    /// FR 7: Esc from the Mode step returns to the form with every value —
    /// the directory above all — unchanged.
    #[test]
    fn esc_from_mode_step_returns_to_form_with_directory_kept() {
        let mut form = interactive_form("/tmp/proj");
        form.prompt = "keep me".to_string();
        let mut app = app_with(form);

        handle_dialog_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
        crate::tui::event::new_agent_mode_step::handle_mode_step_key(&mut app, KeyCode::Esc)
            .unwrap();

        let dialog = app.new_agent_dialog.as_ref().expect("still open");
        assert_eq!(dialog.step, DialogStep::Form);
        assert_eq!(
            dialog.working_dir, "/tmp/proj",
            "directory survives the round trip"
        );
        assert_eq!(dialog.prompt, "keep me", "form values survive too");
    }

    /// FR 3: New session continues to the mission step (launchpad) exactly
    /// as today's Interactive create path does.
    #[test]
    fn new_session_opens_the_launchpad() {
        let mut app = app_with(interactive_form("/tmp/proj"));

        handle_dialog_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();

        // FR 2: first Enter opens the Mode step — the form stays open.
        {
            let d = app
                .new_agent_dialog
                .as_ref()
                .expect("form stays open as Mode step");
            assert_eq!(d.step, DialogStep::Mode, "first Enter opens the Mode step");
        }

        crate::tui::event::new_agent_mode_step::handle_mode_step_key(&mut app, KeyCode::Enter)
            .unwrap();

        assert!(
            app.launchpad_dialog.is_some(),
            "New session must open the mission step"
        );
        assert!(app.pending_launch_dialog.is_some());
        assert!(app.new_agent_dialog.is_none());
        assert!(app.interactive_agents.is_empty(), "no session spawned yet");
    }

    /// FR 7: two native sessions in the chosen dir and one in another dir
    /// (or another harness) — the Resume list shows exactly the two.
    #[test]
    fn resume_lists_exactly_the_sessions_for_harness_and_directory() {
        let sessions = vec![
            session_in("s1", "claude", "/tmp/proj", "2026-10-01T10:00:00Z"),
            session_in("s2", "claude", "/tmp/proj/", "2026-10-01T09:00:00Z"),
            session_in("s3", "codex", "/tmp/proj", "2026-10-01T11:00:00Z"),
            session_in("s4", "claude", "/tmp/other", "2026-10-01T12:00:00Z"),
        ];
        let mut d = interactive_form("/tmp/proj");
        mode_step::open_mode_step(&mut d, sessions);
        d.mode_index = 1;
        let mut app = app_with(d);

        crate::tui::event::new_agent_mode_step::handle_mode_step_key(&mut app, KeyCode::Enter)
            .unwrap();

        let dialog = app.new_agent_dialog.as_ref().expect("still choosing");
        assert_eq!(dialog.step, DialogStep::Mode, "the list shows on the step");
        let picker = dialog
            .session_resume_picker
            .as_ref()
            .expect("the resume list opens");
        let ids: Vec<&str> = picker.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["s1", "s2"], "only harness + directory match");
        assert!(
            app.interactive_agents.is_empty(),
            "Enter only opened the list"
        );
        assert!(app.launchpad_dialog.is_none());
    }

    /// FR 7 + FR 5: nothing to resume → the Resume row is empty, Enter
    /// launches nothing and the dialog stays put.
    #[test]
    fn resume_with_nothing_to_resume_enter_launches_nothing() {
        let sessions = vec![session_in(
            "s1",
            "claude",
            "/tmp/other",
            "2026-10-01T10:00:00Z",
        )];
        let mut d = interactive_form("/tmp/proj");
        mode_step::open_mode_step(&mut d, sessions);
        assert!(mode_step::resume_row_is_empty(&d), "no session applies");
        d.mode_index = 1;
        let mut app = app_with(d);

        crate::tui::event::new_agent_mode_step::handle_mode_step_key(&mut app, KeyCode::Enter)
            .unwrap();

        let dialog = app.new_agent_dialog.as_ref().expect("dialog stays open");
        assert!(dialog.session_resume_picker.is_none(), "no list to show");
        assert!(!dialog.session_picker_open, "and no list left open");
        assert_eq!(
            dialog.task_mode,
            crate::tui::app::dialog::new_agent::NewTaskMode::Interactive
        );
        assert!(
            app.launchpad_dialog.is_none(),
            "the mission step must not open"
        );
        assert!(app.interactive_agents.is_empty(), "nothing launched");
    }

    /// FR 7: a harness with its own `session_list_cmd` gets that list on
    /// Resume, not the native one (FR 4's first branch).
    #[test]
    fn resume_uses_the_cli_own_session_list_when_the_harness_has_one() {
        let mut d = interactive_form("/tmp");
        d.cli_configs = vec![Some(crate::domain::cli_config::CliConfig {
            name: "claude".into(),
            binary: "echo".into(),
            // `echo` prints its args, so the parser sees one real session id.
            session_list_cmd: Some("ses_abcdef123 --list".into()),
            ..Default::default()
        })];
        mode_step::open_mode_step(&mut d, Vec::new());
        d.mode_index = 1;
        let mut app = app_with(d);

        crate::tui::event::new_agent_mode_step::handle_mode_step_key(&mut app, KeyCode::Enter)
            .unwrap();

        let dialog = app.new_agent_dialog.as_ref().expect("still choosing");
        assert!(dialog.session_picker_open, "the CLI's own list opens");
        assert!(
            dialog.session_resume_picker.is_none(),
            "not the native list"
        );
        assert_eq!(dialog.session_entries.len(), 1);
        assert!(app.interactive_agents.is_empty());
    }

    /// FR 7: Terminal (and Background) never open the Mode step.
    #[test]
    fn terminal_form_skips_the_mode_step() {
        let mut d = interactive_form("/tmp/proj");
        d.task_type = NewTaskType::Terminal;
        assert!(!mode_step::can_open_mode_step(&d));

        d.task_type = NewTaskType::Background;
        assert!(!mode_step::can_open_mode_step(&d));

        d.task_type = NewTaskType::Interactive;
        d.edit_id = Some("agent-1".into());
        assert!(!mode_step::can_open_mode_step(&d), "edit mode skips it too");
    }
}
