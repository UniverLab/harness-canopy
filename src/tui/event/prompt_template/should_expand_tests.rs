use super::should_expand_on_key;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

#[test]
fn char_keys_expand() {
    assert!(should_expand_on_key(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(should_expand_on_key(
        KeyCode::Char('z'),
        KeyModifiers::SHIFT
    ));
    assert!(should_expand_on_key(KeyCode::Char(' '), KeyModifiers::NONE));
}

#[test]
fn backspace_and_delete_expand() {
    assert!(should_expand_on_key(KeyCode::Backspace, KeyModifiers::NONE));
    assert!(should_expand_on_key(KeyCode::Delete, KeyModifiers::NONE));
}

#[test]
fn enter_with_modifier_expands() {
    assert!(should_expand_on_key(KeyCode::Enter, KeyModifiers::CONTROL));
}

#[test]
fn enter_without_modifier_does_not_expand() {
    assert!(!should_expand_on_key(KeyCode::Enter, KeyModifiers::NONE));
}

#[test]
fn navigation_keys_do_not_expand() {
    assert!(!should_expand_on_key(KeyCode::Up, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::Down, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::Left, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::Right, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::Tab, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::BackTab, KeyModifiers::SHIFT));
}

#[test]
fn other_keys_do_not_expand() {
    assert!(!should_expand_on_key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::PageUp, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::PageDown, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::Home, KeyModifiers::NONE));
    assert!(!should_expand_on_key(KeyCode::End, KeyModifiers::NONE));
}
