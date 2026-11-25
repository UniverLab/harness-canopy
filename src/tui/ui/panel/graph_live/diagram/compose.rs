use std::collections::HashMap;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use super::{connector_char, PlacedBox, BOX_WIDTH};
use crate::tui::ui::theme::Theme;

pub(super) fn bright_accent(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            r.saturating_add(60),
            g.saturating_add(60),
            b.saturating_add(60),
        ),
        _ => Color::White,
    }
}

pub(super) fn dim_accent(c: Color, dim: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(r / 2, g / 2, b / 2),
        _ => dim,
    }
}

fn recolor_border(line: Line<'static>, fg: Color) -> Line<'static> {
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|s| Span::styled(s.content, s.style.fg(fg)))
        .collect();
    Line::from(spans)
}

fn active_path_cells(
    edge_paths: &[super::EdgePath],
    running_key: Option<&str>,
    tick: Option<u32>,
    theme: &Theme,
) -> HashMap<(usize, usize), (Option<char>, Color)> {
    let mut map: HashMap<(usize, usize), (Option<char>, Color)> = HashMap::new();
    let Some(rk) = running_key else {
        return map;
    };
    let accent = theme.status_running;
    let dim = dim_accent(accent, theme.dim_text);
    for ((_, to), path) in edge_paths {
        if to != rk || path.is_empty() {
            continue;
        }
        for &cell in path {
            map.entry(cell).or_insert((None, accent));
        }
        if let Some(t) = tick {
            let n = path.len();
            let dot_idx = (t / 3) as usize % n;
            map.insert(path[dot_idx], (Some('●'), accent));
            if dot_idx >= 1 {
                map.insert(path[dot_idx - 1], (Some('·'), dim));
            }
            if dot_idx >= 2 {
                map.insert(path[dot_idx - 2], (Some('·'), dim));
            }
        }
    }
    map
}

fn push_plain_run(
    grid: &[Vec<u8>],
    y: usize,
    mut x: usize,
    active: &HashMap<(usize, usize), (Option<char>, Color)>,
    conn_style: Style,
    spans: &mut Vec<Span<'static>>,
) -> usize {
    let mut text = String::new();
    while x < grid[y].len() && grid[y][x] != 0 && !active.contains_key(&(x, y)) {
        text.push(connector_char(grid[y][x]));
        x += 1;
    }
    if !text.is_empty() {
        spans.push(Span::styled(text, conn_style));
    }
    x
}
pub(super) fn compose_canvas(
    placed: Vec<PlacedBox>,
    grid: &[Vec<u8>],
    edge_paths: &[super::EdgePath],
    total_height: usize,
    theme: &Theme,
    running_key: Option<&str>,
    tick: Option<u32>,
) -> (Vec<Line<'static>>, bool) {
    let mut row_boxes: Vec<Vec<(usize, Line<'static>)>> = vec![Vec::new(); total_height];
    for b in placed {
        let is_running = running_key == Some(b.key.as_str());
        let box_h = b.lines.len();
        let pulse_fg = if tick.is_some_and(|t| (t / 8) % 2 == 0) {
            theme.status_running
        } else {
            bright_accent(theme.status_running)
        };
        let solid_fg = bright_accent(theme.status_running);
        for (dy, line) in b.lines.into_iter().enumerate() {
            let line = if is_running && (dy == 0 || dy + 1 == box_h) {
                recolor_border(line, if tick.is_some() { pulse_fg } else { solid_fg })
            } else {
                line
            };
            row_boxes[b.y + dy].push((b.x, line));
        }
    }

    let active = active_path_cells(edge_paths, running_key, tick, theme);
    let conn_style = Style::default().fg(theme.dim_text);
    let mut out = Vec::with_capacity(total_height);

    for y in 0..total_height {
        let mut items: Vec<(usize, usize, Vec<Span<'static>>)> = Vec::new();
        for (x, line) in std::mem::take(&mut row_boxes[y]) {
            items.push((x, BOX_WIDTH, line.spans));
        }
        let mut x = 0usize;
        while x < grid[y].len() {
            if grid[y][x] == 0 {
                x += 1;
                continue;
            }
            let start = x;
            let mut spans = Vec::new();
            while x < grid[y].len() && grid[y][x] != 0 {
                if let Some((ch, color)) = active.get(&(x, y)) {
                    let glyph = ch.unwrap_or_else(|| connector_char(grid[y][x]));
                    spans.push(Span::styled(glyph.to_string(), Style::default().fg(*color)));
                    x += 1;
                } else {
                    x = push_plain_run(grid, y, x, &active, conn_style, &mut spans);
                }
            }
            items.push((start, x - start, spans));
        }
        items.sort_by_key(|(col, _, _)| *col);

        let mut spans = Vec::new();
        let mut cur_col = 0usize;
        for (col, width, item_spans) in items {
            if col > cur_col {
                spans.push(Span::raw(" ".repeat(col - cur_col)));
            }
            spans.extend(item_spans);
            cur_col = col + width;
        }
        out.push(Line::from(spans));
    }

    let anim_drawn = running_key.is_some() && tick.is_some();
    (out, anim_drawn)
}
