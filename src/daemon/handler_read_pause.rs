//! Pending-pause read-surface helpers, shared by `graph get` and `graph list`.
//! Split out of `handler_read` to keep every production file within the
//! file-size gate.

use crate::db::Database;
use crate::domain::graphs::{Graph, GraphStatus};

/// MCP-visible graph status: a pending pause still reads as `running` — the
/// stored `pausing` is exposed separately as `pause_pending`, so no new
/// status value leaks onto the read surfaces.
pub(crate) fn visible_graph_status(status: GraphStatus) -> &'static str {
    if status == GraphStatus::Pausing {
        GraphStatus::Running.as_str()
    } else {
        status.as_str()
    }
}

/// Set the always-present pending-pause JSON pair on a graph `get`/`list`
/// object: `pause_pending`, plus `pause_waiting_for` — the node names the
/// pause is waiting for, empty when no pause is pending.
pub(crate) fn set_pause_pending_json(
    db: &Database,
    out: &mut serde_json::Value,
    lp: &Graph,
) -> anyhow::Result<()> {
    let pending = lp.status == GraphStatus::Pausing;
    let waiting_for = if pending {
        db.pause_waiting_node_names(&lp.id)?
    } else {
        Vec::new()
    };
    out["pause_pending"] = serde_json::json!(pending);
    out["pause_waiting_for"] = serde_json::json!(waiting_for);
    Ok(())
}
