use super::*;

pub(super) fn handle_section_picker_key(
    dialog: &mut SimplePromptDialog,
    db: &Database,
    workdir: &Path,
    code: KeyCode,
) -> Result<bool> {
    match &dialog.picker_mode {
        SectionPickerMode::None => Ok(false),
        SectionPickerMode::AddSection { selected } => {
            handle_add_section_picker_key(dialog, *selected, db, workdir, code)?;
            Ok(true)
        }
        SectionPickerMode::AddCustom { input } => {
            handle_add_custom_section_key(dialog, input.clone(), code);
            Ok(true)
        }
        SectionPickerMode::RemoveSection { selected, .. } => {
            handle_remove_section_picker_key(dialog, *selected, code);
            Ok(true)
        }
        SectionPickerMode::SkillsPicker { .. } => {
            handle_skills_picker_key(dialog, code);
            Ok(true)
        }
        SectionPickerMode::ProjectPicker { .. } => {
            handle_project_picker_key(dialog, code);
            Ok(true)
        }
        SectionPickerMode::PresetPicker { .. } => {
            handle_preset_picker_key(dialog, code);
            Ok(true)
        }
    }
}

pub(super) fn handle_add_section_picker_key(
    dialog: &mut SimplePromptDialog,
    selected: usize,
    db: &Database,
    workdir: &Path,
    code: KeyCode,
) -> Result<()> {
    match code {
        KeyCode::Esc => dialog.picker_mode = SectionPickerMode::None,
        KeyCode::Up => move_add_section_picker(dialog, selected, false),
        KeyCode::Down => move_add_section_picker(dialog, selected, true),
        KeyCode::Enter => select_addable_section(dialog, selected, db, workdir)?,
        KeyCode::Char('c') => {
            dialog.picker_mode = SectionPickerMode::AddCustom {
                input: String::new(),
            };
        }
        _ => {}
    }

    Ok(())
}

/// Moves the highlighted row, wrapping at both ends via the shared
/// [`crate::tui::selection`] model — up from the first entry lands on the
/// last, down from the last lands on the first. The menu is always short
/// enough to fit its box in full, so there is no scroll offset to track.
pub(super) fn move_add_section_picker(
    dialog: &mut SimplePromptDialog,
    selected: usize,
    forward: bool,
) {
    let len = dialog.get_addable_sections().len();
    let selected = crate::tui::selection::move_index(selected, len, forward);
    dialog.picker_mode = SectionPickerMode::AddSection { selected };
}

pub(super) fn select_addable_section(
    dialog: &mut SimplePromptDialog,
    selected: usize,
    db: &Database,
    workdir: &Path,
) -> Result<()> {
    let addable = dialog.get_addable_sections();
    let Some((name, _)) = addable.get(selected).copied() else {
        return Ok(());
    };

    match name {
        "tools" => open_skills_picker(dialog, workdir),
        "project_context" => open_project_picker(dialog, db)?,
        "preset" => open_preset_picker(dialog),
        // The manual protocol resend (FR4): insert the locked, pre-filled
        // `Canopy protocol` section and close the picker like a plain add.
        "canopy_protocol" => {
            dialog.add_canopy_protocol_section();
            dialog.picker_mode = SectionPickerMode::None;
        }
        _ => {
            dialog.add_section(name);
            dialog.picker_mode = SectionPickerMode::None;
        }
    }

    Ok(())
}

/// Open the Preset picker (P2): lists `~/.canopy/prompts/*.md`, read fresh
/// so an external edit shows up immediately. An empty/missing directory
/// still opens the picker — it just shows the "no presets" hint (rendered
/// in `ui::dialogs::section_picker`) instead of erroring.
pub(super) fn open_preset_picker(dialog: &mut SimplePromptDialog) {
    let dir = crate::domain::prompts::prompts_dir(&crate::domain::prompts::canopy_dir());
    let entries = SimplePromptDialog::collect_presets_for_picker(&dir);
    dialog.picker_mode = SectionPickerMode::PresetPicker {
        selected: 0,
        entries,
        filter: String::new(),
    };
}

pub(super) fn open_skills_picker(dialog: &mut SimplePromptDialog, workdir: &Path) {
    let dir = crate::domain::prompts::canopy_dir();
    let store = crate::dynamic_skills::SkillStore::from_config(
        &dir,
        &crate::domain::canopy_config::CanopyConfig::load(&dir).skills,
    );
    let entries = SimplePromptDialog::collect_skills_for_picker(workdir, Some(&store));
    dialog.picker_mode = SectionPickerMode::SkillsPicker {
        selected: 0,
        scroll: 0,
        entries,
        replace_id: None,
    };
}

