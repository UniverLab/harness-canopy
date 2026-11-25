//! The pure renderer behind `draw_graph_live_view`: header, spec marker
//! strip, graph body, and detail footer. Split out of `graph_live.rs` to
//! keep every production file within the file-size gate.

use chrono::{DateTime, Local, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::diagram::diagram_lines;
use super::lines::graph_lines;
pub(super) use super::spec_strip::spec_strip_layout;
#[cfg(test)]
pub(super) use super::spec_strip::SPEC_CHIP_WIDTH;
use super::{LiveViewContext, LiveViewRenderResult};
use crate::db::mutex_store::MutexWaitDetail;
use crate::domain::graphs::{GraphRunStatus, GraphStatus};
use crate::tui::app::graph_live_state::{pausing_label, GraphLiveState, NodeRunInfo};
use crate::tui::ui::panel::compact_cwd;
use crate::tui::ui::sidebar::draw_scroll_indicators;
use crate::tui::ui::theme::Theme;

pub(super) fn render_graph_live_view(
    frame: &mut Frame,
    area: Rect,
    ctx: &LiveViewContext,
) -> LiveViewRenderResult {
    let state = ctx.state;
    let mut top_lines = header_lines(state, ctx.blocked, ctx.theme, ctx.time_format);
    top_lines.push(Line::from(""));

    let chip_row = area.y + top_lines.len() as u16;
    let strip = spec_strip_layout(
        state,
        ctx.selected_spec_id,
        ctx.spec_scroll,
        area.width,
        area.height,
        ctx.theme,
    );
    top_lines.extend(strip.lines);

    let top_para = Paragraph::new(top_lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(ctx.theme.info_bg));
    let top_h = (top_para.line_count(area.width) as u16).min(area.height);
    let body_h = area.height.saturating_sub(top_h);
    let top_area = Rect::new(area.x, area.y, area.width, top_h);
    let body_area = Rect::new(area.x, area.y + top_h, area.width, body_h);

    let mut body_lines = vec![Line::from(Span::styled(
        "Graph",
        Style::default().fg(ctx.theme.dim_text),
    ))];
    let graph_start_line = body_lines.len() as u16;
    let (graph_offset, graph_span) =
        push_graph_lines(&mut body_lines, state, ctx, area.width).unzip();
    let graph_offset = graph_offset.flatten();
    let graph_span = graph_span.flatten();
    body_lines.push(Line::from(""));
    body_lines.extend(footer_lines(
        state,
        ctx.highlighted_node_id,
        ctx.node_info,
        ctx.follow,
        ctx.now,
        ctx.theme,
    ));

    let body_line_count = body_lines.len() as u16;
    let max_scroll = body_line_count.saturating_sub(body_h);
    let clamped_from_input = ctx.scroll.min(max_scroll);

    let (scroll, next_follow_anchor) = live_view_scroll(
        ctx,
        graph_offset,
        graph_start_line,
        clamped_from_input,
        body_h,
        max_scroll,
        graph_span,
    );
    let clamped_scroll = scroll.min(max_scroll);

    if top_h > 0 {
        frame.render_widget(top_para, top_area);
    }
    if body_h > 0 {
        frame.render_widget(
            Paragraph::new(body_lines)
                .wrap(Wrap { trim: false })
                .scroll((clamped_scroll, 0))
                .style(Style::default().bg(ctx.theme.info_bg)),
            body_area,
        );
    }

    let has_up = clamped_scroll > 0;
    let has_down = body_line_count > body_h && clamped_scroll < max_scroll;
    if body_h > 0 && (has_up || has_down) {
        draw_scroll_indicators(frame, body_area, has_up, has_down, ctx.theme);
    }

    LiveViewRenderResult {
        click_map: strip
            .click_map
            .into_iter()
            .map(|(spec_id, row_offset, col_start, col_end)| {
                (
                    spec_id,
                    chip_row + row_offset,
                    area.x + col_start,
                    area.x + col_end,
                )
            })
            .collect(),
        capacity: strip.capacity,
        total_lines: top_h + body_line_count,
        clamped_scroll,
        follow_anchor: next_follow_anchor,
    }
}

/// Append the graph body — either the `no nodes yet` note or the node boxes.
fn push_graph_lines(
    lines: &mut Vec<Line<'static>>,
    state: &GraphLiveState,
    ctx: &LiveViewContext,
    area_width: u16,
) -> Option<(Option<u16>, Option<u16>)> {
    if state.effective_nodes.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no nodes yet)",
            Style::default().fg(ctx.theme.dim_text),
        )));
        return None;
    }
    let result = diagram_lines(
        state,
        ctx.highlighted_node_id,
        ctx.follow,
        area_width,
        ctx.theme,
    )
    .unwrap_or_else(|| {
        graph_lines(
            state,
            ctx.highlighted_node_id,
            ctx.follow,
            area_width,
            ctx.theme,
        )
    });
    let offset = result.highlighted_offset;
    let span = result.highlighted_span;
    lines.extend(result.lines);
    Some((offset, span))
}

