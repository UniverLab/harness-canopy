use super::*;

pub(super) fn draw_knowledge_body(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> Rect {
    let mut remaining = area;
    if let Some(sub) = take_top(&mut remaining, project_list_demand(app.projects.len())) {
        render_titled_panel(
            frame,
            sub,
            " projects ",
            Style::default().fg(theme.dim_text),
            knowledge_border_style(app, theme),
            theme,
            |frame, inner| draw_projects_list(frame, inner, app, theme),
        );
    }
    draw_sidebar_gap(frame, &mut remaining, theme);

    remaining
}

// ── Knowledge layer: projects list ──────────────────────────────────

pub(super) fn draw_projects_list(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if app.projects.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No registered projects",
                Style::default().fg(theme.muted_text),
            ))),
            area,
        );
        return;
    }

    let scroll = scroll_state(
        app.projects.len(),
        Some(app.selected_project),
        (area.height / 4).max(1) as usize,
    );
    let panel_focused = knowledge_border_style_is_focused(app);
    let mut y = area.y;
    let row_h = 4u16;

    // Collected up front (rather than iterating `app.projects` directly) so
    // the graph body can also push into `app.project_click_map` — mirrors
    // `draw_automation_graphs_list`'s `graph_ids_and_meta` pattern, since both
    // borrow `app` mutably for the click map alongside the data being drawn.
    let visible: Vec<(usize, String, String, String)> = app
        .projects
        .iter()
        .enumerate()
        .skip(scroll.start)
        .take(scroll.max_visible)
        .map(|(idx, project)| {
            (
                idx,
                project.name.clone(),
                project.hash.clone(),
                last_two_segments(&project.path),
            )
        })
        .collect();

    for (idx, name, hash, path) in &visible {
        if y + 3 > area.y + area.height {
            break;
        }
        draw_project_graph_card(
            frame,
            Rect::new(area.x, y, area.width, 3),
            *idx == app.selected_project,
            name,
            hash,
            path,
            panel_focused,
            theme,
        );
        app.project_click_map.push((*idx, y, y + 3));
        y += row_h;
    }

    draw_scroll_indicators(frame, area, scroll.has_up, scroll.has_down, theme);
}

#[allow(clippy::too_many_arguments)]
pub(super) fn draw_project_graph_card(
    frame: &mut Frame,
    area: Rect,
    selected: bool,
    title: &str,
    meta1: &str,
    meta2: &str,
    panel_focused: bool,
    theme: &Theme,
) {
    let bg = if selected {
        theme.selected_bg
    } else {
        Color::Reset
    };
    let title_style = project_title_style(selected, panel_focused, theme);
    let meta_style = project_meta_style(selected, theme);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(title, area.width as usize),
            title_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(meta1, area.width as usize),
            meta_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(meta2, area.width as usize),
            meta_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );
}

pub(super) fn knowledge_border_style(app: &App, theme: &Theme) -> Style {
    let focused = layer_focused(app, SidebarLayer::Knowledge);
    Style::default().fg(if focused {
        theme.header_color
    } else {
        theme.border_color
    })
}

pub(super) fn knowledge_border_style_is_focused(app: &App) -> bool {
    layer_focused(app, SidebarLayer::Knowledge)
}

/// Rows needed for the projects panel: 3-row cards + 1-row gap + 2-row
/// border (see `draw_projects_list`), with a 3-row floor so the panel (and
/// its "No registered projects" message) still shows when the list is
/// empty. Like `card_list_demand`, a workspace with few projects leaves
/// rows over for the project graph and Brian's Brain below it (C26) —
/// unlike Knowledge's old behavior of always claiming the whole area,
/// which made the graph panel unreachable no matter how many relations
/// existed.
pub(super) fn project_list_demand(count: usize) -> u16 {
    if count == 0 {
        3
    } else {
        count as u16 * 4 + 2
    }
}

pub(super) fn project_title_style(selected: bool, panel_focused: bool, theme: &Theme) -> Style {
    if selected && panel_focused {
        return Style::default()
            .fg(theme.accent_fg)
            .bg(theme.header_color)
            .add_modifier(Modifier::BOLD);
    }
    if selected {
        return Style::default()
            .fg(theme.text_primary)
            .bg(theme.selected_bg)
            .add_modifier(Modifier::BOLD);
    }
    Style::default()
        .fg(theme.header_color)
        .add_modifier(Modifier::BOLD)
}

pub(super) fn project_meta_style(selected: bool, theme: &Theme) -> Style {
    if selected {
        Style::default()
            .fg(theme.text_primary)
            .bg(theme.selected_bg)
    } else {
        Style::default().fg(theme.dim_text)
    }
}
