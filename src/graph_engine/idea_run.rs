//! Idea-driven (spec-less, mode "idea") runs: the idea such a graph last ran
//! with, and the resume contract that lets `graph_continue run_from_node` /
//! `retry_current_node` pick that run back up instead of dying on the
//! spec-less launch guard (or refusing with nothing but a bare "no specs").

use super::*;

impl GraphEngine {
    /// The idea `graph_id`'s run last executed with — read straight from the
    /// row (no `Graph` field for it, cf. `is_graph_hook_launched`). Graphs
    /// that ran before the run record carried the idea keep it only on their
    /// blank bookkeeping spec's `description` (what `{{spec_content}}`
    /// rendered), so a NULL column falls back to that — otherwise every idea
    /// graph that failed before the column shipped (the motivating
    /// canopy-test-file-cap case) would be refused as "never run" instead of
    /// being resumable.
    pub(crate) fn stored_idea(&self, graph_id: &str) -> Result<Option<String>> {
        if let Some(idea) = self.db.graph_active_run_idea(graph_id)? {
            return Ok(Some(idea));
        }
        Ok(self
            .db
            .list_graph_specs(graph_id)?
            .into_iter()
            .filter(is_no_spec_placeholder)
            .find_map(|spec| spec.description.filter(|text| !text.trim().is_empty())))
    }

    /// Whether `lp` is an idea-driven graph: no queue in play, every bound
    /// spec a blank-name bookkeeping row (or none at all), and at least one
    /// top-level graph node for the idea to drive (the same "a top-level
    /// graph alone is not a spec" shape `empty_launch_check` accepts an idea
    /// against).
    pub(crate) fn is_idea_graph(&self, lp: &Graph) -> Result<bool> {
        if lp.active_run_queue_id.is_some() {
            return Ok(false);
        }
        let specs = self.db.list_graph_specs(&lp.id)?;
        if !specs.iter().all(|spec| spec.name.trim().is_empty()) {
            return Ok(false);
        }
        Ok(!self.db.list_graph_nodes_for_graph(&lp.id)?.is_empty())
    }

    /// The idea this dispatch runs with: an explicit one always wins;
    /// otherwise a resume/directed dispatch may fall back to the idea the
    /// graph last ran with — but only when `lp` really is idea-driven, so a
    /// spec-driven graph's stale stored idea never hijacks its run (FR5).
    pub(crate) fn effective_idea(
        &self,
        lp: &Graph,
        explicit: Option<String>,
        allow_stored: bool,
    ) -> Result<Option<String>> {
        if explicit.is_some() {
            return Ok(explicit);
        }
        if allow_stored && self.is_idea_graph(lp)? {
            return self.stored_idea(&lp.id);
        }
        Ok(None)
    }

    /// Persist the idea this run executes — `Some` only: a run without an
    /// idea never clears the one a previous idea run left behind (FR1).
    pub(crate) fn persist_run_idea(&self, graph_id: &str, idea: Option<&str>) -> Result<()> {
        if let Some(idea) = idea {
            self.db.set_graph_active_run_idea(graph_id, Some(idea))?;
        }
        Ok(())
    }

    /// Pre-dispatch gate for a directed resume (`graph_continue
    /// run_from_node`, `retry_current_node`): `Ok(None)` means the call may
    /// proceed — the graph is not idea-driven, or it is and a stored idea
    /// exists (recreating the blank-name placeholder row from that idea when
    /// the specs are gone, so the directed run has a spec to target and
    /// `{{spec_content}}` renders the idea). The `Ok(Some(message))` refusal
    /// is FR3: an idea graph that has never run has nothing to resume, and
    /// the operator is named the way to start one.
    pub(crate) fn idea_resume_ready(&self, lp: &Graph) -> Result<Option<String>> {
        if !self.is_idea_graph(lp)? {
            return Ok(None);
        }
        let Some(idea) = self.stored_idea(&lp.id)? else {
            return Ok(Some(format!(
                "Graph '{}' is idea-driven and has no stored idea to resume: it has never run. \
                 Start it with graph_run passing an `idea`.",
                lp.name
            )));
        };
        if self.db.list_graph_specs(&lp.id)?.is_empty() {
            let mut placeholder = super::dirty_commit::no_spec_placeholder(&lp.id);
            placeholder.description = Some(idea);
            self.db.insert_graph_spec(&placeholder)?;
        }
        Ok(None)
    }
}
