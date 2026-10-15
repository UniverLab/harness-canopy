use super::*;

pub(super) fn format_intent_lines(
    state: &crate::tui::app::types::SyncPanelState,
    _area_width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if state.active_intents.is_empty() {
        lines.push(Line::from("active missions: none"));
        return lines;
    }

    lines.push(Line::from("active missions:"));
    for intent in state.active_intents.iter().take(3) {
        lines.push(Line::from(format!(
            "  - {} [{}]: {}",
            intent.agent_name,
            intent.impact.as_str(),
            intent.mission
        )));
        if !intent.description.trim().is_empty() {
            lines.push(Line::from(Span::styled(
                format!("    {}", truncate_str(intent.description.trim(), 92)),
                Style::default().fg(theme.dim_text),
            )));
        }
    }
    lines
}

pub(super) fn format_recent_activity_lines(
    state: &crate::tui::app::types::SyncPanelState,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let recent_messages = state
        .recent_messages
        .iter()
        .rev()
        .take(3)
        .collect::<Vec<_>>();

    if recent_messages.is_empty() {
        lines.push(Line::from("recent activity: none"));
        return lines;
    }

    lines.push(Line::from("recent activity:"));
    for message in recent_messages {
        lines.push(Line::from(format!(
            "  - {}: {}",
            message.agent_name,
            truncate_str(message.message.trim(), 92)
        )));
    }
    lines
}

pub(super) fn format_recent_session_lines(
    sessions: &[(String, String)],
    theme: &Theme,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if sessions.is_empty() {
        lines.push(Line::from("  none"));
        return lines;
    }

    for (title, summary) in sessions {
        lines.push(Line::from(format!("  - {}", truncate_str(title, 92))));
        lines.push(Line::from(Span::styled(
            format!("    {}", summary),
            Style::default().fg(theme.dim_text),
        )));
    }
    lines
}

pub(super) fn build_project_overview_lines<'a>(
    project: &'a crate::domain::project::Project,
    project_activity: Option<&crate::tui::app::types::SyncPanelState>,
    recent_sessions: &[(String, String)],
    theme: &Theme,
    datetime_format: &str,
) -> Vec<Line<'a>> {
    let tags = project.tags.as_deref().unwrap_or("none");
    let indexed = project
        .indexed_at
        .map(|ts| format_unix_timestamp(ts, datetime_format))
        .unwrap_or_else(|| "pending".to_string());
    let created = format_unix_timestamp(project.created_at, datetime_format);
    let description = project
        .description
        .as_deref()
        .unwrap_or("No description extracted yet.");

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Project ", Style::default().fg(theme.dim_text)),
            Span::styled(
                &project.name,
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        overview_meta_line("workdir_hash", &project.hash, theme),
        overview_meta_line("path", &project.path, theme),
        overview_meta_line("indexed_at", &indexed, theme),
        overview_meta_line("created_at", &created, theme),
        overview_meta_line("tags", tags, theme),
        Line::from(""),
        overview_heading("Description", theme),
        Line::from(description),
        Line::from(""),
        overview_heading("Workspace context", theme),
    ];

    if let Some(state) = project_activity {
        lines.push(Line::from(format!(
            "participants: {}  vibe: {}",
            state.participant_count,
            state.vibe.as_str()
        )));
        lines.extend(format_intent_lines(state, 0, theme));
        lines.extend(format_recent_activity_lines(state));
    } else {
        lines.push(Line::from("participants: 0  vibe: stable"));
        lines.push(Line::from("active missions: none"));
        lines.push(Line::from("recent activity: none"));
    }

    lines.push(Line::from(""));
    lines.push(overview_heading("Recent sessions", theme));
    lines.extend(format_recent_session_lines(recent_sessions, theme));

    lines
}

/// Section heading on the Overview tab — `header_color` with BOLD, each
/// preceded by a blank line, so a label reads as a heading instead of
/// blending into the white body text (functional requirement 4).
fn overview_heading(text: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default()
            .fg(theme.header_color)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Key/value metadata line on the Overview tab: the key in `dim_text`, the
/// value in the normal ink (functional requirement 4).
fn overview_meta_line(key: &str, value: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{key}: "), Style::default().fg(theme.dim_text)),
        Span::styled(value.to_string(), Style::default().fg(Color::White)),
    ])
}

