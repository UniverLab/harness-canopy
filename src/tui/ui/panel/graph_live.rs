//! The live graph view: a read-only render of [`GraphLiveState`] for the
//! main panel — header, spec queue, current-spec graph (auto-following the
//! engine's current node, or a manually-highlighted one), and a detail
//! footer. Pure over the snapshot; the only I/O is the `App` glue in
//! [`draw_graph_live_view`], which assembles plain values before handing off
//! to [`render_graph_live_view`].

use chrono::{DateTime, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::super::theme::Theme;
use crate::tui::app::graph_live_state::{GraphLiveState, NodeRunInfo};
use crate::tui::app::types::App;

mod lines;
mod render;

use render::render_graph_live_view;

#[cfg(test)]
use crate::domain::graphs::{GraphEdgeCondition, GraphRunStatus};
#[cfg(test)]
use crate::tui::app::graph_live_state::{EnsembleLiveInfo, SpecQueueEntry};
#[cfg(test)]
use lines::{ensemble_box_lines, graph_lines, node_box_lines};
#[cfg(test)]
use ratatui::style::Color;
#[cfg(test)]
use render::{spec_strip_layout, SPEC_CHIP_WIDTH};
#[cfg(test)]
use std::collections::HashMap;

pub(crate) fn draw_graph_live_view(frame: &mut Frame, area: Rect, app: &mut App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        app.graph_spec_strip_click_map.clear();
        return;
    }
    let Some(state) = app.graph_live_state.as_ref() else {
        app.graph_spec_strip_click_map.clear();
        frame.render_widget(
            Paragraph::new("No graph selected").style(Style::default().fg(theme.dim_text)),
            area,
        );
        return;
    };

    let blocked = app
        .graph_sidebar_meta
        .get(&state.graph_id)
        .is_some_and(|meta| meta.blocked);
    let highlighted = app.graph_live_highlighted_node_id().map(str::to_string);
    let node_info = app.graph_live_highlighted_node_run_info();
    let selected_spec_id = app.graph_spec_strip_selected.clone();
    let spec_scroll = app.graph_spec_strip_scroll;

    // CT28: split the area when the tail pane is open — graph on the left
    // (≥140 cols) or top (<140), pane taking the rest.
    let (graph_area, pane_area) = if app.node_tail_dialog_active() {
        tail_pane_split(area)
    } else {
        (area, Rect::default())
    };

    let result = render_graph_live_view(
        frame,
        graph_area,
        &LiveViewContext {
            state,
            follow: app.graph_live_follow,
            follow_anchor: app.graph_live_follow_anchor.as_deref(),
            highlighted_node_id: highlighted.as_deref(),
            node_info: &node_info,
            blocked,
            now: Utc::now(),
            theme,
            time_format: &app.tui_settings.time_format,
            selected_spec_id: selected_spec_id.as_deref(),
            spec_scroll,
            scroll: app.graph_live_view_scroll,
        },
    );

    app.graph_spec_strip_click_map = result.click_map;
    app.graph_spec_strip_capacity = result.capacity;
    app.graph_live_view_total_lines = result.total_lines;
    app.graph_live_view_scroll = result.clamped_scroll;
    app.graph_live_follow_anchor = result.follow_anchor;

    // CT28: live-tail pane renders beside the graph. Read-only viewer.
    if let Some(dialog) = app.node_tail_dialog.as_ref() {
        super::super::dialogs::draw_node_tail_pane(frame, pane_area, dialog, theme, Utc::now());
    }
    app.node_tail_pane_body_height = pane_area.height.saturating_sub(2) as usize;
}

/// CT28: split `area` into (graph, pane). Width ≥ 140 → horizontal (50/50);
/// otherwise vertical (graph 40%, ≥ 8 rows, pane below).
fn tail_pane_split(area: Rect) -> (Rect, Rect) {
    if area.width >= 140 {
        let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(area);
        (cols[0], cols[1])
    } else {
        let graph_h = ((area.height * 40) / 100).max(8).min(area.height);
        let rows = Layout::vertical([Constraint::Length(graph_h), Constraint::Min(0)]).split(area);
        (rows[0], rows[1])
    }
}

/// Everything the pure renderer needs, gathered by [`draw_graph_live_view`]
/// so the render itself stays a plain function of already-computed values
/// (no `Database`/`App` access), which is what makes it testable without a
/// full `App`.
struct LiveViewContext<'a> {
    state: &'a GraphLiveState,
    follow: bool,
    /// CT8: the node id the graph last re-centred on while auto-following
    /// (mirrors `App::graph_live_follow_anchor`). Compared against
    /// `highlighted_node_id` — which equals the engine's current node id
    /// whenever `follow` is true — to decide whether this frame is a real
    /// node transition that should re-centre the scroll.
    follow_anchor: Option<&'a str>,
    highlighted_node_id: Option<&'a str>,
    node_info: &'a NodeRunInfo,
    blocked: bool,
    now: DateTime<Utc>,
    theme: &'a Theme,
    /// CM52: chrono format for the time-only labels (`tui.time_format`).
    time_format: &'a str,
    /// Spec id manually selected in the marker strip, if any — independent
    /// of `highlighted_node_id`/`follow`, which are the *graph's* own
    /// follow/manual state.
    selected_spec_id: Option<&'a str>,
    /// First visible index into `state.spec_queue` for the marker strip.
    spec_scroll: usize,
    scroll: u16,
}

/// What [`render_graph_live_view`] hands back to its `App`-owning caller:
/// where the marker strip's chips actually landed on screen, for mouse
/// hit-testing next frame (mirrors `sidebar_tab_click_map`'s shape).
struct LiveViewRenderResult {
    click_map: Vec<(String, u16, u16, u16)>,
    capacity: usize,
    total_lines: u16,
    clamped_scroll: u16,
    /// CT8: the anchor to persist for next frame. In auto-follow this is the
    /// current node id (adopted whether or not we re-centred this frame); in
    /// manual it is carried through unchanged.
    follow_anchor: Option<String>,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tail_pane_split_tests;
