//! CT31: the hub's Keys tab — the grouped reference of every shortcut, drawn
//! from `tui::keymap_reference`.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::app::types::App;
use crate::tui::keymap_reference::{rows_for, CONTEXTS};
use crate::tui::ui::theme::Theme;

pub(super) fn draw_keys_tab(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();
    for context in CONTEXTS {
        lines.push(Line::from(Span::styled(
            *context,
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        )));
        for row in rows_for(context) {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<14}", row.display_key()),
                    Style::default()
                        .fg(theme.text_primary)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(row.action, Style::default().fg(theme.dim_text)),
            ]));
        }
    }

    let height = area.height as usize;
    let total = lines.len();
    let start = app.hub.keys_scroll.min(total.saturating_sub(1));
    let end = (start + height).min(total);
    frame.render_widget(Paragraph::new(lines[start..end].to_vec()), area);
}
