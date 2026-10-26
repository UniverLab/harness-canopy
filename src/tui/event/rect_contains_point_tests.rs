use super::*;
use ratatui::layout::Rect;

#[test]
fn point_inside_rect() {
    let rect = Rect::new(5, 10, 20, 10);
    assert!(rect_contains_point(rect, 10, 12));
    assert!(rect_contains_point(rect, 5, 10)); // top-left corner
    assert!(rect_contains_point(rect, 24, 19)); // bottom-right corner
}

#[test]
fn point_outside_rect() {
    let rect = Rect::new(5, 10, 20, 10);
    assert!(!rect_contains_point(rect, 4, 10)); // left
    assert!(!rect_contains_point(rect, 25, 10)); // right edge
    assert!(!rect_contains_point(rect, 10, 9)); // above
    assert!(!rect_contains_point(rect, 10, 20)); // below
}

#[test]
fn zero_size_rect() {
    let rect = Rect::new(5, 10, 0, 0);
    assert!(!rect_contains_point(rect, 5, 10));
}