pub(super) fn draw_project_overview(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(project) = app.selected_project() else {
        frame.render_widget(
            Paragraph::new("No registered projects").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    };

    let project_activity = app.activity_panel_state_for_workdir(&project.path);
    let recent_sessions = recent_project_session_summaries(app, project, 3);
    let lines = build_project_overview_lines(
        project,
        project_activity.as_ref(),
        &recent_sessions,
        theme,
        &app.tui_settings.datetime_format,
    );

    render_wrapped_paragraph(frame, area, lines);
}

/// Knowledge layer's Preview (project highlighted, not entered): a cheap
/// summary card — pending backlog count, knowledge entry count, last
/// activity, and a badge if a graph is running against this project's
/// workdir (functional requirement 3). Reads `App::selected_project_preview`,
/// a cache refreshed on the normal tick cadence — never recomputed here.
pub(super) fn draw_project_preview_card(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.playground_active {
        draw_playground_panel(frame, area, app, theme);
        return;
    }

    let Some(project) = app.selected_project() else {
        frame.render_widget(
            Paragraph::new("No registered projects").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    };

    let summary = app.selected_project_preview();
    let running_badge = if summary.is_some_and(|s| s.graph_running) {
        Span::styled("  ● graph running", Style::default().fg(STATUS_RUNNING))
    } else {
        Span::raw("")
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Project ", Style::default().fg(theme.dim_text)),
            Span::styled(
                &project.name,
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
            running_badge,
        ]),
        Line::from(format!("path: {}", project.path)),
        Line::from(""),
    ];

    match summary {
        Some(summary) => {
            lines.push(Line::from(vec![
                Span::styled("Backlog: ", Style::default().fg(theme.dim_text)),
                Span::styled(
                    summary.pending_backlog.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("   Knowledge: ", Style::default().fg(theme.dim_text)),
                Span::styled(
                    summary.knowledge_entries.to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            let last_activity = summary
                .last_activity
                .and_then(|ts| chrono::DateTime::from_timestamp(ts, 0))
                .map(|dt| crate::tui::app::utils::relative_time(&dt))
                .unwrap_or_else(|| "no activity yet".to_string());
            lines.push(Line::from(vec![
                Span::styled("Last activity: ", Style::default().fg(theme.dim_text)),
                Span::styled(last_activity, Style::default().fg(Color::White)),
            ]));
        }
        None => lines.push(Line::from(Span::styled(
            "Summary loading…",
            Style::default().fg(theme.dim_text),
        ))),
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Enter → Overview | Backlog | Knowledge | History",
        Style::default().fg(theme.header_color),
    )));

    render_wrapped_paragraph(frame, area, lines);
}

/// Knowledge layer's Focus (project entered, `Enter`): the tab bar —
/// Overview | Backlog | Knowledge | History — plus the active tab's lazily
/// loaded content (functional requirement 4). Populates
/// `project_tab_click_map`/`project_tab_row_click_map` for mouse
/// hit-testing, reusing the same click-map pattern as the sidebar.
pub(super) fn draw_project_tabs_panel(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    app.project_tab_click_map.clear();
    app.project_tab_row_click_map.clear();

    if area.height == 0 {
        return;
    }
    let Some(project) = app.selected_project().cloned() else {
        frame.render_widget(
            Paragraph::new("No registered projects").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    };
    let Some(active_tab) = app.project_focus else {
        return;
    };

    let [tab_bar_area, content_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);

    draw_project_tab_bar(frame, tab_bar_area, app, &project.name, active_tab, theme);

    match active_tab {
        ProjectTab::Overview => draw_project_overview(frame, content_area, app, theme),
        ProjectTab::Backlog => draw_backlog_overview(frame, content_area, app, theme),
        ProjectTab::Knowledge => draw_knowledge_overview(frame, content_area, app, theme),
        ProjectTab::History => draw_project_history_tab(frame, content_area, app, theme),
    }
}

pub(super) fn draw_project_tab_bar(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    project_name: &str,
    active_tab: ProjectTab,
    theme: &Theme,
) {
    let mut spans = vec![
        Span::styled(" ", Style::default()),
        Span::styled(
            project_name,
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    let mut x = area.x
        + spans
            .iter()
            .map(|s| s.content.chars().count() as u16)
            .sum::<u16>();

    for tab in ProjectTab::ALL {
        let label = format!(" {} ", tab.label());
        let start = x;
        let selected = tab == active_tab;
        spans.push(Span::styled(
            label.clone(),
            if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(theme.header_color)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.dim_text)
            },
        ));
        let width = label.chars().count() as u16;
        app.project_tab_click_map.push((tab, start, start + width));
        x += width;
        spans.push(Span::raw(" "));
        x += 1;
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Persisted per-project History tab: finished graphs + past sessions for
/// this project's workdir, read straight from the DB (functional
/// requirement 5) — see `App::selected_project_history_entries`.
pub(super) fn draw_project_history_tab(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) {
    let selected = app.selected_project_history;
    let entries = app.selected_project_history_entries().to_vec();
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new("No history yet for this project.")
                .style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    }

    let visible_count = (area.height as usize).min(entries.len());
    for (y, (idx, entry)) in (area.y..).zip(entries.iter().take(visible_count).enumerate()) {
        let is_selected = idx == selected;
        let (style, marker) = selected_row_style(is_selected, theme);
        let kind_label = match entry.kind {
            crate::db::project::ProjectHistoryKind::Graph => "graph",
            crate::db::project::ProjectHistoryKind::InteractiveSession => "session",
            crate::db::project::ProjectHistoryKind::TerminalSession => "terminal",
        };
        let when = chrono::DateTime::from_timestamp(entry.at, 0)
            .map(|dt| crate::tui::app::utils::relative_time(&dt))
            .unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(marker, style.fg(theme.header_color)),
            Span::raw(" "),
            Span::styled(format!("[{kind_label}] "), style.fg(theme.dim_text)),
            Span::styled(
                truncate_str(&entry.name, area.width.saturating_sub(20) as usize),
                style.fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  {} · {}", entry.status, when),
                style.fg(theme.dim_text),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        app.project_tab_row_click_map.push((idx, y, y + 1));
    }
}

/// Read-only preview of the selected backlog spec — name + description,
/// same "focus already previews it" convention as `draw_graph_overview` and
/// `draw_knowledge_overview` (no dedicated confirm step needed).
pub(super) fn draw_backlog_overview(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.backlog_specs.is_empty() {
        frame.render_widget(
            Paragraph::new("No backlog specs yet").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    }

    let Some(spec) = app.backlog_specs.get(app.selected_backlog) else {
        frame.render_widget(
            Paragraph::new("No backlog spec selected").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("Backlog ", Style::default().fg(theme.dim_text)),
            Span::styled(
                spec.name.as_str(),
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];

    match spec.description.as_deref() {
        Some(description) if !description.is_empty() => {
            for line in description.lines() {
                lines.push(Line::from(Span::styled(
                    line,
                    Style::default().fg(Color::White),
                )));
            }
        }
        _ => lines.push(Line::from(Span::styled(
            "(no description)",
            Style::default().fg(theme.dim_text),
        ))),
    }

    render_wrapped_paragraph(frame, area, lines);
}

pub(super) fn draw_knowledge_overview(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    let filtered = app.filtered_knowledge_indices();
    if filtered.is_empty() {
        let message = if app.project_knowledge.is_empty() {
            "No knowledge yet. Agents can add facts/patterns."
        } else {
            "No knowledge matches the filter."
        };
        frame.render_widget(
            Paragraph::new(message).style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    }

    let height = area.height as usize;
    let selected_pos = filtered
        .iter()
        .position(|&idx| idx == app.selected_knowledge)
        .unwrap_or(0);
    let offset = selected_pos.saturating_sub(height.saturating_sub(1));

    for (row, display_idx) in (offset..filtered.len()).take(height).enumerate() {
        let node_idx = filtered[display_idx];
        let is_selected = node_idx == app.selected_knowledge;
        let (style, marker) = selected_row_style(is_selected, theme);

        let node = &app.project_knowledge[node_idx];
        let kind_color = if node.kind == "fact" {
            Color::Cyan
        } else {
            Color::Magenta
        };
        let when = chrono::DateTime::from_timestamp(node.updated_at, 0)
            .map(|dt| crate::tui::app::utils::relative_time(&dt))
            .unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(marker, style.fg(theme.header_color)),
            Span::raw(" "),
            Span::styled(format!("[{}] ", node.kind), style.fg(kind_color)),
            Span::styled(
                truncate_str(&node.title, area.width.saturating_sub(24) as usize),
                style.fg(Color::White).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {when}"), style.fg(theme.dim_text)),
        ]);

        let y = area.y + row as u16;
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
        app.project_tab_row_click_map.push((display_idx, y, y + 1));
    }

    if filtered.len() > height {
        let label = format!("{} / {}", selected_pos + 1, filtered.len());
        let width = label.chars().count() as u16;
        if width <= area.width {
            frame.render_widget(
                Paragraph::new(label).style(Style::default().fg(theme.dim_text)),
                Rect::new(area.x + area.width - width, area.y, width, 1),
            );
        }
    }
}
