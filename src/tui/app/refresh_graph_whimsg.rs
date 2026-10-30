//! Graph-event detection for the header, kept in its own file so `refresh.rs`
//! stays under the 800-code-line cap.

use super::*;

impl App {
    /// Turn the previous per-graph snapshot into header events by diffing it
    /// against the graph state this refresh just read. The first call only
    /// primes the snapshot (so a graph already running isn't re-announced).
    pub(crate) fn notify_whimsg_for_graphs(&mut self) {
        use crate::tui::whimsg::events::{diff_graphs, GraphSnapshot};

        let curr: HashMap<String, GraphSnapshot> = self
            .graphs
            .iter()
            .map(|lp| {
                let blocked = self
                    .graph_sidebar_meta
                    .get(&lp.id)
                    .is_some_and(|meta| meta.blocked);
                let node = if blocked || lp.status == GraphStatus::Failed {
                    self.whimsg_graph_node(&lp.id)
                } else {
                    None
                };
                let done_specs = if matches!(lp.status, GraphStatus::Running | GraphStatus::Pausing)
                {
                    graph_live_state::completed_spec_labels(&self.db, lp)
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
        let runs = self.db.list_graph_runs_for_graph(graph_id).ok()?;
        runs.last().map(|run| run.node_id.clone())
    }
}