pub(super) fn open_project_picker(dialog: &mut SimplePromptDialog, db: &Database) -> Result<()> {
    let entries = SimplePromptDialog::collect_projects_for_picker(db)?;
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries,
        filter: String::new(),
        scroll: 0,
    };
    Ok(())
}

pub(super) fn handle_add_custom_section_key(
    dialog: &mut SimplePromptDialog,
    mut input: String,
    code: KeyCode,
) {
    match code {
        KeyCode::Esc => dialog.picker_mode = SectionPickerMode::None,
        KeyCode::Enter => {
            if input.is_empty() || dialog.enabled_sections.contains(&input) {
                return;
            }

            dialog.add_section(&input);
            dialog.picker_mode = SectionPickerMode::None;
        }
        KeyCode::Char(c) => {
            input.push(c);
            dialog.picker_mode = SectionPickerMode::AddCustom { input };
        }
        KeyCode::Backspace => {
            input.pop();
            dialog.picker_mode = SectionPickerMode::AddCustom { input };
        }
        _ => {}
    }
}

pub(super) fn handle_remove_section_picker_key(
    dialog: &mut SimplePromptDialog,
    selected: usize,
    code: KeyCode,
) {
    match code {
        KeyCode::Esc => dialog.picker_mode = SectionPickerMode::None,
        KeyCode::Up => move_remove_section_picker(dialog, selected, false),
        KeyCode::Down => move_remove_section_picker(dialog, selected, true),
        KeyCode::Enter => select_removable_section(dialog, selected),
        _ => {}
    }
}

/// Moves the highlighted row, wrapping at both ends and rescrolling so it
/// stays visible — this list grows with `enabled_sections` and can exceed
/// the viewport, unlike the fixed, short `AddSection` menu.
pub(super) fn move_remove_section_picker(
    dialog: &mut SimplePromptDialog,
    selected: usize,
    forward: bool,
) {
    let len = dialog.get_removable_sections().len();
    let SectionPickerMode::RemoveSection { scroll, .. } = &dialog.picker_mode else {
        return;
    };
    let visible_rows = SimplePromptDialog::remove_section_visible_rows(len);
    let (selected, scroll) =
        crate::tui::selection::move_selection(selected, *scroll, len, visible_rows, forward);
    dialog.picker_mode = SectionPickerMode::RemoveSection { selected, scroll };
}

pub(super) fn select_removable_section(dialog: &mut SimplePromptDialog, selected: usize) {
    let removable = dialog.get_removable_sections();
    let Some((section_id, _)) = removable.get(selected) else {
        return;
    };

    dialog.remove_section(section_id);
    dialog.picker_mode = SectionPickerMode::None;
}

pub(super) fn handle_skills_picker_key(dialog: &mut SimplePromptDialog, code: KeyCode) {
    match code {
        KeyCode::Esc => dialog.picker_mode = SectionPickerMode::None,
        KeyCode::Up => move_skills_picker(dialog, false),
        KeyCode::Down => move_skills_picker(dialog, true),
        KeyCode::Enter | KeyCode::Tab => confirm_skills_picker_selection(dialog),
        _ => {}
    }
}

/// Moves the highlighted row, wrapping at both ends and rescrolling so it
/// stays visible — this is the tools menu that used to walk its selection
/// off screen once the skill count exceeded the viewport.
pub(super) fn move_skills_picker(dialog: &mut SimplePromptDialog, forward: bool) {
    let SectionPickerMode::SkillsPicker {
        selected,
        scroll,
        entries,
        ..
    } = &mut dialog.picker_mode
    else {
        return;
    };
    let len = entries.len();
    let visible_rows = SimplePromptDialog::skills_picker_visible_rows(len);
    let (new_selected, new_scroll) =
        crate::tui::selection::move_selection(*selected, *scroll, len, visible_rows, forward);
    *selected = new_selected;
    *scroll = new_scroll;
}

pub(super) fn confirm_skills_picker_selection(dialog: &mut SimplePromptDialog) {
    let SectionPickerMode::SkillsPicker {
        entries,
        selected,
        replace_id,
        ..
    } = std::mem::replace(&mut dialog.picker_mode, SectionPickerMode::None)
    else {
        return;
    };

    let Some((label, _, _)) = entries.get(selected) else {
        return;
    };

    match replace_id {
        Some(section_id) => dialog.set_tools_section_skill(&section_id, label),
        None => {
            dialog.add_section_with_content("tools", label.clone());
        }
    }
}

