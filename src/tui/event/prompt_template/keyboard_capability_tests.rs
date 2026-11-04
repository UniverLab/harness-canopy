use super::should_send_on_shift_enter;

#[test]
fn shift_enter_sends_when_enhancement_active() {
    assert!(should_send_on_shift_enter(true, true));
}

#[test]
fn plain_enter_never_sends_even_with_enhancement_active() {
    assert!(!should_send_on_shift_enter(true, false));
}

#[test]
fn shift_enter_does_not_send_without_enhancement() {
    // Terminal can't disambiguate Shift+Enter from Enter here — Ctrl+S
    // (handled unconditionally elsewhere) is the fallback send key.
    assert!(!should_send_on_shift_enter(false, true));
}

#[test]
fn plain_enter_without_enhancement_does_not_send() {
    assert!(!should_send_on_shift_enter(false, false));
}
