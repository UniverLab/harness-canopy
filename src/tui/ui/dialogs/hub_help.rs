//! The hub's Help tab: main keys, the one-line graph explanation, and
//! the "Reset hints" action (`r` or click) that brings dismissed hints back.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::tui::app::types::App;
use crate::tui::ui::theme::Theme;

/// One `key — action` row of the main-keys list.
const MAIN_KEYS: &[(&str, &str)] = &[
    ("Ctrl+N", "New session"),
    ("F1", "Hub / context hint"),
    ("F5", "Prompt menu"),
    ("Shift/Ctrl/Alt+Enter", "Newline in agent"),
    ("Esc", "Back / Close"),
    ("Ctrl+Q", "Quit"),
];

/// The one-line graph explanation the Help tab shows.
pub(crate) const GRAPH_ONE_LINER: &str =
    "A graph is a reusable workflow of agent, check, and gate nodes that automates multi-step tasks.";

pub(super) fn draw_help_tab(frame: &mut Frame, app: &mut App, theme: &Theme, area: Rect) {
    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        "Main keys",
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD),
    ))];
    for (key, action) in MAIN_KEYS {
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {key:<22}"),
                Style::default()
                    .fg(theme.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(*action, Style::default().fg(theme.dim_text)),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        GRAPH_ONE_LINER,
        Style::default().fg(theme.dim_text),
    )));
    lines.push(Line::from(""));
    let reset_row = lines.len() as u16;
    lines.push(Line::from(vec![
        Span::styled(
            "  Reset hints",
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  (r or click)", Style::default().fg(theme.dim_text)),
    ]));

    let height = area.height as usize;
    let show = lines.len().min(height);
    frame.render_widget(Paragraph::new(lines[..show].to_vec()), area);
    app.hub.help_reset_rect = if (reset_row as usize) < show {
        Some(Rect::new(area.x, area.y + reset_row, area.width, 1))
    } else {
        None
    };
}
