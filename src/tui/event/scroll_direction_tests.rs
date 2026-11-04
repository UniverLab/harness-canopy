use super::*;

#[test]
fn scroll_up_returns_positive() {
    assert_eq!(scroll_direction(MouseEventKind::ScrollUp), Some(1));
}

#[test]
fn scroll_down_returns_negative() {
    assert_eq!(scroll_direction(MouseEventKind::ScrollDown), Some(-1));
}

#[test]
fn other_mouse_kinds_return_none() {
    assert!(scroll_direction(MouseEventKind::Moved).is_none());
    assert!(scroll_direction(MouseEventKind::Down(MouseButton::Left)).is_none());
    assert!(scroll_direction(MouseEventKind::Up(MouseButton::Left)).is_none());
    assert!(scroll_direction(MouseEventKind::Drag(MouseButton::Left)).is_none());
}
