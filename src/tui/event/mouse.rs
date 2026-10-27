//! Mouse routing: the prompt-dialog overlay, the tabbed sidebar, the main
//! panel click/scroll regions, PTY forwarding, drag selection and
//! Shift+Click copy.
//! Split out of `event/mod.rs` to keep every production file within
//! the file-size gate.

use anyhow::Result;
use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::prompt_template;
use super::scroll::{
    clamp_sidebar_scroll, focused_terminal_like, handle_scroll, scroll_sidebar,
    with_selected_terminal_like, with_selected_terminal_like_mut, with_terminal_like_agent,
};
use super::{cycle_sidebar_layer_and_normalize_focus, switch_sidebar_tab_and_normalize_focus};
use crate::tui::agent::InteractiveAgent;
use crate::tui::app::types::{AgentEntry, App, Focus, ProjectTab, SidebarLayer, TerminalSelection};

/// A left-click inside the right panel claims its focus (automatic switches
/// are dropped while focused); a left-click anywhere else releases it.
fn handle_panel_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return false;
    }
    let Some(sync_area) = app.last_sync_area else {
        if app.panel_focused {
            app.on_panel_clicked(false);
        }
        return false;
    };
    if rect_contains_point(sync_area, mouse.column, mouse.row) {
        app.on_panel_clicked(true);
        return true;
    }
    if app.panel_focused {
        app.on_panel_clicked(false);
    }
    false
}

pub(super) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> Result<()> {
    // The prompt builder is a modal overlay: it owns the mouse while open.
    // Left-clicks on the tab bar switch the active tab; everything else is
    // swallowed so it doesn't leak to the panel/PTY underneath.
    if app.focus == Focus::PromptTemplateDialog {
        handle_prompt_dialog_mouse(app, &mouse);
        return Ok(());
    }

    if handle_sidebar_mouse(app, &mouse) {
        return Ok(());
    }

    if handle_panel_mouse(app, &mouse) {
        return Ok(());
    }

    if handle_project_panel_mouse(app, &mouse) {
        return Ok(());
    }

    if handle_graph_live_panel_mouse(app, &mouse) {
        return Ok(());
    }

    if handle_preview_focus_click(app, &mouse) {
        return Ok(());
    }

    if try_forward_mouse_to_pty(app, &mouse) {
        // The child program owns the mouse; any pending selection is stale.
        app.terminal_selection = None;
        return Ok(());
    }

    if handle_copy_click(app, &mouse) || handle_selection_mouse(app, &mouse) {
        return Ok(());
    }

    handle_mouse_scroll(app, &mouse);
    Ok(())
}

/// Handle a mouse event while the prompt builder is open. A left-click on one
/// of the tab-bar hit-boxes (positioned via the pure `tab_at` mapping against
/// the origin stored during the last frame) switches the active tab; entering
/// the Raw tab refreshes its composed-prompt preview. ScrollUp/ScrollDown
/// events over the Raw tab's content region scroll the preview or the editable
/// buffer without moving the cursor (view-only). All other mouse events are
/// ignored (and swallowed by the caller).
fn handle_prompt_dialog_mouse(app: &mut App, mouse: &MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some((x, y)) = app.prompt_tab_origin else {
                return;
            };
            let Some(tab) =
                crate::tui::app::dialog::SimplePromptDialog::tab_at(x, y, mouse.column, mouse.row)
            else {
                return;
            };

            let db = app.db.clone();
            let workdir = app.current_workdir();
            if let Some(dialog) = app.simple_prompt_dialog.as_mut() {
                let entering_raw =
                    tab == crate::tui::app::dialog::PromptTab::Raw && dialog.active_tab != tab;
                dialog.set_tab(tab);
                if entering_raw {
                    dialog.refresh_raw_preview(&db, &workdir);
                }
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let Some(dialog) = app.simple_prompt_dialog.as_ref() else {
                return;
            };
            if dialog.active_tab != crate::tui::app::dialog::PromptTab::Raw {
                return;
            }
            let Some(content_rect) = app.prompt_raw_content_rect else {
                return;
            };
            if !rect_contains_point(content_rect, mouse.column, mouse.row) {
                return;
            }
            // Skip if the @-picker overlay is open (it draws over the content area).
            if dialog.at_picker.is_some() {
                return;
            }
            let delta: isize = match mouse.kind {
                MouseEventKind::ScrollUp => -1,
                MouseEventKind::ScrollDown => 1,
                _ => unreachable!(),
            };
            let (is_empty, raw_text) = app
                .simple_prompt_dialog
                .as_ref()
                .map_or((false, String::new()), |d| {
                    (d.raw_is_empty(), d.raw_text().to_string())
                });
            let field_width = prompt_template::prompt_field_width(app);
            let dialog = app.simple_prompt_dialog.as_mut().unwrap();
            if is_empty {
                dialog.scroll_raw_preview(delta);
            } else {
                let total_lines = crate::tui::app::dialog::SimplePromptDialog::visual_line_count(
                    &raw_text,
                    field_width,
                );
                let avail_h = content_rect.height as usize;
                dialog.scroll_raw_edit(delta, total_lines, avail_h);
            }
        }
        _ => {}
    }
}