/// CT8: who owns the scroll.
///
/// Auto-follow: re-derive the scroll from the highlighted node ONLY on the
/// frame where the current node's identity changed since the last
/// re-centre (`ctx.follow_anchor`). While `follow` is true,
/// `highlighted_node_id` == the engine's current node id, so this fires on
/// a real node-to-node transition and on nothing else — not a status
/// change, not elapsed time, not the user scrolling. Every other redraw
/// keeps `clamped_from_input`, the user's own scroll, so the whole graph
/// of a running graph can be read.
///
/// Manual navigation: the scroll always chases the selection with the same
/// margin `ensure_visible` uses, so the highlight can never leave the panel.
fn live_view_scroll(
    ctx: &LiveViewContext,
    graph_offset: Option<u16>,
    graph_start_line: u16,
    clamped_from_input: u16,
    area_height: u16,
    max_scroll: u16,
    graph_span: Option<u16>,
) -> (u16, Option<String>) {
    let span = graph_span.unwrap_or(3);
    if ctx.follow {
        let node_changed = ctx.highlighted_node_id != ctx.follow_anchor;
        let scroll = if node_changed {
            match graph_offset {
                Some(offset) => centre_on(graph_start_line + offset, span, area_height, max_scroll),
                None => clamped_from_input,
            }
        } else {
            clamped_from_input
        };
        return (scroll, ctx.highlighted_node_id.map(str::to_string));
    }
    (
        match graph_offset {
            Some(offset) => ensure_visible(
                graph_start_line + offset,
                span,
                clamped_from_input,
                area_height,
            ),
            None => clamped_from_input,
        },
        ctx.follow_anchor.map(str::to_string),
    )
}

/// CT27: place the highlighted box of `span` lines, starting at absolute
/// line `start`, in the middle of a panel of `height` lines. A box that
/// fits gets its middle line on the panel's middle line; a box taller than
/// the panel gets its top border on the first or second visible line so at
/// least its head is readable. Always clamped to `max_scroll` so it never
/// scrolls past the content.
fn centre_on(start: u16, span: u16, height: u16, max_scroll: u16) -> u16 {
    if height == 0 {
        return start.min(max_scroll);
    }
    let scroll = if span <= height {
        start.saturating_add(span / 2).saturating_sub(height / 2)
    } else {
        start.saturating_sub(1)
    };
    scroll.min(max_scroll)
}

fn ensure_visible(start: u16, span: u16, scroll: u16, height: u16) -> u16 {
    let end = start.saturating_add(span);
    let view_end = scroll.saturating_add(height);
    if height == 0 {
        return scroll;
    }
    if start < scroll {
        start
    } else if end > view_end {
        end.saturating_sub(height)
    } else {
        scroll
    }
}

fn status_icon_and_label(
    state: &GraphLiveState,
    blocked: bool,
    theme: &Theme,
    time_format: &str,
) -> (&'static str, String, Color) {
    if let Some(at) = state.autorun_at {
        let local = at.with_timezone(&Local);
        return (
            "◷",
            format!("autorun {}", local.format(time_format)),
            Color::Cyan,
        );
    }
    if blocked {
        return ("⊘", "blocked".to_string(), theme.status_fail);
    }
    match state.graph_status {
        GraphStatus::Running => ("▶", "running".to_string(), theme.status_running),
        GraphStatus::Pausing => ("‖", pausing_label(&state.pause_waiting_for), Color::Yellow),
        GraphStatus::Paused => ("‖", "paused".to_string(), Color::Yellow),
        GraphStatus::Completed => ("✓", "completed".to_string(), theme.status_ok),
        GraphStatus::Failed => ("✗", "failed".to_string(), theme.status_fail),
        GraphStatus::Draft => ("·", "draft".to_string(), theme.dim_text),
    }
}

