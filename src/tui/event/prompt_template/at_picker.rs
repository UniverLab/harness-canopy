use super::*;

pub(super) fn handle_at_picker_key(
    dialog: &mut SimplePromptDialog,
    code: KeyCode,
    modifiers: KeyModifiers,
    section_name: &str,
    field_width: usize,
) -> bool {
    if dialog.at_picker.is_none() {
        return false;
    }

    match code {
        KeyCode::Esc => dialog.at_picker = None,
        KeyCode::Up => move_at_picker(dialog, false),
        KeyCode::Down => move_at_picker(dialog, true),
        KeyCode::Left => go_up_at_picker_dir(dialog),
        KeyCode::Right => enter_selected_at_picker_dir(dialog),
        KeyCode::Enter | KeyCode::Tab => {
            apply_at_picker_selection(dialog, section_name, field_width);
        }
        KeyCode::Backspace => handle_at_picker_backspace(dialog, section_name, field_width),
        KeyCode::Char(c) if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT => {
            push_at_picker_query(dialog, c, modifiers);
        }
        _ => {}
    }

    true
}

pub(super) fn move_at_picker(dialog: &mut SimplePromptDialog, forward: bool) {
    let Some(picker) = dialog.at_picker.as_mut() else {
        return;
    };
    picker.selected =
        crate::tui::selection::move_index(picker.selected, picker.entries.len(), forward);
}

pub(super) fn go_up_at_picker_dir(dialog: &mut SimplePromptDialog) {
    let Some(picker) = dialog.at_picker.as_mut() else {
        return;
    };

    picker.go_up();
}

pub(super) fn enter_selected_at_picker_dir(dialog: &mut SimplePromptDialog) {
    let Some(picker) = dialog.at_picker.as_mut() else {
        return;
    };

    let is_dir = picker
        .entries
        .get(picker.selected)
        .map(|entry| entry.is_dir)
        .unwrap_or(false);
    if is_dir {
        picker.enter_dir();
    }
}

pub(super) fn apply_at_picker_selection(
    dialog: &mut SimplePromptDialog,
    section_name: &str,
    field_width: usize,
) {
    let Some((rel_path, full_path)) = selected_at_picker_paths(dialog) else {
        dialog.at_picker = None;
        return;
    };

    let original_focus = dialog.focused_section;
    dialog.insert_at_completion(section_name, &rel_path, &full_path, field_width);
    dialog.focused_section = original_focus;
    dialog.at_picker = None;
}

pub(super) fn selected_at_picker_paths(dialog: &SimplePromptDialog) -> Option<(String, String)> {
    let picker = dialog.at_picker.as_ref()?;
    let rel_path = picker.relative_path_of_selected()?;
    let full_path = picker.full_path_of_selected()?;
    Some((rel_path, full_path.to_string_lossy().to_string()))
}

pub(super) fn handle_at_picker_backspace(
    dialog: &mut SimplePromptDialog,
    section_name: &str,
    field_width: usize,
) {
    let query_is_empty = dialog
        .at_picker
        .as_ref()
        .map(|picker| picker.query.is_empty())
        .unwrap_or(true);

    if query_is_empty {
        dialog.at_picker = None;
        dialog.backspace_at_cursor(section_name, field_width);
        return;
    }

    let Some(picker) = dialog.at_picker.as_mut() else {
        return;
    };

    picker.query.pop();
    picker.queue_search();
}

pub(super) fn push_at_picker_query(
    dialog: &mut SimplePromptDialog,
    c: char,
    modifiers: KeyModifiers,
) {
    let Some(picker) = dialog.at_picker.as_mut() else {
        return;
    };

    let ch = if modifiers.contains(KeyModifiers::SHIFT) {
        c.to_uppercase().next().unwrap_or(c)
    } else {
        c
    };
    picker.query.push(ch);
    picker.queue_search();
}
