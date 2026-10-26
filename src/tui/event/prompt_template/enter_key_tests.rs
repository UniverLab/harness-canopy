use super::*;
use crate::tui::app::dialog::SimplePromptDialog;

#[test]
fn enter_in_context_inserts_newline_does_not_send() {
    let mut dialog = SimplePromptDialog::new();
    let id = dialog.add_section_with_content("context", "hello".to_string());
    let action = handle_enter_key(&mut dialog, KeyModifiers::empty(), &id, 80)
        .expect("enter handling succeeds");
    assert!(
        matches!(action, PromptAction::None),
        "Enter in a context section must not send"
    );
    assert!(
        dialog.get_section_content(&id).contains('\n'),
        "Enter in a context section must insert a newline"
    );
}

#[test]
fn enter_in_instruction_inserts_newline() {
    let mut dialog = SimplePromptDialog::new();
    let id = dialog.add_section_with_content("instruction", "hello".to_string());
    let action = handle_enter_key(&mut dialog, KeyModifiers::empty(), &id, 80)
        .expect("enter handling succeeds");
    assert!(
        matches!(action, PromptAction::None),
        "Enter in an instruction section must not send"
    );
    assert!(
        dialog.get_section_content(&id).contains('\n'),
        "Enter in an instruction section must insert a newline"
    );
}

// The raw tab's own key handler already inserts a newline on plain Enter
// (it never routes through `handle_enter_key`), so no test is needed here.

#[test]
fn enter_with_nonempty_modifiers_is_noop() {
    let mut dialog = SimplePromptDialog::new();
    let id = dialog.add_section_with_content("context", "hello".to_string());
    let before = dialog.get_section_content(&id);
    let action = handle_enter_key(&mut dialog, KeyModifiers::CONTROL, &id, 80)
        .expect("enter handling succeeds");
    assert!(
        matches!(action, PromptAction::None),
        "Enter with modifiers must not send"
    );
    assert_eq!(
        dialog.get_section_content(&id),
        before,
        "Enter with modifiers must leave content unchanged"
    );
}

#[test]
fn enter_in_unknown_section_inserts_newline() {
    let mut dialog = SimplePromptDialog::new();
    let id = dialog.add_section_with_content("banana", "hello".to_string());
    let action = handle_enter_key(&mut dialog, KeyModifiers::empty(), &id, 80)
        .expect("enter handling succeeds");
    assert!(
        matches!(action, PromptAction::None),
        "Enter in an unknown section must not send"
    );
    assert!(
        dialog.get_section_content(&id).contains('\n'),
        "Enter in an unknown section must insert a newline"
    );
}
