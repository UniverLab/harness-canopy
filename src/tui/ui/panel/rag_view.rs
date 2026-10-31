use super::*;

pub(super) fn rag_status(app: &App, theme: &Theme) -> (&'static str, Color) {
    use crate::rag::status::{compute_rag_status, RagModelStatus};

    match compute_rag_status(
        &app.rag_embeddings_model,
        app.rag_paused,
        app.rag_model_loaded,
        app.rag_info.processing_items,
        app.rag_acquisition_state.clone(),
    ) {
        RagModelStatus::Unavailable(_) => ("✗ unavailable", Color::Red),
        RagModelStatus::DownloadFailed(_) => ("✗ download failed", Color::Red),
        RagModelStatus::Downloading { .. } => ("▼ downloading", Color::Yellow),
        RagModelStatus::Preparing { .. } => ("◉ preparing", Color::Yellow),
        RagModelStatus::Paused => ("‖ paused", Color::Yellow),
        RagModelStatus::Ready if app.rag_info.processing_items > 0 => ("◉ indexing", Color::Yellow),
        RagModelStatus::Ready => ("● ready", theme.header_color),
        RagModelStatus::Sleeping => ("○ sleeping", theme.dim_text),
    }
}

pub(super) fn rag_queue_text(app: &App) -> String {
    if app.rag_info.queued_items > 0 {
        format!("{} queued", app.rag_info.queued_items)
    } else {
        String::new()
    }
}

pub(super) fn rag_summary_lines(
    app: &App,
    status_text: &'static str,
    status_color: Color,
    queue_text: String,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(vec![
            Span::styled("Chunks: ", Style::default().fg(theme.dim_text)),
            Span::styled(
                app.rag_info.total_chunks.to_string(),
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled("Files: ", Style::default().fg(theme.dim_text)),
            Span::styled(
                app.rag_info.indexed_files.to_string(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        labeled_value_line(
            "Status: ",
            Span::styled(status_text, Style::default().fg(status_color)),
            theme,
        ),
    ];
    if !queue_text.is_empty() {
        lines.push(labeled_value_line(
            "Queue:  ",
            Span::styled(queue_text, Style::default().fg(Color::White)),
            theme,
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Press Enter to open the global RAG playground.",
        Style::default().fg(theme.header_color),
    )));
    lines
}

pub(super) fn draw_rag_info_overview(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let (status_text, status_color) = rag_status(app, theme);
    let mut lines = rag_summary_lines(app, status_text, status_color, rag_queue_text(app), theme);

    match crate::rag::status::compute_rag_status(
        &app.rag_embeddings_model,
        app.rag_paused,
        app.rag_model_loaded,
        app.rag_info.processing_items,
        app.rag_acquisition_state.clone(),
    ) {
        crate::rag::status::RagModelStatus::Unavailable(reason) => {
            lines.push(Line::from(Span::styled(
                format!("  {reason}"),
                Style::default().fg(Color::Red),
            )));
        }
        crate::rag::status::RagModelStatus::Downloading { started_at } => {
            lines.push(Line::from(Span::styled(
                format!(
                    "  downloading model ({}s so far)",
                    crate::rag::status::elapsed_secs(started_at)
                ),
                Style::default().fg(Color::Yellow),
            )));
        }
        crate::rag::status::RagModelStatus::Preparing { started_at } => {
            lines.push(Line::from(Span::styled(
                format!(
                    "  preparing model ({}s so far)",
                    crate::rag::status::elapsed_secs(started_at)
                ),
                Style::default().fg(Color::Yellow),
            )));
        }
        crate::rag::status::RagModelStatus::DownloadFailed(reason) => {
            lines.push(Line::from(Span::styled(
                format!("  download failed: {reason} — retry: canopy rag model retry"),
                Style::default().fg(Color::Red),
            )));
        }
        _ => {}
    }

    if !app.rag_file_status.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled("Recent files  ", Style::default().fg(theme.dim_text)),
            Span::styled(
                format!("({}) ", app.rag_file_status.len()),
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.extend(rag_file_status_lines(&app.rag_file_status, area, theme));
    } else if !app.global_rag_queue.is_empty() {
        lines.extend(rag_queue_lines(
            &app.global_rag_queue,
            app.selected_rag_queue,
            theme,
        ));
    }

    render_wrapped_paragraph(frame, area, lines);
}

pub(super) fn rag_file_icon_and_color(event_type: &str, theme: &Theme) -> (&'static str, Color) {
    match event_type {
        "indexed" => ("✓", theme.success),
        "deleted" => ("○", theme.dim_text),
        _ => ("✗", Color::Red),
    }
}

pub(super) fn rag_file_detail(
    file: &crate::db::project::RagPerFileStatus,
    detail_width: usize,
) -> String {
    if file.last_event_type == "error" {
        file.last_detail
            .as_deref()
            .map(|d| format!("error: {}", truncate_str_keep_tail(d, detail_width)))
            .unwrap_or_else(|| "error".to_string())
    } else if file.last_event_type == "deleted" {
        "deleted".to_string()
    } else {
        format!("indexed ×{}", file.times_indexed)
    }
}

pub(super) fn rag_file_entry_lines(
    file: &crate::db::project::RagPerFileStatus,
    name_width: usize,
    detail_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let (icon, icon_color) = rag_file_icon_and_color(&file.last_event_type, theme);
    let filename = std::path::Path::new(&file.file_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| file.file_path.clone());
    let name_trunc = truncate_str(&filename, name_width);
    let detail = rag_file_detail(file, detail_width);

    vec![
        Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(icon_color)),
            Span::styled(
                name_trunc,
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("   ", Style::default().fg(theme.dim_text)),
            Span::styled(detail, Style::default().fg(theme.dim_text)),
        ]),
    ]
}

pub(super) fn rag_file_status_lines(
    files: &[crate::db::project::RagPerFileStatus],
    area: Rect,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let max_rows = (area.height as usize).saturating_sub(8).max(2);
    let name_width = area.width.saturating_sub(4) as usize;
    let detail_width = area.width.saturating_sub(6) as usize;

    let mut lines = Vec::new();
    for file in files.iter().take(max_rows) {
        lines.extend(rag_file_entry_lines(file, name_width, detail_width, theme));
    }

    if max_rows < files.len() {
        let remaining = files.len() - max_rows;
        lines.push(Line::from(Span::styled(
            format!("  … {} more (open playground for full details)", remaining),
            Style::default().fg(theme.dim_text),
        )));
    }
    lines
}

pub(super) fn rag_queue_lines(
    queue: &[crate::db::project::RagQueueItem],
    selected: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("Queue items ", Style::default().fg(theme.dim_text)),
            Span::styled(
                format!("({})", queue.len()),
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    for (idx, item) in queue.iter().enumerate().take(8) {
        let (line_style, marker) = selected_row_style(idx == selected, theme);
        let status_color = if item.status == "processing" {
            Color::Yellow
        } else {
            theme.header_color
        };
        lines.push(Line::from(vec![
            Span::styled(marker, line_style.fg(status_color)),
            Span::raw(" "),
            Span::styled(
                truncate_str(&item.source_path, 40),
                line_style.fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", item.status), line_style.fg(theme.dim_text)),
        ]));
    }

    lines
}
