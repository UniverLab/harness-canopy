use super::*;

use crate::domain::project::Project;

pub(super) fn draw_knowledge_body(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) -> Rect {
    let mut remaining = area;
    let title = project_list_title(app);
    if let Some(sub) = take_top(&mut remaining, project_list_demand(app.projects.len())) {
        render_titled_panel(
            frame,
            sub,
            &title,
            Style::default().fg(theme.dim_text),
            knowledge_border_style(app, theme),
            theme,
            |frame, inner| draw_projects_list(frame, inner, app, theme),
        );
    }
    draw_sidebar_gap(frame, &mut remaining, theme);

    remaining
}

/// The panel title, carrying the live type-ahead so it always shows what the
/// list is being filtered by.
fn project_list_title(app: &App) -> String {
    if app.project_filter.is_empty() {
        " projects ".to_string()
    } else {
        format!(" projects / {} ", app.project_filter)
    }
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

    // The type-ahead narrows the list — and so, with it, the scroll, the
    // click regions and what the arrows walk over.
    let visible = app.visible_project_indices();
    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("No project matches \"{}\"", app.project_filter),
                Style::default().fg(theme.muted_text),
            ))),
            area,
        );
        return;
    }

    let selected_pos = visible.iter().position(|&idx| idx == app.selected_project);
    let scroll = scroll_state(
        visible.len(),
        selected_pos,
        (area.height / 4).max(1) as usize,
    );
    let panel_focused = knowledge_border_style_is_focused(app);
    let mut y = area.y;
    let row_h = 4u16;

    // Collected up front (rather than iterating `app.projects` directly) so
    // the graph body can also push into `app.project_click_map` — mirrors
    // `draw_automation_graphs_list`'s `graph_ids_and_meta` pattern, since both
    // borrow `app` mutably for the click map alongside the data being drawn.
    // The index kept is the *project's* index, so a click on a filtered row
    // lands on the project drawn there.
    let cards: Vec<(usize, String, String, String)> = visible
        .iter()
        .skip(scroll.start)
        .take(scroll.max_visible)
        .map(|&idx| {
            let project = &app.projects[idx];
            (
                idx,
                project.name.clone(),
                project_meta_line(app, project),
                last_two_segments(&project.path),
            )
        })
        .collect();

    for (idx, name, meta1, meta2) in &cards {
        if y + 3 > area.y + area.height {
            break;
        }
        draw_project_graph_card(
            frame,
            Rect::new(area.x, y, area.width, 3),
            *idx == app.selected_project,
            ProjectCardText {
                title: name,
                meta1,
                meta2,
            },
            panel_focused,
            theme,
        );
        app.project_click_map.push((*idx, y, y + 3));
        y += row_h;
    }

    draw_scroll_indicators(frame, area, scroll.has_up, scroll.has_down, theme);
}

/// A project row's first meta line: how long ago the project was last active
/// — the recency the list is ordered by, made visible — then its hash.
fn project_meta_line(app: &App, project: &Project) -> String {
    let Some(activity) = app
        .project_last_activity
        .get(&project.hash)
        .and_then(|at| chrono::DateTime::from_timestamp(*at, 0))
    else {
        return project.hash.clone();
    };
    format!(
        "{} · {}",
        crate::tui::app::utils::relative_time(&activity),
        project.hash
    )
}

/// Text rows for [`draw_project_graph_card`]: title plus two meta lines.
/// Bundled so the card renderer stays within the argument-count lint
/// without changing what it draws.
#[derive(Clone, Copy)]
pub(super) struct ProjectCardText<'a> {
    pub(super) title: &'a str,
    pub(super) meta1: &'a str,
    pub(super) meta2: &'a str,
}

pub(super) fn draw_project_graph_card(
    frame: &mut Frame,
    area: Rect,
    selected: bool,
    text: ProjectCardText<'_>,
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
            truncate_str(text.title, area.width as usize),
            title_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(text.meta1, area.width as usize),
            meta_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(text.meta2, area.width as usize),
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