// ── Mouse: tabbed sidebar (hover, click, scroll, right-click) ───────

/// Handle a mouse event landing on the sidebar: tab-bar clicks (switch the
/// active tab), left-click to select/enter a row in the active tab's body
/// (Live/Automation agent card, Automation graph card, Knowledge project
/// row), scroll to page the active tab's list, and right-click as a
/// shortcut for F2 (cycle tab). Returns `true` if the event was consumed
/// and no further mouse handling should run.
pub(super) fn handle_sidebar_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    let in_sidebar = app.sidebar_visible && mouse.column < sidebar_width(app);

    if !in_sidebar {
        if app.hovered_row.is_some() {
            app.hovered_row = None;
        }
        return false;
    }

    match mouse.kind {
        MouseEventKind::Moved => {
            app.hovered_row = sidebar_agent_at(app, mouse.row);
            true
        }
        MouseEventKind::Down(MouseButton::Left) => {
            handle_sidebar_left_click(app, mouse.row, mouse.column);
            true
        }
        MouseEventKind::Down(MouseButton::Right) => {
            cycle_sidebar_layer_and_normalize_focus(app);
            true
        }
        MouseEventKind::ScrollUp => {
            scroll_sidebar(app, 1);
            true
        }
        MouseEventKind::ScrollDown => {
            scroll_sidebar(app, -1);
            true
        }
        _ => false,
    }
}

fn handle_sidebar_left_click(app: &mut App, row: u16, col: u16) {
    if let Some(layer) = sidebar_tab_at(app, row, col) {
        switch_sidebar_tab_and_normalize_focus(app, layer);
        return;
    }
    if let Some(idx) = sidebar_agent_at(app, row) {
        let reenter = app.selected == idx && !app.agents_rag_focused;
        app.select_agent_at(idx);
        app.focus = if reenter {
            Focus::Agent
        } else {
            Focus::Preview
        };
        return;
    }
    if let Some(graph_id) = automation_graph_at(app, row) {
        let reselect = app.sidebar_layer == SidebarLayer::Automation
            && app.selected_graph_id.as_deref() == Some(graph_id.as_str());
        app.sidebar_layer = SidebarLayer::Automation;
        app.automation_kind = crate::tui::app::AutomationKind::Graph;
        app.selected_graph_id = Some(graph_id);
        app.refresh_graphs_selection();
        app.focus = Focus::Preview;
        let _ = reselect;
        return;
    }
    if let Some(idx) = project_row_at(app, row) {
        let reenter = app.sidebar_layer == SidebarLayer::Knowledge && app.selected_project == idx;
        app.sidebar_layer = SidebarLayer::Knowledge;
        app.selected_project = idx;
        app.refresh_graphs_selection();
        if reenter {
            app.enter_project_focus(ProjectTab::Overview);
            app.focus = Focus::Agent;
        } else {
            app.exit_project_focus();
            app.focus = Focus::Preview;
        }
    }
}

