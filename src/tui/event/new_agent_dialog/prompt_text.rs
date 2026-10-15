//! Multi-line prompt editing and visual-line math for the new-agent dialog.
//! Split out of `new_agent_dialog.rs` to keep every production file within
//! the file-size gate.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};

use super::{DialogFields, NewAgentDialog};
use crate::tui::ui::dialogs::new_agent_dialog::{prompt_visual_line_count, PROMPT_VISIBLE_ROWS};

pub(super) fn handle_prompt_field(
    term_width: u16,
    dialog: &mut NewAgentDialog,
    code: KeyCode,
    fields: DialogFields,
    modifiers: KeyModifiers,
) {
    // The prompt input is now multi-line + responsive: cursor can sit anywhere
    // in the string, scroll follows the cursor, and pasted multi-line text is
    // kept verbatim (rendered with hard breaks by the wrap helper).
    let field_width = prompt_field_width_for(term_width);
    let max_lines = prompt_visual_line_count(dialog, field_width);
    let char_len = dialog.prompt.chars().count();
    let cursor = dialog.prompt_cursor.min(char_len);

    match code {
        KeyCode::Char(c) => {
            insert_prompt_text(dialog, &c.to_string());
        }
        KeyCode::Backspace => backspace_prompt(dialog),
        KeyCode::Delete => delete_prompt_forward(dialog),
        KeyCode::Left => {
            if cursor > 0 {
                dialog.prompt_cursor = cursor - 1;
            }
        }
        KeyCode::Right => {
            if cursor < char_len {
                dialog.prompt_cursor = cursor + 1;
            }
        }
        KeyCode::Home => {
            dialog.prompt_cursor = start_of_visual_line(dialog, cursor, field_width);
        }
        KeyCode::End => {
            dialog.prompt_cursor = end_of_visual_line(dialog, cursor, field_width, char_len);
        }
        KeyCode::Enter if modifiers.contains(KeyModifiers::SHIFT) => {
            // Shift+Enter inserts a hard newline (Enter alone is reserved for
            // submitting the dialog from the top-level handler).
            insert_prompt_text(dialog, "\n");
        }
        KeyCode::Up => {
            if let Some(new_cursor) = move_prompt_visual(dialog, cursor, field_width, false) {
                dialog.prompt_cursor = new_cursor;
            } else {
                // In send mode the model row is not rendered (CM53), so Up goes
                // straight to the target picker.
                dialog.field = fields.prompt_up_target(dialog);
                return;
            }
        }
        KeyCode::Down => {
            if let Some(new_cursor) = move_prompt_visual(dialog, cursor, field_width, true) {
                dialog.prompt_cursor = new_cursor;
            } else {
                dialog.field = fields.extra_field;
                return;
            }
        }
        KeyCode::PageUp => {
            dialog.prompt_scroll = dialog.prompt_scroll.saturating_sub(PROMPT_VISIBLE_ROWS);
        }
        KeyCode::PageDown => {
            let max_scroll = max_lines.saturating_sub(PROMPT_VISIBLE_ROWS);
            dialog.prompt_scroll = (dialog.prompt_scroll + PROMPT_VISIBLE_ROWS).min(max_scroll);
        }
        _ => return,
    }

    // Recompute after mutation, then re-clamp the cursor and keep the
    // containing visual line in view.
    let char_len = dialog.prompt.chars().count();
    let max_lines = prompt_visual_line_count(dialog, field_width);
    dialog.prompt_cursor = dialog.prompt_cursor.min(char_len);
    let cursor_line = visual_line_of_char(dialog, dialog.prompt_cursor, field_width);
    let max_scroll = max_lines.saturating_sub(PROMPT_VISIBLE_ROWS);
    if cursor_line < dialog.prompt_scroll {
        dialog.prompt_scroll = cursor_line;
    } else if cursor_line >= dialog.prompt_scroll + PROMPT_VISIBLE_ROWS {
        dialog.prompt_scroll = cursor_line + 1 - PROMPT_VISIBLE_ROWS;
    }
    dialog.prompt_scroll = dialog.prompt_scroll.min(max_scroll);
    let _ = max_lines; // recomputed for max_scroll above
}

/// Insert `text` at the current cursor. Newlines are kept as hard breaks
/// (the renderer flushes the current line on `\n`); spaces are stored verbatim
/// so a paste that ends with a trailing space survives.
pub(crate) fn insert_prompt_text(dialog: &mut NewAgentDialog, text: &str) {
    if text.is_empty() {
        return;
    }
    let byte_index = char_to_byte_index(&dialog.prompt, dialog.prompt_cursor);
    dialog.prompt.insert_str(byte_index, text);
    dialog.prompt_cursor += text.chars().count();
}

pub(super) fn backspace_prompt(dialog: &mut NewAgentDialog) {
    if dialog.prompt_cursor == 0 {
        return;
    }
    let start = char_to_byte_index(&dialog.prompt, dialog.prompt_cursor - 1);
    let end = char_to_byte_index(&dialog.prompt, dialog.prompt_cursor);
    dialog.prompt.replace_range(start..end, "");
    dialog.prompt_cursor -= 1;
}

