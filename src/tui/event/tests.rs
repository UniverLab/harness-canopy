use super::search_picker::resolve_cd_picker_selection;
use crate::tui::terminal_history::{PickerMode, SuggestionItem, SuggestionPicker};
use std::path::PathBuf;

#[test]
fn test_cd_picker_selection_keeps_downstream_path() {
    let picker = SuggestionPicker {
        input: "./alpha".to_string(),
        mode: PickerMode::CdDirectory,
        all_items: vec![SuggestionItem {
            text: "./beta".to_string(),
            label: "./beta".to_string(),
            count: 0,
        }],
        items: vec![SuggestionItem {
            text: "./beta".to_string(),
            label: "./beta".to_string(),
            count: 0,
        }],
        selected: 0,
        scroll_offset: 0,
        cd_base_dir: Some(PathBuf::from("/repo")),
        cd_current_dir: Some(PathBuf::from("/repo/alpha")),
    };

    let resolved = resolve_cd_picker_selection(&picker).unwrap();
    assert_eq!(resolved, "alpha/beta");
}

#[test]
fn test_cd_picker_selection_keeps_parent_path_relative_to_base() {
    let picker = SuggestionPicker {
        input: "./alpha/beta".to_string(),
        mode: PickerMode::CdDirectory,
        all_items: vec![SuggestionItem {
            text: "..".to_string(),
            label: "../".to_string(),
            count: 0,
        }],
        items: vec![SuggestionItem {
            text: "..".to_string(),
            label: "../".to_string(),
            count: 0,
        }],
        selected: 0,
        scroll_offset: 0,
        cd_base_dir: Some(PathBuf::from("/repo")),
        cd_current_dir: Some(PathBuf::from("/repo/alpha/beta")),
    };

    let resolved = resolve_cd_picker_selection(&picker).unwrap();
    assert_eq!(resolved, "alpha");
}
