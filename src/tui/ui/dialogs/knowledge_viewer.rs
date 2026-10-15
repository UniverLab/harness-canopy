//! Read-only knowledge viewer dialog — one entry's body, scrollable.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::Frame;

use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

pub fn draw_knowledge_viewer(frame: &mut Frame, app: &App, theme: &Theme) {
    let Some(dialog) = app.knowledge_viewer.as_ref() else {
        return;
    };

    let dialog_area = centered_rect(60, 50, frame.area());

    let block = Block::default()
        .title(format!(" {} · {} ", dialog.kind, dialog.title))
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let lines: Vec<Line> = dialog.body.lines().map(Line::from).collect();
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((dialog.scroll, 0)),
        inner,
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::vertical([
        Constraint::Percentage((100 - percent_y) / 2),
        Constraint::Percentage(percent_y),
        Constraint::Percentage((100 - percent_y) / 2),
    ])
    .split(r);

    Layout::horizontal([
        Constraint::Percentage((100 - percent_x) / 2),
        Constraint::Percentage(percent_x),
        Constraint::Percentage((100 - percent_x) / 2),
    ])
    .split(popup_layout[1])[1]
}