/// Map a sidebar (row, col) to the tab cell rendered there, via the click
/// map populated during draw.
pub(super) fn sidebar_tab_at(app: &App, row: u16, col: u16) -> Option<SidebarLayer> {
    app.sidebar_tab_click_map
        .iter()
        .find(|&&(_, tab_row, start, end)| row == tab_row && col >= start && col < end)
        .map(|&(layer, _, _, _)| layer)
}

/// Map a sidebar row (terminal row coordinate) to the agent index rendered
/// there on the last frame, via the click map populated during draw.
pub(super) fn sidebar_agent_at(app: &App, row: u16) -> Option<usize> {
    app.sidebar_click_map
        .iter()
        .find(|&&(_, start, end)| row >= start && row < end)
        .map(|&(idx, _, _)| idx)
}

/// Map a sidebar row to the Automation-layer graph id rendered there.
fn automation_graph_at(app: &App, row: u16) -> Option<String> {
    app.automation_graph_click_map
        .iter()
        .find(|&&(_, start, end)| row >= start && row < end)
        .map(|(id, _, _)| id.clone())
}

/// Map a sidebar row to the Knowledge-layer project index rendered there.
fn project_row_at(app: &App, row: u16) -> Option<usize> {
    app.project_click_map
        .iter()
        .find(|&&(_, start, end)| row >= start && row < end)
        .map(|&(idx, _, _)| idx)
}

// ── Mouse: project Focus tab bar + tab content (main panel) ─────────

/// Handle a mouse event landing on a project's Focus tab bar/content in the
/// main panel — reuses the same click-map-populated-during-draw pattern as
/// the sidebar (functional requirement 4: no second mouse path). Returns
/// `true` if consumed.
pub(super) fn handle_project_panel_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if app.sidebar_layer != SidebarLayer::Knowledge || app.project_focus.is_none() {
        return false;
    }
    let (panel_w, panel_h) = app.last_panel_inner;
    let panel_rect =
        ratatui::layout::Rect::new(app.last_panel_x, app.last_panel_y, panel_w, panel_h);
    if !rect_contains_point(panel_rect, mouse.column, mouse.row) {
        return false;
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if mouse.row == app.last_panel_y {
                if let Some(tab) = project_tab_at(app, mouse.column) {
                    app.enter_project_focus(tab);
                    return true;
                }
            }
            if let Some(idx) = project_tab_row_at(app, mouse.row) {
                app.set_project_tab_row(idx);
            }
            true
        }
        MouseEventKind::ScrollUp => {
            app.select_prev();
            true
        }
        MouseEventKind::ScrollDown => {
            app.select_next();
            true
        }
        _ => false,
    }
}

/// Handle a mouse event landing on the live graph view's spec marker strip —
/// reuses the click-map-populated-during-draw pattern from the sidebar and
/// project panel rather than a second hit-testing mechanism. A left-click on
/// a chip selects that spec (entering manual selection, same as arrow-key
/// navigation); the scroll wheel over the chip row pages a truncated strip.
/// Returns `true` if consumed.
pub(super) fn handle_graph_live_panel_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    if app.focus != Focus::Preview
        || app.sidebar_layer != SidebarLayer::Automation
        || app.automation_kind != crate::tui::app::AutomationKind::Graph
    {
        return false;
    }
    let (panel_w, panel_h) = app.last_panel_inner;
    let panel_rect =
        ratatui::layout::Rect::new(app.last_panel_x, app.last_panel_y, panel_w, panel_h);
    if !rect_contains_point(panel_rect, mouse.column, mouse.row) {
        return false;
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            match graph_spec_strip_at(app, mouse.row, mouse.column) {
                Some(spec_id) => {
                    app.graph_spec_strip_select(spec_id);
                    true
                }
                None => false,
            }
        }
        MouseEventKind::ScrollUp
            if graph_spec_strip_row_range(app)
                .is_some_and(|(lo, hi)| mouse.row >= lo && mouse.row <= hi) =>
        {
            scroll_graph_spec_strip(app, 1);
            true
        }
        MouseEventKind::ScrollDown
            if graph_spec_strip_row_range(app)
                .is_some_and(|(lo, hi)| mouse.row >= lo && mouse.row <= hi) =>
        {
            scroll_graph_spec_strip(app, -1);
            true
        }
        MouseEventKind::ScrollUp => {
            app.graph_live_view_scroll_step(-3);
            true
        }
        MouseEventKind::ScrollDown => {
            app.graph_live_view_scroll_step(3);
            true
        }
        _ => false,
    }
}

