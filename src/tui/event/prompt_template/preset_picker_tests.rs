use super::{handle_preset_picker_key, preset_filtered_indices};
use crate::tui::app::dialog::{SectionPickerMode, SimplePromptDialog};
use ratatui::crossterm::event::KeyCode;

fn dialog_with_presets(entries: Vec<(&str, &str, &str)>) -> SimplePromptDialog {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::PresetPicker {
        selected: 0,
        entries: entries
            .into_iter()
            .map(|(n, p, c)| (n.to_string(), p.to_string(), c.to_string()))
            .collect(),
        filter: String::new(),
    };
    dialog
}

#[test]
fn typing_filters_the_list_and_resets_selection_to_the_top() {
    let mut dialog = dialog_with_presets(vec![
        ("implementer", "role a", "implementer content"),
        ("reviewer", "role b", "reviewer content"),
        ("resilience", "role c", "resilience content"),
    ]);

    // Move off the top so the filter's reset-to-0 is actually exercised.
    handle_preset_picker_key(&mut dialog, KeyCode::Down);
    handle_preset_picker_key(&mut dialog, KeyCode::Down);

    handle_preset_picker_key(&mut dialog, KeyCode::Char('r'));
    handle_preset_picker_key(&mut dialog, KeyCode::Char('e'));

    let filtered = preset_filtered_indices(&dialog);
    let SectionPickerMode::PresetPicker {
        entries, selected, ..
    } = &dialog.picker_mode
    else {
        panic!("expected PresetPicker");
    };
    let names: Vec<&str> = filtered.iter().map(|&i| entries[i].0.as_str()).collect();
    assert_eq!(names, vec!["reviewer", "resilience"]);
    assert_eq!(*selected, 0);
}

#[test]
fn backspace_removes_a_filter_char_and_widens_the_match() {
    let mut dialog = dialog_with_presets(vec![
        ("implementer", "role a", "implementer content"),
        ("reviewer", "role b", "reviewer content"),
    ]);

    handle_preset_picker_key(&mut dialog, KeyCode::Char('x'));
    assert!(preset_filtered_indices(&dialog).is_empty());

    handle_preset_picker_key(&mut dialog, KeyCode::Backspace);
    assert_eq!(preset_filtered_indices(&dialog).len(), 2);
}

#[test]
fn enter_inserts_the_selected_preset_as_a_new_instruction_section_and_closes() {
    let mut dialog = dialog_with_presets(vec![
        ("implementer", "role a", "implementer content"),
        ("reviewer", "role b", "reviewer content"),
    ]);
    dialog.set_section_content("instruction_1", "existing task".to_string());

    handle_preset_picker_key(&mut dialog, KeyCode::Down); // select "reviewer"
    handle_preset_picker_key(&mut dialog, KeyCode::Enter);

    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
    // The existing instruction is left alone — the preset lands in a
    // fresh, freely-editable section rather than clobbering it.
    assert_eq!(dialog.get_section_content("instruction_1"), "existing task");
    assert_eq!(
        dialog.get_section_content("instruction_2"),
        "reviewer content"
    );
    assert!(dialog
        .enabled_sections
        .contains(&"instruction_2".to_string()));
}

#[test]
fn esc_closes_the_picker_without_inserting_anything() {
    let mut dialog = dialog_with_presets(vec![("implementer", "role a", "implementer content")]);
    let original_sections = dialog.enabled_sections.clone();

    handle_preset_picker_key(&mut dialog, KeyCode::Esc);

    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
    assert_eq!(dialog.enabled_sections, original_sections);
}

#[test]
fn enter_on_an_empty_directory_closes_without_inserting_anything() {
    // Mirrors the "no presets in ~/.canopy/prompts" hint state: the
    // picker opened successfully but found nothing to list.
    let mut dialog = dialog_with_presets(vec![]);
    let original_sections = dialog.enabled_sections.clone();

    handle_preset_picker_key(&mut dialog, KeyCode::Enter);

    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
    assert_eq!(dialog.enabled_sections, original_sections);
}

#[test]
fn enter_refuses_a_preset_with_an_unfilled_placeholder_and_inserts_nothing() {
    // Mirrors the observed C11 incident: picking a graph-engine role
    // preset (carrying {{spec_content}}) from the TUI's ad hoc composer,
    // which has no such binding to offer.
    let mut dialog = dialog_with_presets(vec![(
        "implementer",
        "role a",
        "Do this:\n\n{{spec_content}}",
    )]);
    let original_sections = dialog.enabled_sections.clone();

    handle_preset_picker_key(&mut dialog, KeyCode::Enter);

    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
    // Nothing was inserted — not a new section, and definitely not the
    // literal unfilled placeholder.
    assert_eq!(dialog.enabled_sections, original_sections);
    for id in &dialog.enabled_sections {
        assert!(!dialog.get_section_content(id).contains("{{"));
    }
}

#[test]
fn two_presets_confirmed_in_one_session_stay_in_separate_isolated_sections() {
    let mut dialog = dialog_with_presets(vec![
        ("alpha", "role a", "alpha body, nothing else"),
        ("beta", "role b", "beta body, nothing else"),
    ]);

    // Confirm "alpha" first.
    handle_preset_picker_key(&mut dialog, KeyCode::Enter);
    assert_eq!(
        dialog.get_section_content("instruction_2"),
        "alpha body, nothing else"
    );

    // Reopen the picker and confirm "beta" second.
    dialog.picker_mode = SectionPickerMode::PresetPicker {
        selected: 1,
        entries: vec![
            (
                "alpha".to_string(),
                "role a".to_string(),
                "alpha body, nothing else".to_string(),
            ),
            (
                "beta".to_string(),
                "role b".to_string(),
                "beta body, nothing else".to_string(),
            ),
        ],
        filter: String::new(),
    };
    handle_preset_picker_key(&mut dialog, KeyCode::Enter);

    // Each preset landed in its own section, containing only its own body.
    let alpha_content = dialog.get_section_content("instruction_2");
    let beta_content = dialog.get_section_content("instruction_3");
    assert_eq!(alpha_content, "alpha body, nothing else");
    assert_eq!(beta_content, "beta body, nothing else");
    assert!(!alpha_content.contains("beta"));
    assert!(!beta_content.contains("alpha"));
}