/// Indices into the Preset picker's `entries` that currently pass its typed
/// filter, or `Vec::new()` if `dialog.picker_mode` isn't `PresetPicker`.
pub(super) fn preset_filtered_indices(dialog: &SimplePromptDialog) -> Vec<usize> {
    let SectionPickerMode::PresetPicker {
        entries, filter, ..
    } = &dialog.picker_mode
    else {
        return Vec::new();
    };
    SimplePromptDialog::filtered_preset_indices(entries, filter)
}

pub(super) fn handle_preset_picker_key(dialog: &mut SimplePromptDialog, code: KeyCode) {
    match code {
        KeyCode::Esc => dialog.picker_mode = SectionPickerMode::None,
        KeyCode::Up => move_preset_picker(dialog, false),
        KeyCode::Down => move_preset_picker(dialog, true),
        KeyCode::Enter | KeyCode::Tab => confirm_preset_picker_selection(dialog),
        KeyCode::Backspace => pop_preset_picker_filter(dialog),
        KeyCode::Char(c) => push_preset_picker_filter(dialog, c),
        _ => {}
    }
}

pub(super) fn move_preset_picker(dialog: &mut SimplePromptDialog, forward: bool) {
    let filtered_len = preset_filtered_indices(dialog).len();
    if filtered_len == 0 {
        return;
    }
    let SectionPickerMode::PresetPicker { selected, .. } = &mut dialog.picker_mode else {
        return;
    };
    *selected = crate::tui::selection::move_index(*selected, filtered_len, forward);
}

pub(super) fn push_preset_picker_filter(dialog: &mut SimplePromptDialog, c: char) {
    if let SectionPickerMode::PresetPicker {
        filter, selected, ..
    } = &mut dialog.picker_mode
    {
        filter.push(c);
        *selected = 0;
    }
}

pub(super) fn pop_preset_picker_filter(dialog: &mut SimplePromptDialog) {
    if let SectionPickerMode::PresetPicker {
        filter, selected, ..
    } = &mut dialog.picker_mode
    {
        filter.pop();
        *selected = 0;
    }
}

/// Insert the selected preset's full content as a new instruction section
/// (mirrors the Skills picker's un-replace_id path: `add_section_with_content`)
/// so the preset becomes a normal, freely-editable field — never a locked
/// reference — and closes the picker either way.
///
/// Presets under `~/.canopy/prompts/` also serve the graph engine's own role
/// templates (implementer/reviewer/resilience), which carry `{{spec_content}}`/
/// `{{previous_feedback}}` placeholders meant to be filled at agent-spawn
/// time. This TUI composes ad hoc prompts with no such bindings to offer, so
/// a preset that still carries an unfilled placeholder is refused — never
/// inserted with the literal `{{...}}` left in — rather than silently
/// dumping a foreign template into the user's message (spec C11).
pub(super) fn confirm_preset_picker_selection(dialog: &mut SimplePromptDialog) {
    let filtered = preset_filtered_indices(dialog);
    let SectionPickerMode::PresetPicker {
        selected, entries, ..
    } = &dialog.picker_mode
    else {
        return;
    };
    let preset = filtered
        .get(*selected)
        .and_then(|&idx| entries.get(idx))
        .map(|(name, _, content)| (name.clone(), content.clone()));

    dialog.picker_mode = SectionPickerMode::None;
    let Some((name, content)) = preset else {
        return;
    };

    match crate::domain::prompts::render_preset(&name, &content, &HashMap::new()) {
        Ok(rendered) => {
            dialog.add_section_with_content("instruction", rendered);
        }
        Err(err) => {
            crate::domain::notification::send_notification(
                "Preset not inserted",
                &err.to_string(),
                crate::domain::notification::NotificationLevel::Warning,
            );
        }
    }
}

/// Indices into the Project picker's `entries` that currently pass its typed
/// filter, or `Vec::new()` if `dialog.picker_mode` isn't `ProjectPicker`.
pub(super) fn project_filtered_indices(dialog: &SimplePromptDialog) -> Vec<usize> {
    let SectionPickerMode::ProjectPicker {
        entries, filter, ..
    } = &dialog.picker_mode
    else {
        return Vec::new();
    };
    SimplePromptDialog::filtered_project_indices(entries, filter)
}