/// A left-click on the central preview area enters focus on the previewed
/// session (CT6) — the mouse equivalent of pressing Enter in Preview.
/// Consumes the click so it never reaches PTY forwarding or
/// copy/selection below; once focused, later clicks flow to the child as
/// before. Only terminal-like sessions qualify: project cards, the graph
/// live view, playground, RAG overview, and background/session-less
/// previews keep their current mouse behavior. Placed after the panel
/// handlers above and immediately before PTY forwarding so it shadows
/// neither.
pub(super) fn handle_preview_focus_click(app: &mut App, mouse: &MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
        return false;
    }
    if app.focus != Focus::Preview {
        return false;
    }
    if app.playground_active || app.agents_rag_focused || app.project_focus.is_some() {
        return false;
    }
    if app.sidebar_layer == SidebarLayer::Knowledge {
        return false;
    }
    if app.sidebar_layer == SidebarLayer::Automation
        && app.automation_kind == crate::tui::app::AutomationKind::Graph
    {
        return false;
    }
    if !matches!(
        app.selected_agent(),
        Some(AgentEntry::Interactive(_)) | Some(AgentEntry::Terminal(_))
    ) {
        return false;
    }
    let (panel_w, panel_h) = app.last_panel_inner;
    let panel_rect =
        ratatui::layout::Rect::new(app.last_panel_x, app.last_panel_y, panel_w, panel_h);
    if !rect_contains_point(panel_rect, mouse.column, mouse.row) {
        return false;
    }
    // Same state transition as keyboard focus entry in `handle_preview_key`:
    // reset the log scroll, then enter Agent focus.
    app.log_scroll = 0;
    app.focus = Focus::Agent;
    true
}

/// Map a graph-live-panel (row, col) to the spec id whose marker chip is
/// rendered there, via the click map populated during draw.
fn graph_spec_strip_at(app: &App, row: u16, col: u16) -> Option<String> {
    app.graph_spec_strip_click_map
        .iter()
        .find(|&&(_, chip_row, start, end)| row == chip_row && col >= start && col < end)
        .map(|(id, _, _, _)| id.clone())
}

/// The screen row range the marker strip's chips are rendered on, if any are
/// currently visible (empty when there's no spec queue to show).
fn graph_spec_strip_row_range(app: &App) -> Option<(u16, u16)> {
    if app.graph_spec_strip_click_map.is_empty() {
        return None;
    }
    let min_row = app
        .graph_spec_strip_click_map
        .iter()
        .map(|&(_, r, _, _)| r)
        .min()?;
    let max_row = app
        .graph_spec_strip_click_map
        .iter()
        .map(|&(_, r, _, _)| r)
        .max()?;
    Some((min_row, max_row))
}

fn scroll_graph_spec_strip(app: &mut App, dir: i32) {
    let total = app
        .graph_live_state
        .as_ref()
        .map_or(0, |state| state.spec_queue.len());
    let capacity = app.graph_spec_strip_capacity.max(1);
    app.graph_spec_strip_scroll =
        clamp_sidebar_scroll(app.graph_spec_strip_scroll, total, capacity, dir);
}

/// Map a main-panel column (on the tab-bar row) to the `ProjectTab` rendered
/// there on the last frame.
fn project_tab_at(app: &App, col: u16) -> Option<ProjectTab> {
    app.project_tab_click_map
        .iter()
        .find(|&&(_, start, end)| col >= start && col < end)
        .map(|&(tab, _, _)| tab)
}

