//! The graph identity a node-run spawn exports to its child process.
//!
//! Split out of `agent_spawn` (file-size rule): building the run's identity
//! from the run row plus the executed node, and exporting it as
//! `CANOPY_GRAPH_ID`/`CANOPY_NODE_ID`/`CANOPY_NODE_RUN_ID` so the agent's
//! verdict never depends on reproducing UUIDs from memory.

use super::*;
use crate::shared::sync_identity::{
    GraphIdentity, CANOPY_GRAPH_ID_ENV, CANOPY_NODE_ID_ENV, CANOPY_NODE_RUN_ID_ENV,
};

/// The identity a node-run spawn exports to its child: the graph id from the
/// run row, the node id from the node being executed. `None` when the run row
/// is gone (never in production) — the child then exports nothing.
pub(crate) fn graph_identity_for_run(
    db: &Database,
    node: &GraphNode,
    run_id: &str,
) -> Result<Option<GraphIdentity>> {
    Ok(db.get_graph_run(run_id)?.map(|run| GraphIdentity {
        graph_id: run.graph_id,
        node_id: node.id.clone(),
        run_id: run_id.to_string(),
    }))
}

/// Export a spawn's graph identity to the child process: `Some` sets all
/// three `CANOPY_*` vars, `None` removes them so a hook (or a test) never
/// inherits a stale run identity from the daemon's own environment.
pub(crate) fn apply_graph_identity_env(command: &mut Command, identity: Option<&GraphIdentity>) {
    match identity {
        Some(identity) => {
            command.env(CANOPY_GRAPH_ID_ENV, &identity.graph_id);
            command.env(CANOPY_NODE_ID_ENV, &identity.node_id);
            command.env(CANOPY_NODE_RUN_ID_ENV, &identity.run_id);
        }
        None => {
            command.env_remove(CANOPY_GRAPH_ID_ENV);
            command.env_remove(CANOPY_NODE_ID_ENV);
            command.env_remove(CANOPY_NODE_RUN_ID_ENV);
        }
    }
}
