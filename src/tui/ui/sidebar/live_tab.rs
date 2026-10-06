use super::*;

/// CT14 (C32 follow-on): the guaranteed floor must track the section holding
/// the cursor, derived from where `selected` actually lives — not from the
/// stored `agent_section_focus` alone, which can drift after a data refresh
/// that clamps `selected` without updating focus.
pub(crate) fn live_section_floor_index(
    app: &App,
    interactive_indices: &[usize],
    terminal_indices: &[usize],
) -> Option<usize> {
    let cursor_idx = if interactive_indices.contains(&app.selected) {
        Some(0)
    } else if terminal_indices.contains(&app.selected) {
        Some(1)
    } else if matches!(app.agents.get(app.selected), Some(AgentEntry::Group(_))) {
        Some(2)
    } else {
        None
    };
    cursor_idx.or(match app.agent_section_focus {
        AgentSectionFocus::Interactive => Some(0),
        AgentSectionFocus::Terminal => Some(1),
        AgentSectionFocus::Groups => Some(2),
        AgentSectionFocus::Brain => None,
    })
}

/// CT14: like [`live_section_floor_index`], the Automation floor tracks the
/// entry holding the cursor — a valid graph selection means the Graphs section,
/// a background-agent `selected` means Agents — falling back to the stored
/// `automation_kind` only when neither resolves (both sub-lists empty).
pub(crate) fn automation_section_floor_index(
    app: &App,
    background_indices: &[usize],
) -> Option<usize> {
    let graph_active = app
        .selected_graph_id
        .as_deref()
        .is_some_and(|id| app.sidebar_graphs().iter().any(|lp| lp.id == id));
    let agent_active = background_indices.contains(&app.selected);
    if graph_active {
        Some(1)
    } else if agent_active {
        Some(0)
    } else {
        match app.automation_kind {
            AutomationKind::Agent => Some(0),
            AutomationKind::Graph => Some(1),
        }
    }
}

pub(super) fn draw_live_body(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    interactive_indices: &[usize],
    terminal_indices: &[usize],
    theme: &Theme,
) -> Rect {
    let demands = [
        card_list_demand(interactive_indices.len()),
        card_list_demand(terminal_indices.len()),
        groups_list_demand(app.split_groups.len()),
    ];
    // CT14: the guaranteed floor tracks the section holding the cursor
    // (C32 follow-on) — see `live_section_floor_index`.
    let focused_idx = live_section_floor_index(app, interactive_indices, terminal_indices);
    let alloc = fair_section_heights(&demands, area.height, focused_idx, 6);
    let mut remaining = area;

    if let Some(sub) = take_top(&mut remaining, alloc[0]) {
        let border_style = agent_section_border_style(app, AgentSectionFocus::Interactive, theme);
        render_agent_list_panel(
            frame,
            Some(sub),
            " interactive ",
            interactive_indices,
            app,
            INTERACTIVE_COLOR,
            border_style,
            theme,
        );
    }
    draw_sidebar_gap(frame, &mut remaining, theme);
    if let Some(sub) = take_top(&mut remaining, alloc[1]) {
        let border_style = agent_section_border_style(app, AgentSectionFocus::Terminal, theme);
        render_agent_list_panel(
            frame,
            Some(sub),
            " terminal ",
            terminal_indices,
            app,
            theme.success,
            border_style,
            theme,
        );
    }
    draw_sidebar_gap(frame, &mut remaining, theme);
    if let Some(sub) = take_top(&mut remaining, alloc[2]) {
        render_groups_panel(frame, Some(sub), app, AgentSectionFocus::Groups, theme);
    }

    remaining
}

pub(super) fn draw_automation_body(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    background_indices: &[usize],
    theme: &Theme,
) -> Rect {
    let graph_count = app.sidebar_graphs().len();
    let demands = [
        card_list_demand(background_indices.len()),
        card_list_demand(graph_count),
    ];
    // CT14: like `draw_live_body` above, the floor tracks the entry holding
    // the cursor — see `automation_section_floor_index`.
    // Row counts are recomputed per frame from current lengths (FR5); nothing
    // is cached across frames.
    let focused_idx = automation_section_floor_index(app, background_indices);
    let alloc = fair_section_heights(&demands, area.height, focused_idx, 6);
    let mut remaining = area;

    if let Some(sub) = take_top(&mut remaining, alloc[0]) {
        let border_style = automation_agents_border_style(app, theme);
        render_agent_list_panel(
            frame,
            Some(sub),
            " agents ",
            background_indices,
            app,
            theme.header_color,
            border_style,
            theme,
        );
    }
    draw_sidebar_gap(frame, &mut remaining, theme);
    if let Some(sub) = take_top(&mut remaining, alloc[1]) {
        // The archived count is always shown here — even while browsing the
        // main list — so the archive is never an invisible state; see the
        // F4-archive spec's "always-visible count" requirement.
        let title = if app.graph_view_archived {
            format!(" archived graphs ({}) ", app.archived_graph_count)
        } else if app.archived_graph_count > 0 {
            format!(" graphs · {} archived ", app.archived_graph_count)
        } else {
            " graphs ".to_string()
        };
        render_titled_panel(
            frame,
            sub,
            &title,
            Style::default().fg(theme.dim_text),
            automation_border_style(app, AutomationKind::Graph, theme),
            theme,
            |frame, inner| draw_automation_graphs_list(frame, inner, app, theme),
        );
    }

    remaining
}

