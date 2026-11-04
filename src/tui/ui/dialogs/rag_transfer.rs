use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use super::{centered_rect, draw_dialog_left_wave, truncate_str};
use crate::tui::app::types::{App, RagTransferModal};
use crate::tui::ui::effective_animation_tick;
use crate::tui::ui::theme::Theme;

pub fn draw_rag_transfer_modal(frame: &mut Frame, app: &App, theme: &Theme) {
    let Some(modal) = &app.rag_transfer_modal else {
        return;
    };

    let agents = &app.interactive_agents;
    let card_h = 3u16;
    let visible_cards = agents.len().min(5) as u16;
    let list_h = if agents.is_empty() {
        1
    } else {
        visible_cards * card_h + visible_cards.saturating_sub(1)
    };
    let preview_lines = modal.context_payload.lines().count().min(4) as u16;
    let height = 7 + preview_lines + list_h;
    let area = centered_rect(70, height, frame.area());
    frame.render_widget(Clear, area);

    let block = Block::default()
        .title(" Transfer RAG Result ")
        .borders(crate::tui::ui::dialog_borders_for(theme))
        .border_style(Style::default().fg(theme.header_color))
        .style(Style::default().bg(theme.dialog_bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    draw_dialog_left_wave(frame, area, effective_animation_tick(app).into());

    let mut lines = Vec::new();
    push_rag_preview(&mut lines, modal, inner.width, theme);
    push_agent_cards(&mut lines, app, modal, inner.width, theme);
    push_rag_footer(&mut lines, theme);

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Append the query row plus the first few lines of the selected chunk preview.
fn push_rag_preview<'a>(
    lines: &mut Vec<Line<'a>>,
    modal: &'a RagTransferModal,
    inner_width: u16,
    theme: &Theme,
) {
    lines.push(Line::from(vec![
        Span::styled("  Query: ", Style::default().fg(theme.dim_text)),
        Span::styled(
            truncate_str(&modal.query, inner_width.saturating_sub(10) as usize),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Selected chunk preview:",
        Style::default().fg(theme.dim_text),
    )));

    for line in modal.context_payload.lines().skip(5).take(4) {
        lines.push(Line::from(Span::styled(
            format!(
                "  {}",
                truncate_str(line, inner_width.saturating_sub(4) as usize)
            ),
            Style::default().fg(Color::Rgb(170, 200, 170)),
        )));
    }

    lines.push(Line::from(""));
}

/// Append one three-line card per interactive agent, or an empty-state message.
fn push_agent_cards<'a>(
    lines: &mut Vec<Line<'a>>,
    app: &'a App,
    modal: &'a RagTransferModal,
    inner_width: u16,
    theme: &Theme,
) {
    // Full-width trailing filler so a selected row reads as a card spanning the
    // whole dialog (like the sidebar), not just the background behind the text.
    let row_fill =
        |bg: Color| Span::styled(" ".repeat(inner_width as usize), Style::default().bg(bg));

    let agents = &app.interactive_agents;
    if agents.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No interactive agents running.",
            Style::default().fg(theme.dim_text),
        )));
        return;
    }
    for (i, agent) in agents.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        let is_sel = i == modal.picker_selected;
        let accent = agent.accent_color;
        // Subtle selection (matches the sidebar): dark-gray background with
        // the agent name in its accent color, instead of a full accent fill.
        let bg = if is_sel {
            theme.selected_bg
        } else {
            theme.dialog_bg
        };
        let fg = if is_sel { accent } else { Color::White };
        let cursor = if is_sel { "›" } else { " " };
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", cursor), Style::default().fg(accent).bg(bg)),
            Span::styled(
                &agent.name,
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            row_fill(bg),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default().bg(bg)),
            Span::styled(
                format!("pty · {}", agent.cli.as_str()),
                Style::default().fg(theme.dim_text).bg(bg),
            ),
            row_fill(bg),
        ]));
        lines.push(Line::from(vec![
            Span::styled("    ", Style::default().bg(bg)),
            Span::styled(
                truncate_path(&agent.working_dir, inner_width.saturating_sub(6) as usize),
                Style::default().fg(Color::Cyan).bg(bg),
            ),
            row_fill(bg),
        ]));
    }
}

/// Append the blank separator and the navigation hint.
fn push_rag_footer<'a>(lines: &mut Vec<Line<'a>>, theme: &Theme) {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑↓ navigate · Enter open prompt builder · Esc cancel",
        Style::default().fg(theme.dim_text),
    )));
}

fn truncate_path(path: &str, max_chars: usize) -> String {
    if path.len() <= max_chars {
        return path.to_string();
    }
    let trimmed = &path[path.len() - max_chars.saturating_sub(1)..];
    let start = trimmed.find('/').map(|p| p + 1).unwrap_or(0);
    format!("…/{}", &trimmed[start..])
}
