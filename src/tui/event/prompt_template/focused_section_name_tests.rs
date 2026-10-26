use super::focused_section_name;
use crate::tui::app::dialog::SimplePromptDialog;

#[test]
fn focus_zero_returns_none() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 0;
    assert!(focused_section_name(&mut dialog).is_none());
}

#[test]
fn focus_one_returns_first_section() {
    let mut dialog = SimplePromptDialog::new();
    dialog.focused_section = 1;
    assert_eq!(
        focused_section_name(&mut dialog),
        Some("instruction_1".to_string())
    );
}

#[test]
fn stale_focus_is_clamped() {
    let mut dialog = SimplePromptDialog::new();
    // Only 1 enabled section; focus 5 is out of bounds.
    dialog.focused_section = 5;
    let name = focused_section_name(&mut dialog);
    assert!(name.is_some());
    assert_eq!(
        dialog.focused_section, 1,
        "should be clamped to valid index"
    );
}

#[test]
fn no_enabled_sections_returns_none_for_nonzero_focus() {
    let mut dialog = SimplePromptDialog::new();
    dialog.enabled_sections.clear();
    dialog.focused_section = 1;
    assert!(focused_section_name(&mut dialog).is_none());
}
