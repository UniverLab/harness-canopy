use super::*;
use crate::db::Database;
use crate::tui::app::dialog::{ProjectPickerEntry, SectionPickerMode, SimplePromptDialog};
use std::sync::Arc;
use tempfile::NamedTempFile;

#[test]
fn skills_picker_up_at_zero_wraps_to_last() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::SkillsPicker {
        selected: 0,
        scroll: 0,
        entries: vec![
            ("s1".into(), "r1".into(), "skill".into()),
            ("s2".into(), "r2".into(), "skill".into()),
        ],
        replace_id: None,
    };
    handle_skills_picker_key(&mut dialog, KeyCode::Up);
    if let SectionPickerMode::SkillsPicker { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, 1, "up from the first entry wraps to the last");
    } else {
        panic!("expected SkillsPicker");
    }
}

#[test]
fn skills_picker_down_at_end_wraps_to_first() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::SkillsPicker {
        selected: 1,
        scroll: 0,
        entries: vec![
            ("s1".into(), "r1".into(), "skill".into()),
            ("s2".into(), "r2".into(), "skill".into()),
        ],
        replace_id: None,
    };
    handle_skills_picker_key(&mut dialog, KeyCode::Down);
    if let SectionPickerMode::SkillsPicker { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, 0, "down from the last entry wraps to the first");
    } else {
        panic!("expected SkillsPicker");
    }
}

#[test]
fn skills_picker_esc_closes() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::SkillsPicker {
        selected: 0,
        scroll: 0,
        entries: vec![("s1".into(), "r1".into(), "skill".into())],
        replace_id: None,
    };
    handle_skills_picker_key(&mut dialog, KeyCode::Esc);
    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
}

/// Regression test for the reported bug: with more skills than fit the
/// picker's viewport, moving down past the last visible row must slide
/// the scroll offset so the selection stays on screen instead of
/// walking off the bottom.
#[test]
fn skills_picker_scrolls_past_visible_rows() {
    let entries: Vec<_> = (0..20)
        .map(|i| (format!("s{i}"), format!("r{i}"), "skill".to_string()))
        .collect();
    let visible_rows = SimplePromptDialog::skills_picker_visible_rows(20);
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::SkillsPicker {
        selected: visible_rows - 1,
        scroll: 0,
        entries,
        replace_id: None,
    };
    handle_skills_picker_key(&mut dialog, KeyCode::Down);
    if let SectionPickerMode::SkillsPicker {
        selected, scroll, ..
    } = &dialog.picker_mode
    {
        assert_eq!(*selected, visible_rows);
        assert_eq!(
            *scroll, 1,
            "offset must advance to keep the selection visible"
        );
    } else {
        panic!("expected SkillsPicker");
    }
}

fn make_project_entries(names: &[&str]) -> Vec<ProjectPickerEntry> {
    names
        .iter()
        .enumerate()
        .map(|(i, name)| ProjectPickerEntry {
            hash: format!("h{i}"),
            name: name.to_string(),
            path: format!("/{name}"),
        })
        .collect()
}

#[test]
fn project_picker_navigation() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&["p1", "p2"]),
        filter: String::new(),
        scroll: 0,
    };
    handle_project_picker_key(&mut dialog, KeyCode::Down);
    if let SectionPickerMode::ProjectPicker { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, 1);
    } else {
        panic!("expected ProjectPicker");
    }
}

#[test]
fn project_picker_navigation_wraps_at_both_ends() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&["p1", "p2", "p3"]),
        filter: String::new(),
        scroll: 0,
    };
    handle_project_picker_key(&mut dialog, KeyCode::Up);
    if let SectionPickerMode::ProjectPicker { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, 2, "Up from index 0 wraps to the last entry");
    } else {
        panic!("expected ProjectPicker");
    }

    handle_project_picker_key(&mut dialog, KeyCode::Down);
    if let SectionPickerMode::ProjectPicker { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, 0, "Down from the last entry wraps to index 0");
    } else {
        panic!("expected ProjectPicker");
    }
}

