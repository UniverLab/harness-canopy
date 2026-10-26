use super::tail_pane_split;
use ratatui::layout::Rect;

#[test]
fn split_horizontal_at_160x40() {
    let area = Rect::new(0, 0, 160, 40);
    let (graph, pane) = tail_pane_split(area);
    assert_eq!(graph.width, 80);
    assert_eq!(pane.width, 80);
    assert_eq!(graph.height, 40);
    assert_eq!(pane.height, 40);
    assert_eq!(graph.x, 0);
    assert_eq!(pane.x, 80);
}

#[test]
fn split_vertical_at_100x40() {
    let area = Rect::new(0, 0, 100, 40);
    let (graph, pane) = tail_pane_split(area);
    assert_eq!(graph.height, 16); // 40 * 40% = 16
    assert_eq!(graph.y, 0);
    assert_eq!(pane.y, 16);
    assert_eq!(graph.width, 100);
    assert_eq!(pane.width, 100);
}

#[test]
fn split_vertical_minimum_8_rows() {
    let area = Rect::new(0, 0, 80, 10);
    let (graph, _) = tail_pane_split(area);
    // 10 * 40% = 4, clamped to min 8
    assert_eq!(graph.height, 8);
}
