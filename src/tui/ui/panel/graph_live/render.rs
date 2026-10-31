//! The pure renderer behind `draw_graph_live_view`: header, spec marker
//! strip, graph body, and detail footer. Split out of `graph_live.rs` to
//! keep every production file within the file-size gate.

use chrono::{DateTime, Local, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::lines::graph_lines;
use super::{LiveViewContext, LiveViewRenderResult};
use crate::db::mutex_store::MutexWaitDetail;
use crate::domain::graphs::{GraphRunStatus, GraphSpecStatus, GraphStatus};
use crate::tui::app::graph_live_state::{
    pausing_label, GraphLiveState, NodeRunInfo, SpecQueueEntry,
};
use crate::tui::ui::panel::compact_cwd;
use crate::tui::ui::sidebar::draw_scroll_indicators;
use crate::tui::ui::theme::Theme;

pub(super) fn render_graph_live_view(
    frame: &mut Frame,
    area: Rect,
    ctx: &LiveViewContext,
) -> LiveViewRenderResult {
    let state = ctx.state;
    let mut lines = header_lines(state, ctx.blocked, ctx.theme, ctx.time_format);
    lines.push(Line::from(""));

    let chip_row = area.y + lines.len() as u16;
    let strip = spec_strip_layout(
        state,
        ctx.selected_spec_id,
        ctx.spec_scroll,
        area.width,
        area.height,
        ctx.theme,
    );
    lines.extend(strip.lines);

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Graph",
        Style::default().fg(ctx.theme.dim_text),
    )));
    let graph_start_line = lines.len() as u16;
    let (graph_offset, graph_span) = push_graph_lines(&mut lines, state, ctx, area.width).unzip();
    let graph_offset = graph_offset.flatten();
    let graph_span = graph_span.flatten();
    lines.push(Line::from(""));
    lines.extend(footer_lines(
        state,
        ctx.highlighted_node_id,
        ctx.node_info,
        ctx.follow,
        ctx.now,
        ctx.theme,
    ));

    let total_lines = lines.len() as u16;
    let max_scroll = total_lines.saturating_sub(area.height);
    let clamped_from_input = ctx.scroll.min(max_scroll);

    let (scroll, next_follow_anchor) = live_view_scroll(
        ctx,
        graph_offset,
        graph_start_line,
        clamped_from_input,
        area.height,
        max_scroll,
        graph_span,
    );
    let clamped_scroll = scroll.min(max_scroll);

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((clamped_scroll, 0))
            .style(Style::default().bg(ctx.theme.info_bg)),
        area,
    );

    let has_up = clamped_scroll > 0;
    let has_down = total_lines > area.height && clamped_scroll < max_scroll;
    if has_up || has_down {
        draw_scroll_indicators(frame, area, has_up, has_down, ctx.theme);
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
        total_lines,
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
    let result = graph_lines(
        state,
        ctx.highlighted_node_id,
        ctx.follow,
        area_width,
        ctx.theme,
    );
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

/// Chip glyph for a queue entry: the current spec always shows `▶`
/// regardless of its underlying status (running or the next pending one —
/// see `assemble_graph_live_state`'s `current_spec_id` rule); otherwise the
/// glyph reflects the terminal status directly, since that's exactly the
/// case a user goes looking for (which spec failed vs. was skipped).
fn spec_chip(
    entry: &SpecQueueEntry,
    current_spec_id: Option<&str>,
    theme: &Theme,
) -> (&'static str, Color) {
    if Some(entry.spec_id.as_str()) == current_spec_id {
        return ("▶", theme.status_running);
    }
    match entry.status {
        GraphSpecStatus::Pending => ("○", theme.dim_text),
        GraphSpecStatus::Running => ("▶", theme.status_running),
        GraphSpecStatus::Completed => ("✓", theme.status_ok),
        GraphSpecStatus::Failed => ("✗", theme.status_fail),
        GraphSpecStatus::Skipped => ("⊘", theme.status_disabled),
        GraphSpecStatus::Interrupted => ("⚑", theme.status_interrupted),
    }
}

/// A marker chip's fixed on-screen footprint: a 3-column core (bracketed
/// when selected, plain otherwise) plus a 1-column gap, so the strip's
/// column arithmetic never has to special-case which chip is selected.
pub(super) const SPEC_CHIP_WIDTH: u16 = 4;

/// Columns reserved for the "(a-b of N)" suffix when the strip has to
/// truncate — wide enough for three-digit spec counts on either side.
const SPEC_STRIP_RANGE_SUFFIX_WIDTH: u16 = 14;

/// The marker strip's rendered lines, its click map (spec id + row offset +
/// column span, relative to the strip's own origin — the caller translates
/// to absolute screen coordinates), and how many chips fit in the given
/// width.
pub(super) struct SpecStripLayout {
    pub(super) lines: Vec<Line<'static>>,
    pub(super) click_map: Vec<(String, u16, u16, u16)>,
    pub(super) capacity: usize,
    #[allow(dead_code)]
    pub(super) chip_line_count: usize,
}

/// Lay out the spec marker strip: status chips wrapped across multiple lines
/// to fill available height; compressed to a scrollable window with a
/// "(a-b of N)" indicator only when even multi-line wrapping cannot fit all
/// specs — followed by the selected spec's detail, falling back to the
/// running/next-pending spec when nothing is manually selected.
pub(super) fn spec_strip_layout(
    state: &GraphLiveState,
    selected_spec_id: Option<&str>,
    scroll: usize,
    area_width: u16,
    area_height: u16,
    theme: &Theme,
) -> SpecStripLayout {
    if state.spec_queue.is_empty() {
        return SpecStripLayout {
            lines: vec![Line::from(Span::styled(
                "Queue: (empty)",
                Style::default().fg(theme.dim_text),
            ))],
            click_map: Vec::new(),
            capacity: 0,
            chip_line_count: 1,
        };
    }

    let total = state.spec_queue.len();
    let chips_per_line = (area_width / SPEC_CHIP_WIDTH).max(1) as usize;
    let lines_needed = total.div_ceil(chips_per_line);
    const RESERVED_NON_CHIP_LINES: u16 = 8;
    let max_chip_lines = area_height.saturating_sub(RESERVED_NON_CHIP_LINES).max(1) as usize;
    let chip_lines = lines_needed.min(max_chip_lines);
    let truncated = total > chip_lines * chips_per_line;
    let capacity = if truncated {
        // Round up: reserving 3 whole chips (12 cols) for a 14-col suffix
        // leaves the "(a-b of N)" text spilling past the line and soft-wrapping
        // onto an extra visual row. Reserve the ceiling so it fits.
        let last_line_reserved = SPEC_STRIP_RANGE_SUFFIX_WIDTH
            .div_ceil(SPEC_CHIP_WIDTH)
            .max(1) as usize;
        chip_lines.saturating_sub(1) * chips_per_line
            + chips_per_line.saturating_sub(last_line_reserved)
    } else {
        chip_lines * chips_per_line
    };
    let capacity = capacity.max(1);
    let start = scroll.min(total.saturating_sub(capacity));
    let end = (start + capacity).min(total);

    let mut chip_lines_vec: Vec<Line<'static>> = Vec::new();
    let mut click_map: Vec<(String, u16, u16, u16)> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut col: u16 = 0;
    let mut row_offset: u16 = 0;

    for entry in &state.spec_queue[start..end] {
        if col > 0 && col + SPEC_CHIP_WIDTH > area_width {
            chip_lines_vec.push(Line::from(std::mem::take(&mut current_spans)));
            row_offset += 1;
            col = 0;
        }
        let (icon, color) = spec_chip(entry, state.current_spec_id.as_deref(), theme);
        let selected = Some(entry.spec_id.as_str()) == selected_spec_id;
        let core = if selected {
            format!("[{icon}]")
        } else {
            format!(" {icon} ")
        };
        let style = if selected {
            Style::default().fg(color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(color)
        };
        let core_width = core.chars().count() as u16;
        click_map.push((entry.spec_id.clone(), row_offset, col, col + core_width));
        current_spans.push(Span::styled(core, style));
        current_spans.push(Span::raw(" "));
        col += SPEC_CHIP_WIDTH;
    }
    if truncated {
        current_spans.push(Span::styled(
            format!(" ({}-{} of {})", start + 1, end, total),
            Style::default().fg(theme.dim_text),
        ));
    }
    if !current_spans.is_empty() || chip_lines_vec.is_empty() {
        chip_lines_vec.push(Line::from(current_spans));
    }

    let chip_line_count = chip_lines_vec.len();
    let mut lines = chip_lines_vec;
    lines.extend(spec_detail_lines(state, selected_spec_id, theme));

    SpecStripLayout {
        lines,
        click_map,
        capacity,
        chip_line_count,
    }
}

/// Name, status, and (for a failed/skipped spec) the recorded reason for
/// whichever spec the marker strip is showing: the manual selection if
/// there is one, else the running/next-pending spec — the same fallback
/// the strip's detail line always showed before selection existed.
fn spec_detail_lines(
    state: &GraphLiveState,
    selected_spec_id: Option<&str>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let entry = selected_spec_id
        .and_then(|id| state.spec_queue.iter().find(|e| e.spec_id == id))
        .or_else(|| {
            state
                .spec_queue
                .iter()
                .find(|e| Some(e.spec_id.as_str()) == state.current_spec_id.as_deref())
        });
    let Some(entry) = entry else {
        return Vec::new();
    };

    let (_, color) = spec_chip(entry, state.current_spec_id.as_deref(), theme);
    let mut lines = Vec::new();

    // SPECCODE: Show code badge before name, then dimmed type/scope
    let mut name_line_parts = Vec::new();
    if let Some(ref code) = entry.spec_code {
        name_line_parts.push(Span::styled(
            format!("[{code}] "),
            Style::default().fg(theme.status_ok),
        ));
    }
    name_line_parts.push(Span::styled(
        entry.spec_name.clone(),
        Style::default()
            .fg(theme.text_primary)
            .add_modifier(Modifier::BOLD),
    ));

    // SPECCODE: dimmed type/scope after name
    if let Some(ref spec_type) = entry.spec_type {
        let type_str = spec_type.as_str();
        if let Some(ref scope) = entry.spec_scope {
            name_line_parts.push(Span::raw(" "));
            name_line_parts.push(Span::styled(
                format!("({type_str}:{scope}) "),
                Style::default().fg(theme.dim_text),
            ));
        } else {
            name_line_parts.push(Span::raw(" "));
            name_line_parts.push(Span::styled(
                format!("({type_str}) "),
                Style::default().fg(theme.dim_text),
            ));
        }
    }

    name_line_parts.push(Span::raw(" "));
    name_line_parts.push(Span::styled(
        format!("[{}]", spec_status_label(entry.status)),
        Style::default().fg(color),
    ));

    lines.push(Line::from(name_line_parts));

    if let Some(reason) = entry.failure_reason.as_deref() {
        lines.push(Line::from(Span::styled(
            format!("  {reason}"),
            Style::default().fg(theme.dim_text),
        )));
    }
    lines
}

fn spec_status_label(status: GraphSpecStatus) -> &'static str {
    match status {
        GraphSpecStatus::Pending => "pending",
        GraphSpecStatus::Running => "running",
        GraphSpecStatus::Completed => "completed",
        GraphSpecStatus::Failed => "failed",
        GraphSpecStatus::Skipped => "skipped",
        GraphSpecStatus::Interrupted => "interrupted",
    }
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
}