#[test]
fn project_picker_esc_closes() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&["p1"]),
        filter: String::new(),
        scroll: 0,
    };
    handle_project_picker_key(&mut dialog, KeyCode::Esc);
    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
}

#[test]
fn project_picker_typing_filters_and_resets_selection() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 1,
        entries: make_project_entries(&["alpha", "beta", "gamma"]),
        filter: String::new(),
        scroll: 2,
    };
    handle_project_picker_key(&mut dialog, KeyCode::Char('g'));
    let SectionPickerMode::ProjectPicker {
        filter,
        selected,
        scroll,
        ..
    } = &dialog.picker_mode
    else {
        panic!("expected ProjectPicker");
    };
    assert_eq!(filter, "g");
    assert_eq!(*selected, 0);
    assert_eq!(*scroll, 0);
    assert_eq!(project_filtered_indices(&dialog), vec![2]);
}

#[test]
fn project_picker_backspace_removes_last_filter_char_and_refilters() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&["alpha", "beta"]),
        filter: "al".to_string(),
        scroll: 0,
    };
    handle_project_picker_key(&mut dialog, KeyCode::Backspace);
    let SectionPickerMode::ProjectPicker { filter, .. } = &dialog.picker_mode else {
        panic!("expected ProjectPicker");
    };
    assert_eq!(filter, "a");
    assert_eq!(project_filtered_indices(&dialog), vec![0, 1]);
}

#[test]
fn project_picker_filter_matching_nothing_yields_empty_filtered_list() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&["alpha", "beta"]),
        filter: "zzz".to_string(),
        scroll: 0,
    };
    assert!(project_filtered_indices(&dialog).is_empty());
    // Navigation is a no-op against an empty filtered list.
    handle_project_picker_key(&mut dialog, KeyCode::Down);
    let SectionPickerMode::ProjectPicker { selected, .. } = &dialog.picker_mode else {
        panic!("expected ProjectPicker");
    };
    assert_eq!(*selected, 0);
}

#[test]
fn project_picker_enter_confirms_highlighted_filtered_entry() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&["alpha", "beta", "gamma"]),
        filter: "a".to_string(),
        scroll: 0,
    };
    // filter "a" matches alpha(0), beta(1), gamma(2) — move to beta.
    handle_project_picker_key(&mut dialog, KeyCode::Down);
    handle_project_picker_key(&mut dialog, KeyCode::Enter);
    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
    assert!(
        dialog
            .sections
            .values()
            .any(|content| content.contains("/beta")),
        "confirming should insert the highlighted (filtered) entry's path"
    );
}

#[test]
fn project_picker_scroll_follows_selection_past_visible_window() {
    let names: Vec<String> = (0..20).map(|i| format!("proj{i}")).collect();
    let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::ProjectPicker {
        selected: 0,
        entries: make_project_entries(&name_refs),
        filter: String::new(),
        scroll: 0,
    };
    for _ in 0..19 {
        handle_project_picker_key(&mut dialog, KeyCode::Down);
    }
    let SectionPickerMode::ProjectPicker {
        selected, scroll, ..
    } = &dialog.picker_mode
    else {
        panic!("expected ProjectPicker");
    };
    assert_eq!(*selected, 19);
    let visible_rows = SimplePromptDialog::project_picker_visible_rows(20);
    assert!(
        *selected < scroll + visible_rows && *selected >= *scroll,
        "selected entry {selected} must stay within the visible window [{scroll}, {})",
        scroll + visible_rows
    );
}

#[test]
fn add_custom_section_esc_closes() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::AddCustom {
        input: "test".into(),
    };
    handle_add_custom_section_key(&mut dialog, "test".into(), KeyCode::Esc);
    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
}

#[test]
fn add_custom_section_char_appends() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::AddCustom {
        input: String::new(),
    };
    handle_add_custom_section_key(&mut dialog, String::new(), KeyCode::Char('x'));
    if let SectionPickerMode::AddCustom { input } = &dialog.picker_mode {
        assert_eq!(input, "x");
    } else {
        panic!("expected AddCustom");
    }
}