// ── Automation layer: graphs sub-list ────────────────────────────────

/// Status icon shown on a graph's card. Every one of the five statuses gets
/// its own icon/color pair so a listed graph is identifiable at a glance
/// without hiding any of them (running/paused/draft/failed/completed are
/// listed side by side now that the sidebar no longer filters by status);
/// `blocked` is a `Paused` sub-state (its latest run recorded a
/// `graph_report_blocker` description) that borrows `Failed`'s color to flag
/// it needs the same attention, distinguished from `Failed` by icon.
pub(super) fn graph_status_icon(
    lp: &Graph,
    meta: &GraphSidebarMeta,
    theme: &Theme,
) -> (&'static str, Color) {
    match lp.status {
        GraphStatus::Running => ("▶", STATUS_RUNNING),
        GraphStatus::Pausing => ("⏸", theme.warning),
        GraphStatus::Paused if meta.blocked => ("⛔", STATUS_FAIL),
        GraphStatus::Paused => ("⏸", theme.warning),
        GraphStatus::Draft => ("○", theme.dim_text),
        GraphStatus::Completed => ("✓", STATUS_OK),
        GraphStatus::Failed => ("✗", STATUS_FAIL),
    }
}

pub(super) fn draw_active_graph_card(
    frame: &mut Frame,
    area: Rect,
    selected: bool,
    lp: &Graph,
    meta: &GraphSidebarMeta,
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
    let (icon, icon_color) = graph_status_icon(lp, meta, theme);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(icon, Style::default().fg(icon_color)),
            Span::raw(" "),
            Span::styled(
                truncate_str(&lp.name, area.width.saturating_sub(2) as usize),
                title_style,
            ),
        ]))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let last_run_style = if lp.status == GraphStatus::Running {
        Style::default().fg(STATUS_RUNNING)
    } else {
        meta_style
    };
    let last_run_text = match &meta.autorun_label {
        Some(autorun) => format!("{} · {autorun}", meta.last_run_label),
        None => meta.last_run_label.clone(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(&last_run_text, area.width as usize),
            last_run_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            truncate_str(&last_two_segments(&lp.workdir), area.width as usize),
            meta_style,
        )))
        .style(Style::default().bg(bg)),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );
}

pub(super) fn draw_automation_graphs_list(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
) {
    let graphs = app.sidebar_graphs();
    if graphs.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No graphs",
                Style::default().fg(theme.muted_text),
            ))),
            area,
        );
        return;
    }

    let selected_index = app
        .selected_graph_id
        .as_deref()
        .and_then(|id| graphs.iter().position(|lp| lp.id == id));
    let scroll = scroll_state(
        graphs.len(),
        selected_index,
        // CT14 (FR5): visible-row count recomputed from the current height
        // every frame — never remembered from when the list was last drawn.
        ((area.height + 1) / 4).max(1) as usize,
    );
    let panel_focused = layer_focused(app, SidebarLayer::Automation)
        && app.automation_kind == AutomationKind::Graph;
    let mut y = area.y;
    let row_h = 4u16;

    let graph_ids_and_meta: Vec<(String, Graph, GraphSidebarMeta)> = graphs
        .iter()
        .copied()
        .skip(scroll.start)
        .take(scroll.max_visible)
        .map(|lp| {
            let meta = app
                .graph_sidebar_meta
                .get(&lp.id)
                .cloned()
                .unwrap_or_default();
            (lp.id.clone(), lp.clone(), meta)
        })
        .collect();

    for (id, lp, meta) in &graph_ids_and_meta {
        if y + 3 > area.y + area.height {
            break;
        }
        let card_area = Rect::new(area.x, y, area.width, 3);
        let selected = app.selected_graph_id.as_deref() == Some(id.as_str());
        draw_active_graph_card(frame, card_area, selected, lp, meta, panel_focused, theme);
        app.automation_graph_click_map.push((id.clone(), y, y + 3));
        y += row_h;
    }

    draw_scroll_indicators(frame, area, scroll.has_up, scroll.has_down, theme);
}

pub(super) fn automation_border_style(app: &App, kind: AutomationKind, theme: &Theme) -> Style {
    let focused = layer_focused(app, SidebarLayer::Automation) && app.automation_kind == kind;
    Style::default().fg(if focused {
        theme.header_color
    } else {
        theme.border_color
    })
}

/// Border style for the Automation layer's `agents` sub-panel — distinct
/// from `agent_section_border_style` (Live's Interactive/Terminal/Groups)
/// since Automation tracks its active sub-list via `automation_kind`.
pub(super) fn automation_agents_border_style(app: &App, theme: &Theme) -> Style {
    automation_border_style(app, AutomationKind::Agent, theme)
}
