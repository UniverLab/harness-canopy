use super::*;

impl GraphEngine {
    /// Notify that `graph_id` has become blocked on a node needing human
    /// intervention. Called both by the daemon's `graph_report_blocker` tool
    /// (which owns that state transition itself — pausing the graph,
    /// recording the blocker on the run) and by [`Self::block_graph`] (C19),
    /// so there is one notification path for every way a graph can end up
    /// blocked. Also fires `on_blocked` hooks if any are registered.
    pub fn notify_blocked(&self, graph_id: &str, summary: &str) -> Result<()> {
        let graph_name = self
            .db
            .get_graph(graph_id)?
            .map(|lp| lp.name)
            .unwrap_or_else(|| graph_id.to_string());
        self.notification_service
            .notify_graph_finished(&graph_name, GraphFinishOutcome::Blocked { summary });
        Ok(())
    }

    /// Fire `on_blocked` hooks for `graph_id` — called after the graph has
    /// been transitioned to `Paused` and the blocker recorded. Separated
    /// from [`Self::notify_blocked`] because it needs async I/O. Shares the
    /// post-transition firing path with [`Self::block_graph`]: both call this
    /// exactly once per blocker transition, so a blocker never double-fires.
    pub async fn fire_on_blocked_hooks(
        &self,
        graph_id: &str,
        blocker: &str,
        node_name: Option<&str>,
    ) {
        // Clone the owned values the async ctx borrows from out of the
        // short-lived `get_graph` guard so the ctx can borrow them.
        let owned: Option<(Graph, String, Option<String>)> =
            self.db.get_graph(graph_id).ok().flatten().map(|lp| {
                let blocker_owned = blocker.to_string();
                let node_owned = node_name
                    .map(str::to_string)
                    .or_else(|| self.ending_node_name(graph_id, None));
                (lp, blocker_owned, node_owned)
            });
        if let Some((lp, blocker_owned, node_owned)) = owned.as_ref() {
            let ctx = HookContext {
                graph_name: &lp.name,
                workdir: &lp.workdir,
                completed_specs: &[],
                spec_name: None,
                spec_id: None,
                blocker: Some(blocker_owned.as_str()),
                node_name: node_owned.as_deref(),
            };
            self.fire_hooks(lp, GraphHookEvent::OnBlocked, &ctx).await;
        }
    }

    /// Human-readable name of the node that ended the run for `graph_id`:
    /// the first still-`running` row's node name, falling back to
    /// `fallback` (usually the spec name the dispatcher was working) and
    /// finally to the raw node id when the node row is gone. Returns `None`
    /// only when there is no running run and no fallback — callers firing
    /// `on_failed`/`on_blocked` should always have one of the two.
    pub(crate) fn ending_node_name(
        &self,
        graph_id: &str,
        fallback: Option<&str>,
    ) -> Option<String> {
        if let Ok(runs) = self.db.list_running_graph_runs(graph_id) {
            if let Some(run) = runs.into_iter().next() {
                if let Ok(Some(node)) = self.db.get_graph_node(&run.node_id) {
                    return Some(node.name);
                }
                return Some(run.node_id);
            }
        }
        fallback.map(str::to_string)
    }

    /// Fire `on_spec_completed` hooks for a just-completed spec.
    pub async fn fire_on_spec_completed_hooks(&self, lp: &Graph, spec: &GraphSpec) {
        let ctx = HookContext {
            graph_name: &lp.name,
            workdir: &lp.workdir,
            completed_specs: &[],
            spec_name: Some(&spec.name),
            spec_id: Some(&spec.id),
            blocker: None,
            node_name: None,
        };
        self.fire_hooks(lp, GraphHookEvent::OnSpecCompleted, &ctx)
            .await;
    }
    /// `(done, total)` specs for `graph_id`'s current run — the graph's bound
    /// specs, or `queue_id`'s members when this run is drawing from a queue.
    /// `done` counts specs already `completed`; skipped/pending/running/failed
    /// specs count toward `total` but not `done`.
    pub(crate) fn spec_progress(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<(usize, usize)> {
        match queue_id {
            Some(queue_id) => {
                let ids = self.db.list_queue_member_spec_ids(queue_id)?;
                let done = self.count_completed_queue_specs(&ids)?;
                Ok((done, ids.len()))
            }
            None => {
                let specs = self.db.list_graph_specs(graph_id)?;
                let done = specs
                    .iter()
                    .filter(|spec| spec.status == GraphSpecStatus::Completed)
                    .count();
                Ok((done, specs.len()))
            }
        }
    }

    pub(crate) fn count_completed_queue_specs(&self, ids: &[String]) -> Result<usize> {
        let mut done = 0;
        for id in ids {
            if let Some(spec) = self.db.get_graph_spec(id)? {
                if spec.status == GraphSpecStatus::Completed {
                    done += 1;
                }
            }
        }
        Ok(done)
    }

    /// Fire the spec-completed notification for `spec`, which the caller has
    /// already marked `completed` in the database. Carries the next spec this
    /// run will pick up (if any) so the toast leads with progress *and* what's
    /// coming next.
    pub(crate) fn notify_spec_completed(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        queue_id: Option<&str>,
    ) -> Result<()> {
        let (done, total) = self.spec_progress(&lp.id, queue_id)?;
        let next_pending = self.first_pending_spec_name(&lp.id, queue_id)?;
        self.notification_service.notify_spec_completed(
            &lp.name,
            &spec.name,
            done,
            total,
            next_pending.as_deref(),
        );
        Ok(())
    }

    /// Name of the next spec this run will work: the queue's next pending
    /// member for a queue run, else the graph's first
    /// `running`-or-`pending`-or-`interrupted` bound spec in position order.
    /// `None` when nothing is left to do.
    pub(crate) fn first_pending_spec_name(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<Option<String>> {
        match queue_id {
            Some(queue_id) => {
                let Some(spec_id) = self.db.queue_next_pending_spec_id(queue_id)? else {
                    return Ok(None);
                };
                Ok(self.db.get_graph_spec(&spec_id)?.map(|spec| spec.name))
            }
            None => {
                let specs = self.db.list_graph_specs(graph_id)?;
                let next = specs
                    .iter()
                    .find(|spec| spec.status == GraphSpecStatus::Running)
                    .or_else(|| {
                        specs.iter().find(|spec| {
                            matches!(
                                spec.status,
                                GraphSpecStatus::Pending | GraphSpecStatus::Interrupted
                            )
                        })
                    });
                Ok(next.map(|spec| spec.name.clone()))
            }
        }
    }
}
