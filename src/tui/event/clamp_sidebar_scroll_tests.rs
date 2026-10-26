use super::*;

#[test]
fn scroll_down_increments() {
    assert_eq!(clamp_sidebar_scroll(0, 20, 8, -1), 1);
}

#[test]
fn scroll_down_clamps_at_max() {
    // total=20, max_visible=8 → max_offset=12
    assert_eq!(clamp_sidebar_scroll(12, 20, 8, -1), 12);
    assert_eq!(
        clamp_sidebar_scroll(13, 20, 8, -1),
        12,
        "should not exceed max"
    );
}

#[test]
fn scroll_up_decrements() {
    assert_eq!(clamp_sidebar_scroll(5, 20, 8, 1), 4);
}

#[test]
fn scroll_up_clamps_at_zero() {
    assert_eq!(clamp_sidebar_scroll(0, 20, 8, 1), 0);
}

#[test]
fn empty_list_stays_at_zero() {
    assert_eq!(clamp_sidebar_scroll(0, 0, 8, -1), 0);
    assert_eq!(clamp_sidebar_scroll(0, 0, 8, 1), 0);
}
