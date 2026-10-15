use super::*;

pub(super) fn draw_playground_panel(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.playground_detail_mode {
        draw_playground_detail(frame, area, app, theme);
    } else {
        draw_playground_list(frame, area, app, theme);
    }
}

pub(super) fn playground_header_lines(app: &App, theme: &Theme) -> Vec<Line<'static>> {
    let scope_label = playground_scope_label(app);
    let query = &app.playground_query;

    let mut header = vec![Line::from(vec![
        Span::styled("RAG Playground ", Style::default().fg(theme.dim_text)),
        Span::styled(
            format!("({scope_label}) "),
            Style::default().fg(Color::Yellow),
        ),
        Span::styled(
            format!("· {query}"),
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ),
    ])];

    if app.playground_search_pending {
        header.push(Line::from(vec![
            Span::styled("  ◉ Searching", Style::default().fg(Color::Yellow)),
            Span::styled(
                " · press Esc to cancel",
                Style::default().fg(theme.dim_text),
            ),
        ]));
    } else if !app.playground_results.is_empty() {
        header.push(Line::from(vec![
            Span::styled("  ✓ ", Style::default().fg(theme.header_color)),
            Span::styled(
                format!("{} results", app.playground_results.len()),
                Style::default().fg(theme.header_color),
            ),
        ]));
    }

    header.push(Line::from(Span::styled(
        "Type to search · ↑↓ navigate · Tab toggle scope · Enter focus · Ctrl+T transfer · Esc close",
        Style::default().fg(theme.dim_text),
    )));
    header.push(Line::from(""));

    header
}

pub(super) fn playground_empty_state(app: &App, theme: &Theme) -> Line<'static> {
    let message = if app.playground_query.trim().is_empty() {
        "Start typing to search indexed chunks."
    } else if app.playground_search_pending {
        "◉ Searching..."
    } else {
        "No matching chunks."
    };
    let color = if app.playground_search_pending {
        Color::Yellow
    } else {
        theme.dim_text
    };
    Line::from(Span::styled(message, Style::default().fg(color)))
}

pub(super) fn visible_playground_window(
    area: Rect,
    selected: usize,
    total: usize,
) -> (usize, usize) {
    let max_visible = ((area.height.saturating_sub(4)) / 5).max(1) as usize;
    let scroll_start = crate::tui::selection::clamp_scroll(selected, 0, total, max_visible);
    (max_visible, scroll_start)
}

pub(super) fn project_name_for_chunk<'a>(
    app: &'a App,
    _chunk: &crate::rag::vector_store::SearchResult,
) -> &'a str {
    app.projects
        .first()
        .map(|project| project.name.as_str())
        .unwrap_or("?")
}

pub(super) fn draw_playground_list(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let mut lines = playground_header_lines(app, theme);

    if app.playground_results.is_empty() {
        lines.push(playground_empty_state(app, theme));
        render_wrapped_paragraph(frame, area, lines);
        return;
    }

    let total = app.playground_results.len();
    let (max_visible, scroll_start) =
        visible_playground_window(area, app.playground_selected, total);
    for (idx, chunk) in app
        .playground_results
        .iter()
        .enumerate()
        .skip(scroll_start)
        .take(max_visible)
    {
        lines.extend(render_chunk_entry(
            chunk,
            project_name_for_chunk(app, chunk),
            idx == app.playground_selected,
            area.width,
            theme,
        ));
    }

    if total > max_visible {
        lines.push(Line::from(Span::styled(
            format!("  {}/{} results", app.playground_selected + 1, total),
            Style::default().fg(theme.dim_text),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("  {} results", total),
            Style::default().fg(theme.dim_text),
        )));
    }

    render_wrapped_paragraph(frame, area, lines);
}

pub(super) fn playground_scope_label(app: &App) -> String {
    app.playground_project_hash
        .as_ref()
        .and_then(|hash| app.projects.iter().find(|p| &p.hash == hash))
        .map(|p| format!("Project: {}", p.name))
        .unwrap_or_else(|| "Global".to_string())
}

pub(super) fn render_chunk_entry<'a>(
    chunk: &'a crate::rag::vector_store::SearchResult,
    project_name: &'a str,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'a>> {
    let (style, marker) = selected_row_style(selected, theme);
    let dist = chunk
        .distance
        .map_or("—".to_string(), |d| format!("{d:.3}"));
    let path = format!("{} · {} [dist={}]", project_name, chunk.file_path, dist);
    let mut lines = vec![Line::from(vec![
        Span::styled(marker, style.fg(theme.header_color)),
        Span::raw(" "),
        Span::styled(
            truncate_str(&path, width.saturating_sub(3) as usize),
            style.fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ])];

    for line in chunk.content.lines().take(3) {
        lines.push(Line::from(vec![
            Span::styled("   ", style),
            Span::styled(
                truncate_str(line, width.saturating_sub(6) as usize),
                style.fg(theme.dim_text),
            ),
        ]));
    }

    lines.push(Line::from(""));
    lines
}

pub(super) fn playground_detail_header(
    chunk: &crate::rag::vector_store::SearchResult,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let dist = chunk
        .distance
        .map_or("—".to_string(), |d| format!("{d:.4}"));
    vec![
        Line::from(vec![
            Span::styled("‹ ", Style::default().fg(theme.header_color)),
            Span::styled(
                format!("{} [dist={}]", chunk.file_path, dist),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(Span::styled(
            "↑↓ scroll · Enter/Ctrl+T transfer · Esc back to list",
            Style::default().fg(theme.dim_text),
        )),
        Line::from(""),
    ]
}

pub(super) fn detail_progress_line(
    total_lines: usize,
    visible_lines: usize,
    start: usize,
    theme: &Theme,
) -> Option<Line<'static>> {
    if total_lines <= visible_lines {
        return None;
    }

    let percent = ((start.saturating_add(visible_lines)).min(total_lines) * 100)
        .checked_div(total_lines)
        .unwrap_or(100)
        .min(100);
    Some(Line::from(Span::styled(
        format!("  ── {percent}% ──"),
        Style::default().fg(theme.dim_text),
    )))
}

pub(super) fn draw_playground_detail(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(chunk) = app.playground_results.get(app.playground_selected) else {
        return;
    };

    let mut lines = playground_detail_header(chunk, theme);
    let content_lines: Vec<&str> = chunk.content.lines().collect();
    let visible_lines = area.height.saturating_sub(5) as usize;
    let start = (app.playground_scroll as usize).min(content_lines.len().saturating_sub(1));
    let end = (start + visible_lines).min(content_lines.len());

    for line in &content_lines[start..end] {
        lines.push(Line::from(Span::styled(
            truncate_str(line, area.width as usize),
            Style::default().fg(Color::White),
        )));
    }

    if let Some(progress) = detail_progress_line(content_lines.len(), visible_lines, start, theme) {
        lines.push(Line::from(""));
        lines.push(progress);
    }

    render_wrapped_paragraph(frame, area, lines);
}
