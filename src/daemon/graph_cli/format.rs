use anyhow::Result;
use chrono::{DateTime, Utc};

use crate::db::Database;
use crate::domain::graphs::{
    Graph, GraphCrewRow, GraphCrewSummary, GraphNodeRun, GraphRunStatus, GraphSpec,
    GraphSpecStatus, GraphStatus,
};

pub(super) fn commit_rights_note(output: Option<&serde_json::Value>) -> String {
    let Some(violation) = output.and_then(|o| o.get("commit_rights_violation")) else {
        return String::new();
    };
    let head_after = violation
        .get("head_after")
        .and_then(|v| v.as_str())
        .unwrap_or("?");
    format!(
        "  \x1b[31m(no commit rights — HEAD moved to {})\x1b[0m",
        head_after.chars().take(8).collect::<String>()
    )
}

/// The `graph info` annotation for a run cut short by a daemon restart/kill
/// (B36), or an empty string for every other run — pairs with the distinct
/// icon `run_status_icon` already gives it, so a `fail` here never reads as
/// the node's own doing. The engine never touches git on this path, so
/// there's nothing to point the operator at recovering — the spec's own
/// `interrupted` status (visible via `graph_info`'s spec listing) is what
/// says the worktree still holds the interrupted attempt's partial work.
pub(super) fn interrupted_note(output: Option<&serde_json::Value>) -> String {
    if !is_interrupted(output) {
        return String::new();
    }
    "  \x1b[33m(interrupted — not a node failure)\x1b[0m".to_string()
}

/// The `graph info` annotation for a quorum join that settled with no filed
/// verdict (CB66), or an empty string for every other run. Pairs with the
/// distinct icon `run_status_icon` gives `Error`, so a no-verdict exit never
/// reads as an ordinary fail.
pub(super) fn no_verdict_note(output: Option<&serde_json::Value>) -> String {
    let is_no_verdict = output
        .and_then(|o| o.get("no_verdict"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !is_no_verdict {
        return String::new();
    }
    "  \x1b[33merror (no verdict)\x1b[0m".to_string()
}

/// Count of specs that have reached a final `completed` state, alongside the
/// total — used to render `done/total` progress in `graph list`/`graph info`.
pub(super) fn spec_progress(specs: &[GraphSpec]) -> (usize, usize) {
    let done = specs
        .iter()
        .filter(|s| s.status == GraphSpecStatus::Completed)
        .count();
    (done, specs.len())
}

/// `done/total` progress for a graph's `graph list` row. A queue-driven run binds
/// no specs of its own (`graph_specs.graph_id` stays null for queue members), so
/// counting `bound_specs` renders a misleading `0/0`. When the graph's row
/// carries an `active_run_queue_id`, count that queue's members instead —
/// mirroring `GraphEngine::spec_progress`, which the running engine uses for the
/// same graph. Falls back to the bound specs for ordinary (non-queue) graphs.
pub(super) fn graph_progress(
    db: &Database,
    lp: &Graph,
    bound_specs: &[GraphSpec],
) -> Result<(usize, usize)> {
    let Some(queue_id) = lp.active_run_queue_id.as_deref() else {
        return Ok(spec_progress(bound_specs));
    };
    let ids = db.list_queue_member_spec_ids(queue_id)?;
    let mut done = 0;
    for id in &ids {
        if let Some(spec) = db.get_graph_spec(id)? {
            if spec.status == GraphSpecStatus::Completed {
                done += 1;
            }
        }
    }
    Ok((done, ids.len()))
}

/// The spec a graph is actively working through: the one currently `running`,
/// or else the next `pending`/`interrupted` one (equally runnable — see
/// `queue_next_pending_spec_id`) in position order. Mirrors
/// `build_graph_summary_json` in `daemon/handler.rs` so the CLI and MCP report
/// the same "current spec" for a given graph. The blank-name placeholder is
/// engine bookkeeping, never work: it is skipped here, not reported.
pub(super) fn current_spec(specs: &[GraphSpec]) -> Option<&GraphSpec> {
    specs
        .iter()
        .filter(|s| !s.name.trim().is_empty())
        .find(|s| s.status == GraphSpecStatus::Running)
        .or_else(|| {
            specs
                .iter()
                .filter(|s| !s.name.trim().is_empty())
                .find(|s| {
                    matches!(
                        s.status,
                        GraphSpecStatus::Pending | GraphSpecStatus::Interrupted
                    )
                })
        })
}

/// Name of the spec a graph is actively working through, for both bound-spec
/// graphs (via [`current_spec`]) and queue-driven graphs, which never bind a
/// spec to `graph_specs.graph_id` and so must fall back to `graph_runs` (see
/// [`Database::list_graph_runs_for_graph`]) to find what's currently running.
/// A blank-name placeholder resolves to `None`, never to an empty name.
pub(super) fn current_spec_name(
    db: &Database,
    lp: &Graph,
    bound_specs: &[GraphSpec],
) -> Result<Option<String>> {
    if let Some(spec) = current_spec(bound_specs) {
        return Ok(Some(spec.name.clone()));
    }
    let runs = db.list_graph_runs_for_graph(&lp.id)?;
    let Some(run) = current_running_run(&runs).or_else(|| runs.last()) else {
        return Ok(None);
    };
    Ok(db
        .get_graph_spec(&run.spec_id)?
        .filter(|s| !s.name.trim().is_empty())
        .map(|s| s.name))
}

/// The run currently in flight, if any — assumes `runs` is ordered by
/// `started_at` ascending (as every `list_graph_runs_for_*` query returns it),
/// so the last matching entry is the most recent.
pub(super) fn current_running_run(runs: &[GraphNodeRun]) -> Option<&GraphNodeRun> {
    runs.iter()
        .rev()
        .find(|r| r.status == GraphRunStatus::Running)
}

/// Spec ids referenced by `runs`, in first-seen (chronological) order with
/// duplicates dropped — used to approximate a queue-driven graph's queue from
/// its run history, since the queue itself isn't persisted per graph.
pub(super) fn distinct_spec_ids_in_order(runs: &[GraphNodeRun]) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for run in runs {
        if seen.insert(run.spec_id.as_str()) {
            out.push(run.spec_id.as_str());
        }
    }
    out
}

/// Resolve a user-supplied graph reference against the full graph set: an exact
/// id match wins first, then an exact (necessarily unique) name match, then
/// an unambiguous id prefix. Anything else is an actionable error listing the
/// candidates the caller could have meant.
pub(super) fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

pub(super) fn status_icon(status: GraphStatus) -> &'static str {
    match status {
        GraphStatus::Running => "\x1b[36m▶\x1b[0m",
        GraphStatus::Pausing => "\x1b[33m⏸\x1b[0m",
        GraphStatus::Paused => "\x1b[33m⏸\x1b[0m",
        GraphStatus::Completed => "\x1b[32m✓\x1b[0m",
        GraphStatus::Failed => "\x1b[31m✗\x1b[0m",
        GraphStatus::Draft => "\x1b[90m●\x1b[0m",
    }
}

