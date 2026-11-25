//! Header, footer, and status-formatting helpers for the live graph view.
//! Split out of `render.rs` to keep every production file within the
//! file-size gate.

use chrono::{DateTime, Local, Utc};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::db::mutex_store::MutexWaitDetail;
use crate::domain::graphs::{GraphRunStatus, GraphStatus};
use crate::tui::app::graph_live_state::{pausing_label, GraphLiveState, NodeRunInfo};
use crate::tui::ui::panel::compact_cwd;
use crate::tui::ui::theme::Theme;

pub(super) fn status_icon_and_label(
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

pub(super) fn header_lines(
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

pub(super) fn format_elapsed(started_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - started_at).num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

pub(super) fn run_status_span(status: Option<GraphRunStatus>, theme: &Theme) -> Span<'static> {
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

pub(super) fn footer_lines(
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
pub(super) fn waiting_for_text(waiting_for: &[MutexWaitDetail]) -> String {
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
