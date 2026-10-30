use super::*;

// ── Focus/border styling ────────────────────────────────────────────

pub(super) fn is_rag_focused(app: &App) -> bool {
    matches!(app.focus, Focus::Home | Focus::Preview)
        && app.agents_rag_focused
        && !app.playground_active
}

pub(super) fn rag_info_title(app: &App) -> &'static str {
    if app.rag_paused {
        " ragInfo ‖ "
    } else {
        " ragInfo "
    }
}

pub(super) fn rag_border_style(app: &App, theme: &Theme) -> Style {
    Style::default().fg(if is_rag_focused(app) {
        theme.header_color
    } else {
        theme.border_color
    })
}

// ── RAG (pinned top) ─────────────────────────────────────────────────

pub(super) fn draw_rag_queue(
    frame: &mut Frame,
    area: Rect,
    items: &[crate::db::project::RagQueueItem],
    scroll_pos: usize,
    theme: &Theme,
) {
    let mut y = area.y;
    for (idx, item) in items.iter().enumerate() {
        if y >= area.y + area.height {
            break;
        }
        let (icon, icon_color) = if item.status == "processing" {
            ("◉", theme.warning)
        } else {
            ("·", theme.header_color)
        };
        let is_cursor = idx == scroll_pos;
        let prefix = if is_cursor { "›" } else { " " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, Style::default().fg(theme.header_color)),
                Span::styled(icon, Style::default().fg(icon_color)),
                Span::raw(" "),
                Span::styled(
                    truncate_str(&item.source_path, area.width.saturating_sub(3) as usize),
                    Style::default().fg(theme.text_primary),
                ),
            ])),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;
        if y < area.y + area.height {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    truncate_str(
                        &last_two_segments(&item.source_path),
                        area.width.saturating_sub(3) as usize,
                    ),
                    Style::default().fg(theme.dim_text),
                ))),
                Rect::new(area.x + 2, y, area.width.saturating_sub(2), 1),
            );
            y += 1;
        }
    }
}

pub(super) fn draw_rag_info(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let mut lines = vec![
        labeled_kv_line(" chunks: ", &app.rag_info.total_chunks.to_string(), theme),
        labeled_kv_line(" files:  ", &app.rag_info.indexed_files.to_string(), theme),
    ];

    let queue_text = rag_queue_text(app);
    if !queue_text.is_empty() {
        lines.push(labeled_kv_line(" queue:  ", &queue_text, theme));
    }

    lines.push(rag_status_line(app, theme));
    lines.push(Line::from(Span::styled(
        " Enter → playground ",
        Style::default().fg(theme.header_color),
    )));

    frame.render_widget(Paragraph::new(lines), area);

    if app.rag_info.total_chunks == 0 && !app.global_rag_queue.is_empty() && area.height > 5 {
        let queue_area = Rect::new(
            area.x,
            area.y + 5,
            area.width,
            area.height.saturating_sub(5),
        );
        draw_rag_queue(
            frame,
            queue_area,
            &app.global_rag_queue,
            app.selected_rag_queue,
            theme,
        );
    }
}

pub(super) fn labeled_kv_line(label: &'static str, value: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(label, Style::default().fg(theme.dim_text)),
        Span::styled(
            value.to_string(),
            Style::default()
                .fg(theme.text_primary)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

pub(super) fn rag_status_line(app: &App, theme: &Theme) -> Line<'static> {
    use crate::rag::status::{compute_rag_status, RagModelStatus};

    match compute_rag_status(
        &app.rag_embeddings_model,
        app.rag_paused,
        app.rag_model_loaded,
        app.rag_info.processing_items,
        app.rag_acquisition_state.clone(),
    ) {
        RagModelStatus::Unavailable(_) => Line::from(Span::styled(
            " ✗ unavailable ",
            Style::default().fg(theme.error),
        )),
        RagModelStatus::DownloadFailed(_) => Line::from(Span::styled(
            " ✗ download failed ",
            Style::default().fg(theme.error),
        )),
        RagModelStatus::Downloading { .. } => Line::from(Span::styled(
            " ⬇ downloading ",
            Style::default().fg(theme.warning),
        )),
        RagModelStatus::Preparing { .. } => Line::from(Span::styled(
            " ⚙ preparing ",
            Style::default().fg(theme.warning),
        )),
        RagModelStatus::Paused => Line::from(Span::styled(
            " ‖ paused ",
            Style::default().fg(theme.warning),
        )),
        RagModelStatus::Ready if app.rag_info.processing_items > 0 => Line::from(Span::styled(
            " ◉ indexing ",
            Style::default().fg(theme.warning),
        )),
        RagModelStatus::Ready => Line::from(Span::styled(
            " ● ready ",
            Style::default().fg(theme.header_color),
        )),
        RagModelStatus::Sleeping => Line::from(Span::styled(
            " ○ sleeping ",
            Style::default().fg(theme.dim_text),
        )),
    }
}

pub(super) fn rag_queue_text(app: &App) -> String {
    if app.rag_info.queued_items > 0 {
        format!("{} queued", app.rag_info.queued_items)
    } else {
        String::new()
    }
}
