use super::*;

#[derive(Clone, Copy)]
pub(super) struct SidebarContentAreas {
    pub(super) content: Rect,
    pub(super) dashboard: Option<Rect>,
}

pub(super) fn dashboard_height(app: &App, theme: &Theme) -> u16 {
    // Ask the dashboard how many rows it will actually draw (cpu/mem/load are
    // always present; gpu/pwr/swap only when their data is). Reserving a fixed
    // slot for optional rows — as an earlier version did for the now
    // battery-only `pwr:` row — left a blank line when the row was absent.
    let content_lines = crate::tui::ui::system_dashboard::dashboard_content_line_count(
        &app.system_info,
        app.temperature_unit,
        theme,
    ) as u16;
    content_lines + 2
}

pub(super) fn split_sidebar_content(area: Rect, app: &App, theme: &Theme) -> SidebarContentAreas {
    let height = dashboard_height(app, theme);
    let dashboard = (area.height >= height).then_some(Rect::new(
        area.x,
        area.y + area.height - height,
        area.width,
        height,
    ));
    let content = dashboard.map_or(area, |dashboard| {
        Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(dashboard.height),
        )
    });
    SidebarContentAreas { content, dashboard }
}

pub(super) fn split_top_panel(
    content: Rect,
    enabled: bool,
    top_height: u16,
) -> (Option<Rect>, Rect) {
    if !enabled {
        return (None, content);
    }

    let [top, bottom] =
        Layout::vertical([Constraint::Length(top_height), Constraint::Min(0)]).areas(content);
    (Some(top), bottom)
}

pub(super) fn section_block<'a>(
    title: &'a str,
    title_style: Style,
    border_style: Style,
    theme: &Theme,
) -> Block<'a> {
    Block::default()
        .title_bottom(
            Line::from(Span::styled(title, title_style))
                .alignment(ratatui::layout::Alignment::Right),
        )
        .borders(borders_for(theme))
        .border_style(border_style)
}

pub(super) fn render_titled_panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    title_style: Style,
    border_style: Style,
    theme: &Theme,
    render_inner: impl FnOnce(&mut Frame, Rect),
) {
    let block = section_block(title, title_style, border_style, theme);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_inner(frame, inner);
}

pub(super) fn take_top(area: &mut Rect, height: u16) -> Option<Rect> {
    if area.height == 0 || height == 0 {
        return None;
    }

    let [top, rest] = Layout::vertical([
        Constraint::Length(height.min(area.height)),
        Constraint::Min(0),
    ])
    .areas(*area);
    *area = rest;
    Some(top)
}

#[derive(Clone, Copy)]
pub(super) struct ScrollState {
    pub(super) start: usize,
    pub(super) max_visible: usize,
    pub(super) has_up: bool,
    pub(super) has_down: bool,
}

pub(super) fn scroll_state(
    total_items: usize,
    selected: Option<usize>,
    max_visible: usize,
) -> ScrollState {
    scroll_state_with_offset(total_items, selected, max_visible, 0)
}

/// Like [`scroll_state`], but shifts the auto-follow-selection start further
/// down by `manual_offset` rows (mouse-wheel scrolling), clamped so the list
/// never scrolls past its last page.
pub(super) fn scroll_state_with_offset(
    total_items: usize,
    selected: Option<usize>,
    max_visible: usize,
    manual_offset: usize,
) -> ScrollState {
    let auto_start = selected.map_or(0, |sel| {
        crate::tui::selection::clamp_scroll(sel, 0, total_items, max_visible)
    });
    let max_start = total_items.saturating_sub(max_visible);
    let start = (auto_start + manual_offset).min(max_start);

    ScrollState {
        start,
        max_visible,
        has_up: start > 0,
        has_down: total_items.saturating_sub(start) > max_visible,
    }
}

pub(super) fn render_brain_if_visible(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if area.height < 3 || area.width < 6 {
        app.sidebar_brain_rect = None;
        return;
    }
    app.sidebar_brain_rect = Some(area);
    if app.sidebar_brain.is_none() {
        return;
    }
    if let Some(brain) = app.sidebar_brain.as_ref() {
        crate::tui::ui::panel::draw_brians_brain(frame, area, brain, theme.sidebar_bg);
    }
}

pub(super) fn render_dashboard_if_present(
    frame: &mut Frame,
    area: Option<Rect>,
    app: &App,
    theme: &Theme,
) {
    let Some(area) = area else {
        return;
    };
    crate::tui::ui::system_dashboard::render_system_dashboard(
        frame,
        area,
        &app.system_info,
        app.temperature_unit,
        theme,
    );
}

