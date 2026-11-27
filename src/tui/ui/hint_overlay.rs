//! First-run hint rendering: the `[?]` marker and the open tip popover.
//!
//! The data and state rules live in [`crate::tui::hints`]; this module only
//! draws. Anchor sites call [`draw_marker`], the top overlay calls
//! [`draw_tip_overlay`].

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::app::types::App;
use crate::tui::hints::{tick_style, HINTS};
use crate::tui::ui::theme::Theme;

/// Draw the `[?]` marker at `rect` (a 3-cell spot the caller reserved)
/// and record it for click hit-testing.
pub(crate) fn draw_marker(frame: &mut Frame, app: &mut App, theme: &Theme, rect: Rect) {
    if rect.width < 3 || rect.height < 1 {
        return;
    }
    let cell = Rect::new(rect.x, rect.y, 3, 1);
    frame.render_widget(
        Paragraph::new(Span::styled("[?]", tick_style(app, theme))),
        cell,
    );
    app.hint_marker = Some(cell);
}

/// The open tip: a centered popover with the hint text, its key, and
/// the two persisted actions. Records both action rows for clicks.
pub(crate) fn draw_tip_overlay(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let Some(id) = app.hint_tip_open else {
        return;
    };
    let Some(hint) = HINTS.iter().find(|hint| hint.id == id) else {
        return;
    };
    let area = frame.area();
    let body: Vec<&str> = hint.text.lines().collect();
    let wide = body
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let width = (wide + 4).clamp(24, area.width.saturating_sub(4).max(24));
    let height = (body.len() as u16 + 5).min(area.height.max(1));
    let rect = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(" Hint ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 4 || inner.width < 10 {
        return;
    }
    let mut lines: Vec<Line> = body
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(theme.text_primary))))
        .collect();
    lines.push(Line::from(Span::styled(
        format!("Key: {}", hint.key),
        Style::default().fg(theme.dim_text),
    )));
    lines.push(Line::from(vec![
        Span::styled(
            "y",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": Got it    ", Style::default().fg(theme.dim_text)),
        Span::styled(
            "n",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": Hide all hints    ", Style::default().fg(theme.dim_text)),
        Span::styled("Esc: close", Style::default().fg(theme.dim_text)),
    ]));
    let show = lines.len().min(inner.height as usize);
    frame.render_widget(Paragraph::new(lines[..show].to_vec()), inner);
    let row = inner.y + show.saturating_sub(1) as u16;
    app.hint_tip_got_it = Some(Rect::new(inner.x, row, 9.min(inner.width), 1));
    let hide_x = inner.x + 13.min(inner.width);
    let hide_w = 17.min(inner.width.saturating_sub(13.min(inner.width)));
    app.hint_tip_hide_all = Some(Rect::new(hide_x, row, hide_w, 1));
}