pub(super) fn spec_status_icon(status: GraphSpecStatus) -> &'static str {
    match status {
        GraphSpecStatus::Running => "\x1b[36m▶\x1b[0m",
        GraphSpecStatus::Completed => "\x1b[32m✓\x1b[0m",
        GraphSpecStatus::Failed => "\x1b[31m✗\x1b[0m",
        GraphSpecStatus::Skipped => "\x1b[90m⊘\x1b[0m",
        GraphSpecStatus::Pending => "\x1b[90m●\x1b[0m",
        GraphSpecStatus::Interrupted => "\x1b[33m⚑\x1b[0m",
    }
}

/// B36: a run cut short by a daemon restart/kill is recorded as `fail` (see
/// `reconcile_orphaned_graphs` / `reconcile_stranded_queue_specs`), but it must
/// never render like an ordinary node failure — its own icon, distinct from
/// both a genuine fail and a healthy pass.
pub(super) fn run_status_icon(
    status: GraphRunStatus,
    output: Option<&serde_json::Value>,
) -> &'static str {
    if is_interrupted(output) {
        return "\x1b[33m⚑\x1b[0m";
    }
    match status {
        GraphRunStatus::Running => "\x1b[36m▶\x1b[0m",
        GraphRunStatus::Waiting => "\x1b[36m◷\x1b[0m",
        GraphRunStatus::Pass => "\x1b[32m✓\x1b[0m",
        GraphRunStatus::Fail => "\x1b[31m✗\x1b[0m",
        GraphRunStatus::Interrupted => "\x1b[33m⚑\x1b[0m",
        GraphRunStatus::Error => "\x1b[33m⚠\x1b[0m",
    }
}

