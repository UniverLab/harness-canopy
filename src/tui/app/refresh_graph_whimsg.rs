//! Graph-event detection for the header, kept in its own file so `refresh.rs`
//! stays under the 800-code-line cap.

use super::*;

impl App {
    /// Turn the previous per-graph snapshot into header events by diffing it
    /// against the graph state this refresh just read. The first call only
    /// primes the snapshot (so a graph already running isn't re-announced).
    pub(crate) fn notify_whimsg_for_graphs(&mut self) {
        use crate::tui::whimsg::events::{diff_graphs, GraphSnapshot};

        let worker_mode = self.snapshot_worker.is_some();
        let curr: HashMap<String, GraphSnapshot> = self
            .graphs
            .iter()
            .map(|lp| {
                let blocked = self
                    .graph_sidebar_meta
                    .get(&lp.id)
                    .is_some_and(|meta| meta.blocked);
                // Worker mode reads the header inputs the snapshot built
                // off-thread (same per-status conditions as the sync path);
                // without a worker the queries run inline as before.
                let cached = worker_mode
                    .then(|| self.graph_whim_cache.get(&lp.id))
                    .flatten();
                let node = if blocked || lp.status == GraphStatus::Failed {
                    match cached {
                        Some(info) => info.node.clone(),
                        None if worker_mode => None,
                        None => self.whimsg_graph_node(&lp.id),
                    }
                } else {
                    None
                };
                let done_specs = if matches!(lp.status, GraphStatus::Running | GraphStatus::Pausing)
                {
                    match cached {
                        Some(info) => info.done_specs.clone(),
                        None if worker_mode => Vec::new(),
                        None => graph_live_state::completed_spec_labels(&self.db, lp),
                    }
                } else {
                    Vec::new()
                };
                (
                    lp.id.clone(),
                    GraphSnapshot {
                        name: lp.name.clone(),
                        status: lp.status,
                        blocked,
                        node,
                        done_specs,
                    },
                )
            })
            .collect();

        if let Some(prev) = self.whimsg_graphs.as_ref() {
            for ev in diff_graphs(prev, &curr) {
                self.whimsg.notify_event(ev);
            }
        }
        self.whimsg_graphs = Some(curr);
    }

    /// The node a graph last ran, for a blocked/failed `{node}` template.
    fn whimsg_graph_node(&self, graph_id: &str) -> Option<String> {
        self.db
            .last_graph_run_summary(graph_id)
            .ok()
            .flatten()
            .map(|summary| summary.node_id)
    }
}