fn header_lines(
    state: &GraphLiveState,
    blocked: bool,
    theme: &Theme,
    time_format: &str,
) -> Vec<Line<'static>> {
    let (icon, label, color) = status_icon_and_label(state, blocked, theme, time_format);
    vec![
        Line::from(vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                state.graph_name.clone(),
                Style::default()
                    .fg(theme.header_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(label, Style::default().fg(color)),
            Span::raw("   "),
            Span::styled(
                format!("{}/{} specs", state.done_count, state.total_count),
                Style::default().fg(theme.dim_text),
            ),
        ]),
        Line::from(Span::styled(
            format!("Workdir: {}", compact_cwd(&state.workdir)),
            Style::default().fg(theme.dim_text),
        )),
    ]
}

fn format_elapsed(started_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - started_at).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn run_status_span(status: Option<GraphRunStatus>, theme: &Theme) -> Span<'static> {
    match status {
        Some(GraphRunStatus::Running) => {
            Span::styled("running", Style::default().fg(theme.status_running))
        }
        Some(GraphRunStatus::Waiting) => {
            Span::styled("waiting", Style::default().fg(theme.status_running))
        }
        Some(GraphRunStatus::Pass) => Span::styled("pass", Style::default().fg(theme.status_ok)),
        Some(GraphRunStatus::Fail) => Span::styled("fail", Style::default().fg(theme.status_fail)),
        Some(GraphRunStatus::Error) => Span::styled(
            "error (no verdict)",
            Style::default().fg(theme.status_interrupted),
        ),
        Some(GraphRunStatus::Interrupted) => {
            Span::styled("interrupted", Style::default().fg(theme.status_fail))
        }
        None => Span::styled("(no runs yet)", Style::default().fg(theme.dim_text)),
    }
}

fn footer_lines(
    state: &GraphLiveState,
    highlighted_node_id: Option<&str>,
    node_info: &NodeRunInfo,
    follow: bool,
    now: DateTime<Utc>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let Some(node_id) = highlighted_node_id else {
        return vec![Line::from(Span::styled(
            "(no node selected)",
            Style::default().fg(theme.dim_text),
        ))];
    };
    let Some(node) = state.effective_nodes.iter().find(|n| n.id == node_id) else {
        return vec![Line::from(Span::styled(
            "(node not found)",
            Style::default().fg(theme.dim_text),
        ))];
    };

    let mode_label = if follow { "auto-follow" } else { "manual" };
    let mut lines = vec![Line::from(vec![
        Span::styled(
            node.name.clone(),
            Style::default()
                .fg(theme.header_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("[{}]", node.kind.display_str()),
            Style::default().fg(theme.dim_text),
        ),
        Span::raw("  "),
        Span::styled(
            format!("({mode_label})"),
            Style::default().fg(theme.dim_text),
        ),
    ])];

    let mut meta = vec![run_status_span(node_info.status, theme)];
    if let Some(started_at) = node_info.started_at {
        meta.push(Span::raw("  "));
        meta.push(Span::styled(
            format!("elapsed {}", format_elapsed(started_at, now)),
            Style::default().fg(theme.dim_text),
        ));
    }
    if let Some(iteration) = node_info.iteration {
        meta.push(Span::raw("  "));
        meta.push(Span::styled(
            format!("iter {iteration}"),
            Style::default().fg(theme.dim_text),
        ));
    }
    if let Some(route) = node_info.chosen_route.as_deref() {
        meta.push(Span::raw("  "));
        meta.push(Span::styled(
            format!("route → {route}"),
            Style::default()
                .fg(theme.status_ok)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // CM24: when the highlighted node is ensemble-owned (a member or the
    // join), show the join's straggler/grace config side by side — the only
    // place the TUI surfaces ensemble quorum tuning.
    if let Some(ensemble) = state.ensembles.iter().find(|ensemble| {
        ensemble.join_node_id == node_id || ensemble.members.iter().any(|m| m.node_id == node_id)
    }) {
        if let Some(straggler) = ensemble.straggler_timeout_minutes {
            meta.push(Span::raw("  "));
            meta.push(Span::styled(
                format!("straggler {straggler}m"),
                Style::default().fg(theme.dim_text),
            ));
        }
        if let Some(grace) = ensemble.quorum_grace_minutes {
            meta.push(Span::raw("  "));
            meta.push(Span::styled(
                if grace == 0 {
                    "grace immediate".to_string()
                } else {
                    format!("grace {grace}m")
                },
                Style::default().fg(theme.dim_text),
            ));
        }
    }
    lines.push(Line::from(meta));
    if !node_info.waiting_for.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("waiting for: {}", waiting_for_text(&node_info.waiting_for)),
            Style::default().fg(theme.dim_text),
        )));
    }

    lines
}