pub(super) fn is_interrupted(output: Option<&serde_json::Value>) -> bool {
    output
        .and_then(|o| o.get("interrupted"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub(super) fn format_dt(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// Compact `⏲ HH:MMZ` badge for a scheduled autorun, used in `graph list` rows.
pub(super) fn format_autorun_compact(at: DateTime<Utc>) -> String {
    format!("⏲ {}", at.format("%H:%MZ"))
}

/// Human relative hint for a scheduled autorun, e.g. "in 2h 14m", used
/// alongside the ISO8601 timestamp in `graph info`.
pub(super) fn format_relative_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds();
    if secs <= 0 {
        "due now".to_string()
    } else if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3600 {
        format!("in {}m", secs / 60)
    } else if secs < 86400 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        if mins == 0 {
            format!("in {hours}h")
        } else {
            format!("in {hours}h {mins}m")
        }
    } else {
        format!("in {}d", secs / 86400)
    }
}

pub(super) fn format_elapsed(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Two-space-aligned columns, header underlined — the table shape
/// `canopy graph crew` prints. Cells are padded to their column's widest
/// value, trailing padding trimmed, so both the header and every row read
/// as one grid without pulling in a table crate.
fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut grid: Vec<Vec<String>> = Vec::with_capacity(rows.len() + 1);
    grid.push(headers.iter().map(|h| (*h).to_string()).collect());
    grid.extend(rows.iter().cloned());
    let mut widths = vec![0usize; headers.len()];
    for row in &grid {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }
    let line = |row: &[String]| -> String {
        row.iter()
            .enumerate()
            .map(|(index, cell)| format!("{:<width$}", cell, width = widths[index]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_string()
    };
    let separator: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let mut out = String::new();
    out.push_str(&line(&grid[0]));
    out.push('\n');
    out.push_str(&line(&separator));
    for row in grid.iter().skip(1) {
        out.push('\n');
        out.push_str(&line(row));
    }
    out
}

/// One table cell for a possibly-empty string: empty renders as the
/// same `-` placeholder every absent cell uses.
fn cell(value: &str) -> String {
    if value.is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

/// The Availability column: `available`, `unavailable (reason)`, or `-`
/// when the row has no platform to ask the roster about (CM48).
fn availability_cell(row: &GraphCrewRow) -> String {
    match row.availability.as_deref() {
        Some("unavailable") => {
            let reason = row.availability_reason.as_deref().unwrap_or("unspecified");
            format!("unavailable ({reason})")
        }
        Some(state) => cell(state),
        None => "-".to_string(),
    }
}

/// `canopy graph crew`'s per-graph table: kind, name, pair, timeout, and
/// the CM49 crew / CM48 availability columns when a row carries them.
pub(crate) fn format_graph_crew_table(rows: &[GraphCrewRow]) -> String {
    if rows.is_empty() {
        return "No matching crew assignments found.".to_string();
    }
    let headers = [
        "Kind",
        "Name",
        "Platform",
        "Model",
        "Timeout",
        "Crew",
        "Availability",
    ];
    let body: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            vec![
                row.kind.as_str().to_string(),
                row.name.clone(),
                cell(&row.platform),
                cell(&row.model),
                row.timeout_minutes
                    .map(|minutes| format!("{minutes}m"))
                    .unwrap_or_else(|| "-".to_string()),
                cell(row.crew.as_deref().unwrap_or_default()),
                availability_cell(row),
            ]
        })
        .collect();
    render_table(&headers, &body)
}

/// `canopy graph crew --group-by-pair`'s summary table: one row per
/// distinct pair with its counts and the graphs that use it.
pub(super) fn format_graph_crew_summary_table(summaries: &[GraphCrewSummary]) -> String {
    if summaries.is_empty() {
        return "No matching crew assignments found.".to_string();
    }
    let headers = [
        "Platform",
        "Model",
        "Graphs",
        "Ensembles",
        "Nodes",
        "Hooks",
        "Graphs using it",
    ];
    let body: Vec<Vec<String>> = summaries
        .iter()
        .map(|summary| {
            vec![
                summary.platform.clone(),
                cell(&summary.model),
                summary.graph_count.to_string(),
                summary.ensemble_count.to_string(),
                summary.node_count.to_string(),
                summary.hook_count.to_string(),
                summary.graph_names.join(", "),
            ]
        })
        .collect();
    render_table(&headers, &body)
}
