use crate::db::Database;
use crate::domain::graphs::{Graph, GraphSpec, GraphSpecStatus};

/// What a graph is actually working on (CB30).
///
/// `list_graph_specs` returns the graph's *bound* specs, which for a queue or
/// idea run is the blank-name placeholder row the engine inserts as
/// `graph_runs.spec_id` bookkeeping — never the work itself. Every display
/// surface must go through [`active_graph_spec_context`] instead of reading
/// the bound list directly, so `graph`, `graph list` and `canopy graph
/// info` agree on the answer.
#[derive(Debug)]
pub(crate) enum ActiveGraphSpec {
    /// A normal bound spec (never a placeholder).
    Bound(GraphSpec),
    /// A queue member spec currently in flight.
    QueueMember {
        spec: GraphSpec,
        queue_id: String,
        queue_name: String,
    },
    /// An idea-driven run: a placeholder exists but must not be shown.
    Idea { placeholder_id: String },
    /// Nothing running (all done, or nothing queued yet).
    None,
}

/// The single answer to "what spec is this graph on", shared by `graph`
/// ([`super::graph_details_json`]), `graph list` ([`super::build_graph_summary_json`]) and
/// `canopy graph info` (`handle_graph_info` in `daemon/graph_cli.rs`).
///
/// The placeholder is hidden, never relabelled: a fabricated name would
/// replace a confusing answer with a false one.
pub(crate) fn active_graph_spec_context(
    db: &Database,
    lp: &Graph,
) -> anyhow::Result<ActiveGraphSpec> {
    if let Some(queue_id) = lp.active_run_queue_id.as_deref() {
        if let Some(queue_name) = db.get_queue(queue_id)?.map(|queue| queue.name) {
            if let Some(active) = queue_active_graph_spec(db, queue_id, &queue_name)? {
                return Ok(active);
            }
            // Queue drained (or empty): fall through to the bound/idea
            // logic so a finished queue graph reports `None` rather than a
            // stale member. Callers still render the queue link itself from
            // `active_run_queue_id`.
        }
    }
    bound_active_graph_spec(db, &lp.id, lp.active_run_queue_id.is_some())
}

/// The spec dispatch is on inside `queue_id`, or `None` when the queue holds
/// nothing runnable: a `Running` member anywhere beats an earlier pending one
/// (dispatch is sequential, so a running member means everything before it
/// already completed); with nothing running, the first runnable — or stalled —
/// member in queue order is the one to report.
pub(crate) fn queue_active_graph_spec(
    db: &Database,
    queue_id: &str,
    queue_name: &str,
) -> anyhow::Result<Option<ActiveGraphSpec>> {
    let mut running: Option<GraphSpec> = None;
    let mut pending: Option<GraphSpec> = None;
    for spec_id in db.list_queue_member_spec_ids(queue_id)? {
        let Some(spec) = db.get_graph_spec(&spec_id)? else {
            continue;
        };
        if spec.status == GraphSpecStatus::Running {
            if running.is_none() {
                running = Some(spec);
            }
        // Same fallback shape as `build_graph_summary_json`'s
        // bound-spec search: the next runnable (or stalled)
        // member in queue order when nothing is `Running`.
        } else if pending.is_none()
            && matches!(
                spec.status,
                GraphSpecStatus::Pending | GraphSpecStatus::Interrupted | GraphSpecStatus::Failed
            )
        {
            pending = Some(spec);
        }
    }
    let Some(spec) = running.or(pending) else {
        return Ok(None);
    };
    Ok(Some(ActiveGraphSpec::QueueMember {
        spec,
        queue_id: queue_id.to_string(),
        queue_name: queue_name.to_string(),
    }))
}

/// The bound spec dispatch is on — the first non-terminal real spec in
/// position order, the same pick `build_graph_summary_json` historically
/// made — or the idea placeholder when that's all the graph has. The
/// placeholder is hidden, never relabelled: a fabricated name would
/// replace a confusing answer with a false one.
pub(crate) fn bound_active_graph_spec(
    db: &Database,
    graph_id: &str,
    has_active_queue: bool,
) -> anyhow::Result<ActiveGraphSpec> {
    let mut placeholder_id: Option<String> = None;
    let mut current: Option<GraphSpec> = None;
    for spec in db.list_graph_specs(graph_id)? {
        if crate::graph_engine::is_no_spec_placeholder(&spec) {
            if placeholder_id.is_none() {
                placeholder_id = Some(spec.id.clone());
            }
            continue;
        }
        if current.is_none()
            && matches!(
                spec.status,
                GraphSpecStatus::Running
                    | GraphSpecStatus::Pending
                    | GraphSpecStatus::Interrupted
                    | GraphSpecStatus::Failed
            )
        {
            current = Some(spec);
        }
    }
    if let Some(spec) = current {
        return Ok(ActiveGraphSpec::Bound(spec));
    }
    if let Some(placeholder_id) = placeholder_id {
        // CB30: a completed queue-driven graph still has `active_run_queue_id`
        // set (B31 keeps it as last-run context) and only the placeholder in
        // its bound specs. Without this guard, we'd report `Idea` for a
        // finished queue — replacing one false answer with another.
        if has_active_queue {
            return Ok(ActiveGraphSpec::None);
        }
        return Ok(ActiveGraphSpec::Idea { placeholder_id });
    }
    Ok(ActiveGraphSpec::None)
}
