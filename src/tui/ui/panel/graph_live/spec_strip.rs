//! Spec queue marker strip layout and detail lines for the live graph view.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::domain::graphs::GraphSpecStatus;
use crate::tui::app::graph_live_state::{GraphLiveState, SpecQueueEntry};
use crate::tui::ui::theme::Theme;

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