/// The project graph's minimum *outer* height — what the split hands out,
/// not what `draw_project_graph` gets to draw into. `render_titled_panel`
/// strips the border before calling in, so with `Borders::ALL` this leaves
/// an inner budget of 2 edge lines; with a borderless theme, all 4.
/// `draw_project_graph` must budget the inner height it receives directly
/// rather than subtracting border rows a second time.
pub(super) const GRAPH_MIN_HEIGHT: u16 = 4;

/// How the leftover space below the three layers is carved up between the
/// project relation graph and Brian's Brain. When the graph has nothing to
/// show, the brain takes the whole area, as before. When it does, the graph
/// gets its minimum first (it carries information; the brain is atmosphere),
/// then the brain takes whatever remains, if that remainder still meets its
/// own minimum — otherwise the graph draws alone rather than splitting into
/// two broken panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BrainOrGraphLayout {
    Neither,
    GraphOnly(Rect),
    BrainOnly(Rect),
    Both { graph: Rect, brain: Rect },
}

pub(super) fn split_brain_or_graph(area: Rect, graph_has_content: bool) -> BrainOrGraphLayout {
    if area.height == 0 {
        return BrainOrGraphLayout::Neither;
    }

    if !graph_has_content {
        return if area.height >= 3 && area.width >= 6 {
            BrainOrGraphLayout::BrainOnly(area)
        } else {
            BrainOrGraphLayout::Neither
        };
    }

    if area.height < GRAPH_MIN_HEIGHT {
        return BrainOrGraphLayout::Neither;
    }

    let brain_fits = area.width >= 6 && area.height >= GRAPH_MIN_HEIGHT + 3;
    if !brain_fits {
        return BrainOrGraphLayout::GraphOnly(area);
    }

    let graph = Rect::new(area.x, area.y, area.width, GRAPH_MIN_HEIGHT);
    let brain = Rect::new(
        area.x,
        area.y + GRAPH_MIN_HEIGHT,
        area.width,
        area.height - GRAPH_MIN_HEIGHT,
    );
    BrainOrGraphLayout::Both { graph, brain }
}

pub(super) fn render_graph_panel(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    render_titled_panel(
        frame,
        area,
        " project graph ",
        Style::default().fg(theme.dim_text),
        Style::default().fg(theme.dim_text),
        theme,
        |frame, inner| draw_project_graph(frame, inner, app, theme, 0),
    );
}

/// Whether the shared strip should reserve rows for the project graph
/// instead of handing them all to Brian's Brain: only on the Knowledge tab
/// (C26 decision 1 — projects aren't the subject on Live/Automation), and
/// only when there's an edge to draw (C26 decision 2 — a workspace of
/// unrelated projects still fills `project_graph_trees` with one singleton
/// per project, which is not "content"). CT1 moves the graph into the
/// right panel's Knowledge face, so while that face is showing the sidebar
/// stops rendering it there — one home for the graph, never two.
#[cfg(test)]
pub(super) fn graph_has_content(edge_count: usize, is_knowledge_tab: bool) -> bool {
    is_knowledge_tab && edge_count > 0
}

pub(super) fn render_brain_or_graph(
    frame: &mut Frame,
    area: Rect,
    app: &mut App,
    theme: &Theme,
    _is_knowledge_tab: bool,
) {
    // CT1: the project-relations graph belongs exclusively to the right
    // panel's Knowledge face. Keep this sidebar region for Brian's Brain.
    match split_brain_or_graph(area, false) {
        BrainOrGraphLayout::Neither => {}
        BrainOrGraphLayout::GraphOnly(graph_area) => {
            render_graph_panel(frame, graph_area, app, theme);
        }
        BrainOrGraphLayout::BrainOnly(brain_area) => {
            render_brain_if_visible(frame, brain_area, app, theme);
        }
        BrainOrGraphLayout::Both { graph, brain } => {
            render_graph_panel(frame, graph, app, theme);
            render_brain_if_visible(frame, brain, app, theme);
        }
    }
}

pub(super) fn draw_sidebar_gap(frame: &mut Frame, remaining: &mut Rect, theme: &Theme) {
    if remaining.height > 0 {
        if let Some(gap) = take_top(remaining, 1) {
            frame.render_widget(
                Paragraph::new("").style(Style::default().bg(theme.sidebar_bg)),
                gap,
            );
        }
    }
}

pub(crate) fn draw_scroll_indicators(
    frame: &mut Frame,
    area: Rect,
    has_up: bool,
    has_down: bool,
    theme: &Theme,
) {
    if has_up {
        frame.render_widget(
            Paragraph::new("▲").style(Style::default().fg(theme.dim_text)),
            Rect::new(area.x + area.width.saturating_sub(2), area.y, 1, 1),
        );
    }
    if has_down {
        frame.render_widget(
            Paragraph::new("▼").style(Style::default().fg(theme.dim_text)),
            Rect::new(
                area.x + area.width.saturating_sub(2),
                (area.y + area.height).saturating_sub(1),
                1,
                1,
            ),
        );
    }
}