/// One-line holder summary for a mutex-waiting run: each resource with who
/// holds it (`cpu (held by graph/node)`), or bare while the resource is free
/// but an earlier waiter is ahead in FIFO order.
fn waiting_for_text(waiting_for: &[MutexWaitDetail]) -> String {
    waiting_for
        .iter()
        .map(|detail| match (&detail.holder_graph, &detail.holder_node) {
            (Some(graph), Some(holder)) => {
                format!("{} (held by {graph}/{holder})", detail.resource)
            }
            _ => detail.resource.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{GraphNode, GraphNodeKind, GraphStatus};
    use crate::tui::app::graph_live_state::{EnsembleLiveInfo, EnsembleMemberLiveInfo};
    use serde_json::json;

    fn blank_state() -> GraphLiveState {
        GraphLiveState {
            graph_id: String::new(),
            graph_name: String::new(),
            graph_status: GraphStatus::Running,
            workdir: String::new(),
            trigger_type: "manual".to_string(),
            schedule_expr: None,
            watch_path: None,
            autorun_at: None,
            pause_waiting_for: Vec::new(),
            spec_queue: Vec::new(),
            done_count: 0,
            total_count: 0,
            current_spec_id: None,
            effective_nodes: Vec::new(),
            effective_edges: Vec::new(),
            ensembles: Vec::new(),
            router_taken_routes: std::collections::HashMap::new(),
            current_node_id: None,
            current_node_status: None,
            current_node_started_at: None,
            current_node_iteration: None,
        }
    }

    fn scroll_ctx<'a>(
        state: &'a GraphLiveState,
        node_info: &'a NodeRunInfo,
        theme: &'a Theme,
        follow: bool,
        highlighted_node_id: Option<&'a str>,
        follow_anchor: Option<&'a str>,
        scroll: u16,
    ) -> LiveViewContext<'a> {
        LiveViewContext {
            state,
            follow,
            follow_anchor,
            highlighted_node_id,
            node_info,
            blocked: false,
            now: Utc::now(),
            theme,
            time_format: "%H:%M",
            selected_spec_id: None,
            spec_scroll: 0,
            scroll,
        }
    }

    fn agent_node(id: &str, position: i64) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: id.to_string(),
            kind: GraphNodeKind::Agent,
            config: json!({}),
            position,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn auto_follow_centres_tall_box_in_panel() {
        // (a) Forward transition to a 10-line box in a 20-line panel: the
        // box's middle line must sit on panel row 10 ± 1.
        let state = blank_state();
        let info = NodeRunInfo::default();
        let theme = Theme::classic();
        let ctx = scroll_ctx(&state, &info, &theme, true, Some("new"), Some("old"), 0);
        let (scroll, anchor) = live_view_scroll(&ctx, Some(10), 0, 0, 20, u16::MAX, Some(10));
        assert_eq!(scroll, 5);
        assert_eq!(anchor.as_deref(), Some("new"));
        for middle in [14u16, 15u16] {
            let row = middle - scroll;
            assert!(
                (9..=11).contains(&row),
                "box middle line {middle} lands on panel row {row}, expected 10 ± 1"
            );
        }
    }
    // Breaks if: auto-follow keeps the old span-3 `ensure_visible` (which
    // would leave scroll at 0 and the box hanging off the bottom edge).

    #[test]
    fn centre_on_taller_than_panel_anchors_top_border() {
        // (b) A 30-line box in a 20-line panel: top border on row 0 or 1.
        assert_eq!(centre_on(5, 30, 20, u16::MAX), 4);
        assert_eq!(5 - centre_on(5, 30, 20, u16::MAX), 1);
        assert_eq!(centre_on(0, 30, 20, u16::MAX), 0);
    }
    // Breaks if: `centre_on` uses `start` instead of `start.saturating_sub(1)`
    // for the taller-than-panel case.

    #[test]
    fn auto_follow_near_end_of_content_clamps_without_overscroll() {
        // (c) A box near the end of the content: the centred scroll is
        // clamped to `max_scroll` inside `centre_on`, so the view never
        // scrolls past the content.
        let state = blank_state();
        let info = NodeRunInfo::default();
        let theme = Theme::classic();
        let ctx = scroll_ctx(&state, &info, &theme, true, Some("last"), Some("prev"), 0);
        // centre_on(35, 5, 20) = 35 + 2 - 10 = 27, but only 20 lines of
        // content follow row 0, so it must clamp to 20.
        assert_eq!(centre_on(35, 5, 20, u16::MAX), 27);
        let (scroll, _) = live_view_scroll(&ctx, Some(35), 0, 0, 20, 20, Some(5));
        assert_eq!(scroll, 20, "centred scroll must clamp to max_scroll");
        assert!(
            scroll <= 20,
            "scroll {scroll} overscrolls past max_scroll 20"
        );
    }
    // Breaks if: `centre_on` drops its `max_scroll` clamp (the unclamped
    // value 27 would be returned and scroll past the content).

    #[test]
    fn auto_follow_keeps_user_scroll_when_node_unchanged() {
        // (d) CT8 regression: a redraw without a node change keeps the
        // user's own scroll.
        let state = blank_state();
        let info = NodeRunInfo::default();
        let theme = Theme::classic();
        let ctx = scroll_ctx(&state, &info, &theme, true, Some("m1"), Some("m1"), 12);
        let (scroll, anchor) = live_view_scroll(&ctx, Some(5), 3, 12, 20, u16::MAX, Some(10));
        assert_eq!(scroll, 12);
        assert_eq!(anchor.as_deref(), Some("m1"));
    }
    // Breaks if: the `node_changed` guard is removed and auto-follow
    // re-centres every frame again.

    #[test]
    fn manual_selection_brings_whole_tall_box_into_view() {
        // (e) Manual selection of a 10-line box below the viewport: all 10
        // lines end up visible.
        let state = blank_state();
        let info = NodeRunInfo::default();
        let theme = Theme::classic();
        let ctx = scroll_ctx(&state, &info, &theme, false, Some("big"), None, 0);
        let (scroll, _) = live_view_scroll(&ctx, Some(30), 3, 0, 20, u16::MAX, Some(10));
        assert_eq!(scroll, 23);
        assert!(
            33 >= scroll && 43 <= scroll + 20,
            "box lines 33..43 must sit inside viewport {scroll}..{}",
            scroll + 20
        );
    }
    // Breaks if: the manual branch falls back to the hardcoded span of 3
    // (which would scroll to 16 and leave most of the box cut off).

    #[test]
    fn highlighted_span_measures_plain_and_ensemble_boxes() {
        let theme = Theme::classic();
        let mut state = blank_state();
        state.effective_nodes = vec![agent_node("n", 0)];
        let plain = graph_lines(&state, Some("n"), true, 80, &theme);
        assert_eq!(plain.highlighted_offset, Some(0));
        assert_eq!(plain.highlighted_span, Some(3));

        state.effective_nodes = vec![agent_node("m0", 0), agent_node("j", 1)];
        state.ensembles = vec![EnsembleLiveInfo {
            ensemble_id: "ens".to_string(),
            name: "Designer".to_string(),
            join_node_id: "j".to_string(),
            members: vec![EnsembleMemberLiveInfo {
                node_id: "m0".to_string(),
                label: "m0".to_string(),
                status: None,
            }],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }];
        let collapsed = graph_lines(&state, Some("m0"), true, 80, &theme);
        assert_eq!(collapsed.highlighted_offset, Some(0));
        assert_eq!(collapsed.highlighted_span, Some(4));
    }
    // Breaks if: `highlighted_span` is not measured after `dfs_push_box`
    // (plain box 3 lines, 1-member ensemble box 4 lines).

    fn buffer_row_text(buf: &ratatui::buffer::Buffer, y: u16, width: u16) -> String {
        (0..width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn strip_remains_fixed_at_top_when_body_scrolls() {
        use crate::domain::graphs::{GraphEdge, GraphEdgeCondition, GraphSpecStatus};
        use crate::tui::app::graph_live_state::SpecQueueEntry;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = blank_state();
        state.graph_name = "demo".to_string();
        state.spec_queue = vec![SpecQueueEntry {
            spec_id: "s1".to_string(),
            spec_name: "pinned-spec".to_string(),
            spec_code: None,
            spec_type: None,
            spec_scope: None,
            status: GraphSpecStatus::Running,
            failure_reason: None,
        }];
        state.current_spec_id = Some("s1".to_string());
        state.effective_nodes = (0..10).map(|i| agent_node(&format!("n{i}"), i)).collect();
        state.effective_edges = (0..9)
            .map(|i| GraphEdge {
                id: format!("e{i}"),
                spec_id: Some("s1".to_string()),
                graph_id: None,
                from_node: format!("n{i}"),
                to_node: format!("n{}", i + 1),
                condition: GraphEdgeCondition::Pass,
            })
            .collect();

        let info = NodeRunInfo::default();
        let theme = Theme::classic();
        let draw_at = |scroll: u16| -> (Vec<String>, LiveViewRenderResult) {
            let mut terminal = Terminal::new(TestBackend::new(60, 18)).unwrap();
            let mut res = None;
            terminal
                .draw(|frame| {
                    let area = frame.area();
                    res = Some(render_graph_live_view(
                        frame,
                        area,
                        &scroll_ctx(&state, &info, &theme, false, Some("n0"), None, scroll),
                    ));
                })
                .unwrap();
            let buf = terminal.backend().buffer().clone();
            let rows = (0..18).map(|y| buffer_row_text(&buf, y, 60)).collect();
            (rows, res.unwrap())
        };

        let (rows0, res0) = draw_at(0);
        let (rows_scrolled, res_scrolled) = draw_at(500);
        // Header (2) + blank (1) + strip chips (1) + spec detail (1) = 5 top rows
        assert_eq!(&rows0[..5], &rows_scrolled[..5]);
        assert!(rows_scrolled[4].contains("pinned-spec"));
        assert!(res_scrolled.clamped_scroll > 0);
        assert_eq!(res0.total_lines, res_scrolled.total_lines);
    }

    #[test]
    fn max_scroll_matches_total_minus_full_height_and_shows_footer_end() {
        use crate::domain::graphs::{GraphEdge, GraphEdgeCondition};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = blank_state();
        state.graph_name = "demo".to_string();
        state.effective_nodes = (0..10).map(|i| agent_node(&format!("n{i}"), i)).collect();
        state.effective_edges = (0..9)
            .map(|i| GraphEdge {
                id: format!("e{i}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: format!("n{i}"),
                to_node: format!("n{}", i + 1),
                condition: GraphEdgeCondition::Pass,
            })
            .collect();

        let info = NodeRunInfo {
            waiting_for: vec![MutexWaitDetail {
                resource: "cpu".to_string(),
                holder_graph: None,
                holder_node: None,
                since: None,
            }],
            ..NodeRunInfo::default()
        };
        let theme = Theme::classic();
        let full_height = 18u16;
        let mut terminal = Terminal::new(TestBackend::new(60, full_height)).unwrap();
        let mut res = None;
        terminal
            .draw(|frame| {
                let area = frame.area();
                res = Some(render_graph_live_view(
                    frame,
                    area,
                    &scroll_ctx(&state, &info, &theme, false, None, None, u16::MAX),
                ));
            })
            .unwrap();
        let res = res.unwrap();
        assert_eq!(res.clamped_scroll, res.total_lines - full_height);
        let buf = terminal.backend().buffer();
        let last_row = buffer_row_text(buf, full_height - 1, 60);
        assert!(
            last_row.contains("(no node selected)"),
            "expected footer last line at max scroll, got: {last_row:?}"
        );
    }

    #[test]
    fn narrow_panel_falls_back_to_vertical_edge_labels() {
        use crate::domain::graphs::{GraphEdge, GraphEdgeCondition};
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let mut state = blank_state();
        state.graph_name = "demo".to_string();
        state.effective_nodes = vec![agent_node("a", 0), agent_node("b", 1)];
        state.effective_edges = vec![GraphEdge {
            id: "e1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "a".to_string(),
            to_node: "b".to_string(),
            condition: GraphEdgeCondition::Pass,
        }];

        let info = NodeRunInfo::default();
        let theme = Theme::classic();
        let mut terminal = Terminal::new(TestBackend::new(29, 40)).unwrap();
        terminal
            .draw(|frame| {
                let area = frame.area();
                render_graph_live_view(
                    frame,
                    area,
                    &scroll_ctx(&state, &info, &theme, false, Some("a"), None, 0),
                );
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        let text = (0..40)
            .map(|y| buffer_row_text(&buf, y, 29))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("pass →"),
            "width-29 panel must fall back to the vertical list with edge labels, got:\n{text}"
        );
    }
    // Breaks if: `push_graph_lines` stops falling back to `graph_lines`
    // when `diagram_lines` returns `None` (the diagram draws no edge labels).
}