/// Map a main-panel row to the active tab's list row index rendered there.
fn project_tab_row_at(app: &App, row: u16) -> Option<usize> {
    app.project_tab_row_click_map
        .iter()
        .find(|&&(_, start, end)| row >= start && row < end)
        .map(|&(idx, _, _)| idx)
}

pub(super) fn handle_copy_click(app: &mut App, mouse: &MouseEvent) -> bool {
    if !matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
        return false;
    }

    if mouse.modifiers.contains(KeyModifiers::SHIFT) {
        app.terminal_selection = None;
        handle_shift_click_copy(app);
        return true;
    }

    false
}

// ── Mouse drag selection over the focused PTY pane ───────────────────
//
// Click+drag selects text cells linearly (like a terminal); on release the
// selection is copied to the system clipboard without any TUI decoration.
pub(super) fn handle_selection_mouse(app: &mut App, mouse: &MouseEvent) -> bool {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.terminal_selection = None;
            let Some(agent) = focused_terminal_like(app) else {
                return false;
            };
            let Some((col, row)) = mouse_pty_position(app, mouse) else {
                return false;
            };
            app.terminal_selection = Some(TerminalSelection {
                agent,
                start: (row, col),
                end: (row, col),
                dragging: true,
            });
            true
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            let (col, row) = clamped_pty_position(app, mouse);
            let Some(sel) = app.terminal_selection.as_mut() else {
                return false;
            };
            if !sel.dragging {
                return false;
            }
            sel.end = (row, col);
            true
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(sel) = app.terminal_selection.take() else {
                return false;
            };
            if !sel.dragging || sel.start == sel.end {
                return false;
            }
            let (start, end) = sel.normalized();
            let text = with_terminal_like_agent(app, sel.agent.0, sel.agent.1, |agent| {
                agent
                    .screen_snapshot()
                    .map(|snap| snap.selection_text(start, end))
            })
            .flatten()
            .unwrap_or_default();
            if text.trim().is_empty() {
                return true;
            }
            crate::tui::clipboard::set_text(&text);
            mark_copied(app);
            true
        }
        _ => false,
    }
}

/// Pane-relative (col, row) for drag events, clamped to the panel bounds so
/// dragging past an edge extends the selection to that edge. Uses the
/// focused panel's last-rendered geometry, which tracks whichever half of a
/// split (if any) is currently focused.
pub(super) fn clamped_pty_position(app: &App, mouse: &MouseEvent) -> (u16, u16) {
    let (panel_w, panel_h) = app.last_panel_inner;
    let col = mouse
        .column
        .saturating_sub(app.last_panel_x)
        .min(panel_w.saturating_sub(1));
    let row = mouse
        .row
        .saturating_sub(app.last_panel_y)
        .min(panel_h.saturating_sub(1));
    (col, row)
}

fn handle_mouse_scroll(app: &mut App, mouse: &MouseEvent) {
    let Some(dir) = scroll_direction(mouse.kind) else {
        return;
    };

    // Scrolling shifts the pane content under a selection's coordinates.
    app.terminal_selection = None;

    if app.hub_open {
        match app.hub.tab {
            crate::tui::app::hub::HubTab::Missions => {
                let mission_count = crate::domain::gamification::MISSIONS.len();
                if mission_count != 0 {
                    let forward = dir < 0;
                    app.legend_selected = crate::tui::selection::move_index(
                        app.legend_selected,
                        mission_count,
                        forward,
                    );
                }
            }
            crate::tui::app::hub::HubTab::Shelf => app.move_shelf_selection(dir < 0),
            _ => {}
        }
        return;
    }

    if handle_sync_panel_scroll(app, mouse, dir) {
        return;
    }

    handle_scroll(app, dir);
}

pub(super) fn scroll_direction(kind: MouseEventKind) -> Option<i32> {
    match kind {
        MouseEventKind::ScrollUp => Some(1),
        MouseEventKind::ScrollDown => Some(-1),
        _ => None,
    }
}