#[test]
fn add_custom_section_backspace_removes() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::AddCustom {
        input: "abc".into(),
    };
    handle_add_custom_section_key(&mut dialog, "abc".into(), KeyCode::Backspace);
    if let SectionPickerMode::AddCustom { input } = &dialog.picker_mode {
        assert_eq!(input, "ab");
    } else {
        panic!("expected AddCustom");
    }
}

#[test]
fn remove_section_picker_navigation() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("context");
    dialog.add_section("context");
    dialog.picker_mode = SectionPickerMode::RemoveSection {
        selected: 1,
        scroll: 0,
    };
    handle_remove_section_picker_key(&mut dialog, 1, KeyCode::Up);
    if let SectionPickerMode::RemoveSection { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, 0);
    } else {
        panic!("expected RemoveSection");
    }
}

#[test]
fn remove_section_picker_up_at_zero_wraps_to_last() {
    let mut dialog = SimplePromptDialog::new();
    dialog.add_section("context");
    dialog.add_section("context");
    let last = dialog.get_removable_sections().len() - 1;
    dialog.picker_mode = SectionPickerMode::RemoveSection {
        selected: 0,
        scroll: 0,
    };
    handle_remove_section_picker_key(&mut dialog, 0, KeyCode::Up);
    if let SectionPickerMode::RemoveSection { selected, .. } = &dialog.picker_mode {
        assert_eq!(*selected, last, "up from the first entry wraps to the last");
    } else {
        panic!("expected RemoveSection");
    }
}

#[test]
fn remove_section_picker_esc_closes() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::RemoveSection {
        selected: 0,
        scroll: 0,
    };
    handle_remove_section_picker_key(&mut dialog, 0, KeyCode::Esc);
    assert_eq!(dialog.picker_mode, SectionPickerMode::None);
}

fn test_db() -> Arc<Database> {
    let tmp = NamedTempFile::new().expect("create temp file");
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(Database::new(&path).expect("create test db"))
}

#[test]
fn add_section_picker_up_at_zero_wraps_to_last() {
    let mut dialog = SimplePromptDialog::new();
    let last = dialog.get_addable_sections().len() - 1;
    dialog.picker_mode = SectionPickerMode::AddSection { selected: 0 };
    let db = test_db();
    let workdir = Path::new("/tmp");
    handle_add_section_picker_key(&mut dialog, 0, &db, workdir, KeyCode::Up).unwrap();
    if let SectionPickerMode::AddSection { selected } = &dialog.picker_mode {
        assert_eq!(*selected, last, "up from the first entry wraps to the last");
    } else {
        panic!("expected AddSection");
    }
}

#[test]
fn add_section_picker_down_at_last_wraps_to_zero() {
    let mut dialog = SimplePromptDialog::new();
    let last = dialog.get_addable_sections().len() - 1;
    dialog.picker_mode = SectionPickerMode::AddSection { selected: last };
    let db = test_db();
    let workdir = Path::new("/tmp");
    handle_add_section_picker_key(&mut dialog, last, &db, workdir, KeyCode::Down).unwrap();
    if let SectionPickerMode::AddSection { selected } = &dialog.picker_mode {
        assert_eq!(*selected, 0, "down from the last entry wraps to the first");
    } else {
        panic!("expected AddSection");
    }
}

#[test]
fn add_section_picker_custom_switches_mode() {
    let mut dialog = SimplePromptDialog::new();
    dialog.picker_mode = SectionPickerMode::AddSection { selected: 0 };
    let db = test_db();
    let workdir = Path::new("/tmp");
    handle_add_section_picker_key(&mut dialog, 0, &db, workdir, KeyCode::Char('c')).unwrap();
    assert!(matches!(
        dialog.picker_mode,
        SectionPickerMode::AddCustom { .. }
    ));
}