pub(super) fn delete_prompt_forward(dialog: &mut NewAgentDialog) {
    let char_len = dialog.prompt.chars().count();
    if dialog.prompt_cursor >= char_len {
        return;
    }
    let start = char_to_byte_index(&dialog.prompt, dialog.prompt_cursor);
    let end = char_to_byte_index(&dialog.prompt, dialog.prompt_cursor + 1);
    dialog.prompt.replace_range(start..end, "");
}

pub(super) fn char_to_byte_index(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or_else(|| s.len())
}

pub(super) fn visual_line_of_char(
    dialog: &NewAgentDialog,
    char_index: usize,
    field_width: usize,
) -> usize {
    let field_width = field_width.max(1);
    let mut line = 0usize;
    let mut col = 0usize;
    for (i, ch) in dialog.prompt.chars().enumerate() {
        if i == char_index {
            // The char at `i` is the first on its line; resolve the wrap
            // that landed it on this line (if any) before returning.
            if col >= field_width {
                return line + 1;
            }
            return line;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
            continue;
        }
        if col >= field_width {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    line
}

pub(super) fn start_of_visual_line(
    dialog: &NewAgentDialog,
    char_index: usize,
    field_width: usize,
) -> usize {
    let field_width = field_width.max(1);
    let mut line_start = 0usize;
    let mut col = 0usize;
    for (i, ch) in dialog.prompt.chars().enumerate() {
        if i == char_index {
            return line_start;
        }
        if ch == '\n' {
            line_start = i + 1;
            col = 0;
            continue;
        }
        if col >= field_width {
            line_start = i;
            col = 1;
        } else {
            col += 1;
        }
    }
    line_start
}

pub(super) fn end_of_visual_line(
    dialog: &NewAgentDialog,
    char_index: usize,
    field_width: usize,
    char_len: usize,
) -> usize {
    let field_width = field_width.max(1);
    let mut line_end = char_len;
    let mut col = 0usize;
    for (i, ch) in dialog.prompt.chars().enumerate() {
        if ch == '\n' {
            line_end = i;
            if i >= char_index {
                return line_end;
            }
            col = 0;
            line_end = char_len;
            continue;
        }
        if col >= field_width {
            line_end = i;
            if i >= char_index {
                return line_end;
            }
            col = 1;
            line_end = char_len;
        } else {
            col += 1;
        }
    }
    line_end
}

pub(super) fn move_prompt_visual(
    dialog: &NewAgentDialog,
    cursor: usize,
    field_width: usize,
    forward: bool,
) -> Option<usize> {
    let char_len = dialog.prompt.chars().count();
    let current_line = visual_line_of_char(dialog, cursor, field_width);
    let total_lines = prompt_visual_line_count(dialog, field_width);
    if !forward && current_line == 0 {
        return None;
    }
    if forward && current_line + 1 >= total_lines {
        return None;
    }
    let target_line = if forward {
        current_line + 1
    } else {
        current_line - 1
    };
    let target_start = start_of_visual_line_at(dialog, target_line, field_width);
    let target_end = end_of_visual_line_at(dialog, target_line, field_width, char_len);
    let col_in_current =
        cursor.saturating_sub(start_of_visual_line_at(dialog, current_line, field_width));
    let target_width = target_end.saturating_sub(target_start);
    Some(target_start + col_in_current.min(target_width))
}

pub(super) fn start_of_visual_line_at(
    dialog: &NewAgentDialog,
    target_line: usize,
    field_width: usize,
) -> usize {
    let field_width = field_width.max(1);
    let mut line = 0usize;
    let mut line_start = 0usize;
    let mut col = 0usize;
    for (i, ch) in dialog.prompt.chars().enumerate() {
        if line == target_line {
            return line_start;
        }
        if ch == '\n' {
            line += 1;
            line_start = i + 1;
            col = 0;
            continue;
        }
        if col >= field_width {
            line += 1;
            line_start = i;
            col = 1;
        } else {
            col += 1;
        }
    }
    line_start
}

pub(super) fn end_of_visual_line_at(
    dialog: &NewAgentDialog,
    target_line: usize,
    field_width: usize,
    char_len: usize,
) -> usize {
    let field_width = field_width.max(1);
    let mut line = 0usize;
    let mut line_end = char_len;
    let mut col = 0usize;
    for (i, ch) in dialog.prompt.chars().enumerate() {
        if ch == '\n' {
            if line == target_line {
                return i;
            }
            line += 1;
            col = 0;
            line_end = char_len;
            continue;
        }
        if col >= field_width {
            if line == target_line {
                return i;
            }
            line += 1;
            col = 1;
            line_end = char_len;
        } else {
            col += 1;
        }
    }
    line_end
}

/// Mirrors the clamp the renderer uses (`prompt_field_width` in
/// `ui/dialogs/new_agent_dialog.rs`) so the input handler's cursor / scroll
/// math agrees with what the user sees on the screen.
pub(crate) fn prompt_field_width_for(term_width: u16) -> usize {
    let term_width = term_width.max(1);
    let max_dialog_w = term_width.saturating_sub(2).max(1);
    let preferred_dialog_w = term_width.saturating_mul(65) / 100;
    let min_dialog_w = 40u16.min(max_dialog_w);
    let dialog_width = preferred_dialog_w.clamp(min_dialog_w, max_dialog_w);
    (dialog_width.saturating_sub(5) as usize).max(10)
}