pub(super) fn handle_project_picker_key(dialog: &mut SimplePromptDialog, code: KeyCode) {
    match code {
        KeyCode::Esc => dialog.picker_mode = SectionPickerMode::None,
        KeyCode::Up => move_project_picker(dialog, false),
        KeyCode::Down => move_project_picker(dialog, true),
        KeyCode::Enter | KeyCode::Tab => confirm_project_picker_selection(dialog),
        KeyCode::Backspace => pop_project_picker_filter(dialog),
        KeyCode::Char(c) => push_project_picker_filter(dialog, c),
        _ => {}
    }
}

/// Move the highlighted row up/down within the filtered list, wrapping at
/// both ends, then slides `scroll` (via the shared [`crate::tui::selection`]
/// model) so the new selection is always drawn — including right after a
/// wraparound jump.
pub(super) fn move_project_picker(dialog: &mut SimplePromptDialog, forward: bool) {
    let filtered_len = project_filtered_indices(dialog).len();
    if filtered_len == 0 {
        return;
    }
    let SectionPickerMode::ProjectPicker {
        selected, scroll, ..
    } = &mut dialog.picker_mode
    else {
        return;
    };
    let visible_rows = SimplePromptDialog::project_picker_visible_rows(filtered_len);
    let (new_selected, new_scroll) = crate::tui::selection::move_selection(
        *selected,
        *scroll,
        filtered_len,
        visible_rows,
        forward,
    );
    *selected = new_selected;
    *scroll = new_scroll;
}

pub(super) fn push_project_picker_filter(dialog: &mut SimplePromptDialog, c: char) {
    if let SectionPickerMode::ProjectPicker {
        filter,
        selected,
        scroll,
        ..
    } = &mut dialog.picker_mode
    {
        filter.push(c);
        *selected = 0;
        *scroll = 0;
    }
}

pub(super) fn pop_project_picker_filter(dialog: &mut SimplePromptDialog) {
    if let SectionPickerMode::ProjectPicker {
        filter,
        selected,
        scroll,
        ..
    } = &mut dialog.picker_mode
    {
        filter.pop();
        *selected = 0;
        *scroll = 0;
    }
}

pub(super) fn confirm_project_picker_selection(dialog: &mut SimplePromptDialog) {
    let filtered = project_filtered_indices(dialog);
    let SectionPickerMode::ProjectPicker {
        selected, entries, ..
    } = &dialog.picker_mode
    else {
        return;
    };
    let project = filtered
        .get(*selected)
        .and_then(|&idx| entries.get(idx))
        .cloned();

    dialog.picker_mode = SectionPickerMode::None;
    if let Some(project) = project {
        dialog.add_section_with_content("project_context", project.path);
    }
}

pub(super) fn open_add_section_picker_if_available(dialog: &mut SimplePromptDialog) {
    if dialog.get_addable_sections().is_empty() {
        return;
    }

    dialog.picker_mode = SectionPickerMode::AddSection { selected: 0 };
}

pub(super) fn open_remove_section_picker_if_available(dialog: &mut SimplePromptDialog) {
    if dialog.get_removable_sections().is_empty() {
        return;
    }

    dialog.picker_mode = SectionPickerMode::RemoveSection {
        selected: 0,
        scroll: 0,
    };
}

pub(super) fn handle_section_char_input(
    dialog: &mut SimplePromptDialog,
    section_name: &str,
    c: char,
    field_width: usize,
    workdir: &Path,
) {
    if SimplePromptDialog::is_tools_section(section_name) {
        return;
    }

    dialog.insert_char_at_cursor(section_name, c, field_width);
    if c != '@' || dialog.at_picker.is_some() {
        return;
    }

    let trigger_pos = dialog.cursor(section_name).saturating_sub(1);
    dialog.at_picker = Some(AtPicker::new(workdir.to_path_buf(), trigger_pos));
}

pub(super) fn handle_section_backspace(
    dialog: &mut SimplePromptDialog,
    section_name: &str,
    field_width: usize,
) {
    if SimplePromptDialog::is_tools_section(section_name) {
        return;
    }

    // Check if cursor is inside a collapsed paste block
    if dialog.cursor_in_collapsed_placeholder(section_name) {
        dialog.backspace_collapsed_paste(section_name, field_width);
    } else {
        dialog.backspace_at_cursor(section_name, field_width);
    }
}