pub(super) fn handle_sync_panel_scroll(app: &mut App, mouse: &MouseEvent, dir: i32) -> bool {
    let Some(sync_area) = app.last_sync_area else {
        return false;
    };
    if !rect_contains_point(sync_area, mouse.column, mouse.row) {
        return false;
    }

    // Scrolling anywhere in the panel counts as interacting with it: the
    // pending automatic face switch (if any) is dropped, not deferred —
    // covers every branch below, including the no-op fallback.
    app.on_panel_scrolled();

    if let Some(rect) = app.last_activity_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            if dir > 0 {
                app.sync_scroll_offset = app.sync_scroll_offset.saturating_sub(3);
            } else {
                app.sync_scroll_offset = app.sync_scroll_offset.saturating_add(3);
            }
            return true;
        }
    }

    if let Some(rect) = app.last_knowledge_graph_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            let total = app.project_graph_edges.len();
            let visible = rect.height as usize;
            app.knowledge_graph_scroll = clamp_sidebar_scroll(
                app.knowledge_graph_scroll as usize,
                total,
                visible.max(1),
                dir,
            ) as u16;
            return true;
        }
    }

    if let Some(rect) = app.last_knowledge_list_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            let total = app.project_knowledge.len();
            let visible = (rect.height as usize).saturating_sub(1);
            app.knowledge_list_scroll = clamp_sidebar_scroll(
                app.knowledge_list_scroll as usize,
                total,
                visible.max(1),
                dir,
            ) as u16;
            return true;
        }
    }

    if let Some(rect) = app.last_graph_face_rect {
        if rect_contains_point(rect, mouse.column, mouse.row) {
            // `graph_face_scroll_step` mirrors `graph_live_view_scroll_step`'s
            // convention (positive = forward into content), the opposite of
            // this function's `dir` (ScrollDown = -1 must move forward, to
            // match `sync_scroll_offset` and `clamp_sidebar_scroll` above) —
            // negate to translate between the two.
            app.graph_face_scroll_step(-dir * 3);
            return true;
        }
    }

    // Inside the panel but outside every scrollable region: consumed, no-op
    // (functional requirement 3).
    true
}

pub(super) fn rect_contains_point(rect: ratatui::layout::Rect, column: u16, row: u16) -> bool {
    column >= rect.x
        && column < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

/// Try to forward the mouse event to the focused PTY agent.
/// Returns `true` if the event was consumed.
fn try_forward_mouse_to_pty(app: &mut App, mouse: &MouseEvent) -> bool {
    let Some((pty_col, pty_row)) = mouse_pty_position(app, mouse) else {
        return false;
    };

    with_selected_terminal_like_mut(app, |agent| {
        agent
            .forward_mouse(mouse.kind, MouseButton::Left, pty_col, pty_row)
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

pub(super) fn mouse_pty_position(app: &App, mouse: &MouseEvent) -> Option<(u16, u16)> {
    let panel_x = app.last_panel_x;
    let panel_y = app.last_panel_y;
    let panel_width = app.last_panel_inner.0;
    let panel_height = app.last_panel_inner.1;

    if mouse.column < panel_x
        || mouse.row < panel_y
        || mouse.column >= panel_x.saturating_add(panel_width)
        || mouse.row >= panel_y.saturating_add(panel_height)
    {
        return None;
    }

    Some((
        mouse.column.saturating_sub(panel_x),
        mouse.row.saturating_sub(panel_y),
    ))
}

fn sidebar_width(app: &App) -> u16 {
    if app.sidebar_visible {
        crate::tui::ui::SIDEBAR_WIDTH
    } else {
        0
    }
}

fn handle_shift_click_copy(app: &mut App) {
    mark_copied(app);

    let Some(text) =
        with_selected_terminal_like(app, InteractiveAgent::get_plain_text_from_screen).flatten()
    else {
        return;
    };

    crate::tui::clipboard::set_text(&text);
}

fn mark_copied(app: &mut App) {
    app.show_copied = true;
    app.copied_at = std::time::Instant::now();
}
