use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::content::wrap_styled_content;
use super::geometry::generate_top_border;
use super::truncate_str;
use crate::tui::ui::theme::Theme;

/// Draw the Raw tab's content region: a single free-text field spanning the
/// content area, or — when the buffer is empty — a dimmed, read-only preview
/// of the composed Normal-form prompt (the exact string a send would produce),
/// clamped with a "… (+N lines)" tail when it overflows.
pub(super) fn draw_raw_tab_content(
    frame: &mut Frame,
    dialog: &crate::tui::app::dialog::SimplePromptDialog,
    accent: Color,
    inner: ratatui::layout::Rect,
    field_width: usize,
    list_panel_height: u16,
    theme: &Theme,
) {
    let content_top = inner.y + 3;
    // Leave the send line plus the scheduled-list panel (B33) at the bottom.
    let content_bottom = inner.y + inner.height.saturating_sub(2 + list_panel_height);
    let avail_h = content_bottom.saturating_sub(content_top).max(1) as usize;

    // Header/label line (row 2, the gap row above the field).
    let raw_empty = dialog.raw_is_empty();
    let label = if raw_empty {
        " Raw — preview of composed prompt (read-only) "
    } else {
        " Raw — sent exactly as typed "
    };
    let label_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(generate_top_border(label, inner.width, label_style)),
        ratatui::layout::Rect {
            x: inner.x,
            y: content_top.saturating_sub(1),
            width: inner.width,
            height: 1,
        },
    );

    let content_area = ratatui::layout::Rect {
        x: inner.x + 1,
        y: content_top,
        width: inner.width.saturating_sub(2),
        height: avail_h as u16,
    };
    let section_bg = theme.stage_bg;

    if raw_empty {
        // Read-only preview, dimmed. ↑↓/PgUp/PgDn scroll it; edge markers show
        // how much is hidden above and below the window.
        let preview = dialog
            .raw_preview
            .as_deref()
            .unwrap_or("(nothing to preview yet)");
        let all_lines: Vec<&str> = preview.lines().collect();
        let scroll = dialog
            .raw_preview_scroll
            .min(all_lines.len().saturating_sub(1));
        let mut lines: Vec<Line> = Vec::new();
        if scroll > 0 {
            lines.push(Line::from(Span::styled(
                format!("… (−{scroll} lines above)"),
                Style::default().fg(accent),
            )));
        }
        let body_h = avail_h.saturating_sub(lines.len()).saturating_sub(1).max(1);
        let shown = all_lines.len().min(scroll + body_h) - scroll;
        lines.extend(all_lines.iter().skip(scroll).take(body_h).map(|l| {
            Line::from(Span::styled(
                (*l).to_string(),
                Style::default().fg(theme.dim_text),
            ))
        }));
        let below = all_lines.len().saturating_sub(scroll + shown);
        if below > 0 {
            lines.push(Line::from(Span::styled(
                format!("… (+{below} lines, ↑↓ scroll)"),
                Style::default().fg(accent),
            )));
        }
        frame.render_widget(
            Paragraph::new(ratatui::text::Text::from(lines)),
            content_area,
        );
    } else {
        // Editable raw field: reuse the section wrapping/cursor machinery.
        let text = dialog.raw_text();
        let cursor_idx = dialog
            .cursor(crate::tui::app::dialog::RAW_SECTION_ID)
            .min(text.chars().count());
        // Cursor-follow scroll computed from the cursor's visual line,
        // overridden by wheel scroll when the user has scrolled with the mouse.
        let prefix: String = text.chars().take(cursor_idx).collect();
        let cursor_line =
            crate::tui::app::dialog::SimplePromptDialog::visual_line_count(&prefix, field_width)
                .saturating_sub(1);
        let base_scroll = cursor_line.saturating_sub(avail_h.saturating_sub(1));
        let scroll = dialog.raw_edit_scroll.unwrap_or(base_scroll) as u16;

        let styled = dialog.get_file_reference_with_styling(text, accent);
        let wrapped = wrap_styled_content(
            styled,
            Some(cursor_idx),
            field_width,
            section_bg,
            theme.text_primary,
        );
        frame.render_widget(
            Paragraph::new(ratatui::text::Text::from(wrapped)).scroll((scroll, 0)),
            content_area,
        );
    }
}

/// One scheduled-list row's text: `MM-DD HH:MM  <first line of prompt>`,
/// truncated to `width`. Pure so the rendered row and its tests agree.
pub(super) fn scheduled_row_text(
    fire_local: chrono::NaiveDateTime,
    prompt: &str,
    width: usize,
) -> String {
    let preview = prompt.lines().next().unwrap_or("").trim();
    let full = format!("{}  {}", fire_local.format("%m-%d %H:%M"), preview);
    truncate_str(&full, width)
}

/// The pending-sends list state: the entries, the focused row (only set while
/// the list has keyboard focus), the scroll offset, how many rows fit, and the
/// id currently being edited inline.
pub(super) struct ScheduledList<'a> {
    pub pending: &'a [crate::db::scheduled_prompts::ScheduledPrompt],
    pub selected: Option<usize>,
    pub scroll: usize,
    pub visible_rows: usize,
    pub editing_id: Option<&'a str>,
}

/// Draw the pending-scheduled-sends list panel (B33): a labeled top border,
/// then one row per visible entry (fire time + prompt preview). The selected
/// row (only set while the list has keyboard focus) is drawn reversed.
pub(super) fn draw_scheduled_list(
    frame: &mut Frame,
    list: &ScheduledList<'_>,
    accent: Color,
    inner: ratatui::layout::Rect,
    panel_top: u16,
    theme: &Theme,
) {
    let ScheduledList {
        pending,
        selected,
        scroll,
        visible_rows,
        editing_id,
    } = *list;
    let editing = editing_id.is_some_and(|id| pending.iter().any(|send| send.id == id));
    let label = if editing {
        format!(" Scheduled sends ({}) — editing ", pending.len())
    } else {
        format!(" Scheduled sends ({}) · Ctrl+P browse ", pending.len())
    };
    let label_style = Style::default().fg(accent).add_modifier(Modifier::BOLD);
    frame.render_widget(
        Paragraph::new(generate_top_border(&label, inner.width, label_style)),
        ratatui::layout::Rect {
            x: inner.x,
            y: panel_top,
            width: inner.width,
            height: 1,
        },
    );

    let row_width = inner.width.saturating_sub(2) as usize;
    for row in 0..visible_rows {
        let idx = scroll + row;
        let Some(send) = pending.get(idx) else {
            break;
        };
        let fire_local = send.fire_at.with_timezone(&chrono::Local).naive_local();
        let text = scheduled_row_text(fire_local, &send.prompt, row_width);
        let is_selected = selected == Some(idx);
        let style = if is_selected {
            Style::default()
                .fg(accent)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            Style::default().fg(theme.text_primary)
        };
        let marker = if is_selected { "› " } else { "  " };
        let line = Line::from(vec![
            Span::styled(marker, Style::default().fg(accent)),
            Span::styled(text, style),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            ratatui::layout::Rect {
                x: inner.x,
                y: panel_top + 1 + row as u16,
                width: inner.width,
                height: 1,
            },
        );
    }
}
