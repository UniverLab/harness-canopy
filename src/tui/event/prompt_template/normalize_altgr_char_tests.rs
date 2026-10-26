use super::normalize_altgr_char;

#[test]
fn maps_q_q_and_2_to_at() {
    assert_eq!(normalize_altgr_char('q'), '@');
    assert_eq!(normalize_altgr_char('Q'), '@');
    assert_eq!(normalize_altgr_char('2'), '@');
}

#[test]
fn other_chars_pass_through() {
    assert_eq!(normalize_altgr_char('a'), 'a');
    assert_eq!(normalize_altgr_char('z'), 'z');
    assert_eq!(normalize_altgr_char(' '), ' ');
    assert_eq!(normalize_altgr_char('1'), '1');
}
