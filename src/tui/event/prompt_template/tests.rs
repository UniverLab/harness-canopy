use super::normalize_prompt_char_input;
use ratatui::crossterm::event::KeyModifiers;

#[test]
fn normalize_prompt_char_input_accepts_plain_and_shift_chars() {
    assert_eq!(
        normalize_prompt_char_input('@', KeyModifiers::NONE),
        Some('@')
    );
    assert_eq!(
        normalize_prompt_char_input('A', KeyModifiers::SHIFT),
        Some('A')
    );
}

#[test]
fn normalize_prompt_char_input_maps_common_altgr_at_variants() {
    let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
    assert_eq!(normalize_prompt_char_input('q', altgr), Some('@'));
    assert_eq!(normalize_prompt_char_input('2', altgr), Some('@'));
    assert_eq!(normalize_prompt_char_input('@', altgr), Some('@'));
}

#[test]
fn normalize_prompt_char_input_rejects_control_shortcuts() {
    assert_eq!(
        normalize_prompt_char_input('a', KeyModifiers::CONTROL),
        None
    );
}
