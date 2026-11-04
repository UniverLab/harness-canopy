use super::*;
use ratatui::crossterm::event::KeyCode;

/// should_expand_on_key expands on regular character input
#[test]
fn should_expand_on_char_input() {
    assert!(should_expand_on_key(
        KeyCode::Char('a'),
        KeyModifiers::empty()
    ));
    assert!(should_expand_on_key(
        KeyCode::Char('Z'),
        KeyModifiers::empty()
    ));
    assert!(should_expand_on_key(
        KeyCode::Char('1'),
        KeyModifiers::empty()
    ));
    assert!(should_expand_on_key(
        KeyCode::Char(' '),
        KeyModifiers::empty()
    ));
}

/// should_expand_on_key expands on Backspace and Delete
#[test]
fn should_expand_on_delete_keys() {
    assert!(should_expand_on_key(
        KeyCode::Backspace,
        KeyModifiers::empty()
    ));
    assert!(should_expand_on_key(KeyCode::Delete, KeyModifiers::empty()));
}

/// should_expand_on_key doesn't expand on navigation keys
#[test]
fn should_not_expand_on_navigation() {
    assert!(!should_expand_on_key(KeyCode::Up, KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::Down, KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::Left, KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::Right, KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::Tab, KeyModifiers::empty()));
    assert!(!should_expand_on_key(
        KeyCode::BackTab,
        KeyModifiers::empty()
    ));
}

/// should_expand_on_key expands on Enter only with modifiers
#[test]
fn should_expand_on_enter_with_modifiers() {
    assert!(!should_expand_on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert!(should_expand_on_key(KeyCode::Enter, KeyModifiers::SHIFT));
    assert!(should_expand_on_key(KeyCode::Enter, KeyModifiers::CONTROL));
}

/// should_send_on_shift_enter requires both conditions
#[test]
fn should_send_on_shift_enter_both_required() {
    assert!(!should_send_on_shift_enter(false, false));
    assert!(!should_send_on_shift_enter(true, false));
    assert!(!should_send_on_shift_enter(false, true));
    assert!(should_send_on_shift_enter(true, true));
}

/// normalize_prompt_char_input passes through chars with no modifiers
#[test]
fn normalize_prompt_char_input_no_modifiers() {
    assert_eq!(
        normalize_prompt_char_input('a', KeyModifiers::empty()),
        Some('a')
    );
    assert_eq!(
        normalize_prompt_char_input('Z', KeyModifiers::empty()),
        Some('Z')
    );
}

/// normalize_prompt_char_input passes through chars with only SHIFT
#[test]
fn normalize_prompt_char_input_shift_only() {
    assert_eq!(
        normalize_prompt_char_input('a', KeyModifiers::SHIFT),
        Some('a')
    );
    assert_eq!(
        normalize_prompt_char_input('Z', KeyModifiers::SHIFT),
        Some('Z')
    );
}

/// normalize_prompt_char_input returns None for CTRL alone
#[test]
fn normalize_prompt_char_input_ctrl_alone() {
    assert_eq!(
        normalize_prompt_char_input('a', KeyModifiers::CONTROL),
        None
    );
}

/// normalize_prompt_char_input normalizes AltGr chars (Ctrl+Alt)
#[test]
fn normalize_prompt_char_input_altgr() {
    let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
    assert_eq!(normalize_prompt_char_input('q', altgr), Some('@'));
    assert_eq!(normalize_prompt_char_input('Q', altgr), Some('@'));
    assert_eq!(normalize_prompt_char_input('2', altgr), Some('@'));
    // Other chars pass through
    assert_eq!(normalize_prompt_char_input('x', altgr), Some('x'));
}

/// normalize_altgr_char maps known AltGr sequences
#[test]
fn normalize_altgr_char_mappings() {
    assert_eq!(normalize_altgr_char('q'), '@');
    assert_eq!(normalize_altgr_char('Q'), '@');
    assert_eq!(normalize_altgr_char('2'), '@');
}

/// normalize_altgr_char passes through other characters
#[test]
fn normalize_altgr_char_passthrough() {
    assert_eq!(normalize_altgr_char('a'), 'a');
    assert_eq!(normalize_altgr_char('Z'), 'Z');
    assert_eq!(normalize_altgr_char('1'), '1');
    assert_eq!(normalize_altgr_char('@'), '@');
}

/// should_expand_on_key with various modifier combinations
#[test]
fn should_expand_on_key_modifier_combinations() {
    // Alt alone should not affect char expansion
    assert!(should_expand_on_key(KeyCode::Char('a'), KeyModifiers::ALT));
    // Ctrl alone should not affect char expansion
    assert!(should_expand_on_key(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL
    ));
    // Super should not affect char expansion
    assert!(should_expand_on_key(
        KeyCode::Char('a'),
        KeyModifiers::SUPER
    ));
}

/// normalize_prompt_char_input with all modifier combinations
#[test]
fn normalize_prompt_char_input_all_combinations() {
    let alt_only = KeyModifiers::ALT;
    let super_only = KeyModifiers::SUPER;

    // Alt alone: None
    assert_eq!(normalize_prompt_char_input('a', alt_only), None);
    // Super alone: None
    assert_eq!(normalize_prompt_char_input('a', super_only), None);
    // Shift+Ctrl: None (not treated specially)
    assert_eq!(
        normalize_prompt_char_input('a', KeyModifiers::SHIFT | KeyModifiers::CONTROL),
        None
    );
}

/// should_expand_on_key exhaustive key coverage
#[test]
fn should_expand_on_key_escape_and_special() {
    // Escape and function keys should not expand
    assert!(!should_expand_on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::F(1), KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::Home, KeyModifiers::empty()));
    assert!(!should_expand_on_key(KeyCode::End, KeyModifiers::empty()));
    assert!(!should_expand_on_key(
        KeyCode::PageUp,
        KeyModifiers::empty()
    ));
    assert!(!should_expand_on_key(
        KeyCode::PageDown,
        KeyModifiers::empty()
    ));
}
