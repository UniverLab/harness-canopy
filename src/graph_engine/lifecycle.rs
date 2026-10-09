use super::*;

impl GraphEngine {
    pub fn start_background(self: Arc<Self>, graph_id: String) {
        Arc::clone(&self).start_background_run(graph_id, None, None, None, None, None);
    }

    /// Same as [`Self::start_background`], but optionally drives the graph's
    /// pending queue specs (see [`Self::run_graph`]) and/or overrides the
    /// workdir for this run only.
    ///
    /// This is a fresh dispatch, not a resume — it backs `graph_run`, the tool
    /// a human/scheduler calls to launch or *relaunch* a graph (including
    /// directly relaunching a `paused` graph instead of going through
    /// `graph_continue`). Every spec it reaches is treated as newly entered
    /// for `{{spec_start_head}}` purposes (B10): even a spec left `running`
    /// from a stale, never-reset prior attempt gets a fresh baseline here,
    /// rather than silently inheriting one captured under a previous
    /// run/launch. See [`Self::resume_background`] for the one path that is
    /// allowed to reuse a persisted baseline.
    pub fn start_background_run(
        self: Arc<Self>,
        graph_id: String,
        queue_id: Option<String>,
        workdir_override: Option<String>,
        idea: Option<String>,
        sandbox: Option<Sandbox>,
        directed: Option<DirectedStart>,
    ) {
        tokio::spawn(async move {
            if let Err(error) = self
                .run_graph(
                    graph_id.clone(),
                    queue_id,
                    workdir_override,
                    idea,
                    sandbox,
                    directed,
                )
                .await
            {
                if error.downcast_ref::<EmptySpecSetError>().is_some() {
                    // (B17) The graph never launched — its status is already
                    // untouched, and it must stay that way, so don't
                    // `fail_graph` it.
                    tracing::error!("Graph '{}' launch refused: {error:#}", graph_id);
                } else {
                    tracing::error!("Graph '{}' failed to run: {error:#}", graph_id);
                    let _ = self
                        .fail_graph(&graph_id, None, None, None, &error.to_string())
                        .await;
                }
            }
        });
    }

    /// Request a pause for a running graph.
    ///
    /// If `interrupt` is false (default), sets the graph to `Pausing` state and
    /// waits for the running node to complete naturally. The engine checks this
    /// state between node executions and transitions to `Paused` after the
    /// current node finishes.
    ///
    /// If `interrupt` is true, immediately terminates the running node and
    /// marks it as `Interrupted` (not `Fail`). Use this when the operator
    /// explicitly wants to stop the current node.
    ///
    /// Returns true if the pause was accepted (graph was running or pausing),
    /// false if the graph is not running or does not exist.
    pub fn request_pause(&self, graph_id: &str, interrupt: bool) -> Result<bool> {
        let Some(lp) = self.db.get_graph(graph_id)? else {
            return Ok(false);
        };

        match lp.status {
            GraphStatus::Running => {
                if interrupt {
                    // Immediate termination mode: mark paused and interrupt running nodes
                    let result =
                        self.db
                            .update_graph_status(graph_id, GraphStatus::Paused, None, None);
                    if matches!(result, Ok(true)) {
                        activity::publish(&self.db, &lp.workdir, graph_id, &lp.name, "Paused.");
                    }
                    for run in self
                        .db
                        .list_running_graph_runs(graph_id)
                        .unwrap_or_default()
                    {
                        self.interrupt_run(&run, "operator requested interrupt");
                    }
                    result
                } else if self
                    .db
                    .list_running_graph_runs(graph_id)
                    .unwrap_or_default()
                    .is_empty()
                {
                    // Req 6: nothing is in flight, so there is nothing to wait
                    // for — pause immediately, exactly as before this change.
                    let result =
                        self.db
                            .update_graph_status(graph_id, GraphStatus::Paused, None, None);
                    if matches!(result, Ok(true)) {
                        activity::publish(&self.db, &lp.workdir, graph_id, &lp.name, "Paused.");
                    }
                    result
                } else {
                    // Wait-for-completion mode: a node is running. Record the
                    // pending pause; the engine transitions the graph to
                    // `Paused` once that node finishes on its own.
                    self.db.request_pause_pending(graph_id)
                }
            }
            GraphStatus::Pausing => {
                if interrupt {
                    // Already pausing, but interrupt requested: terminate now
                    for run in self
                        .db
                        .list_running_graph_runs(graph_id)
                        .unwrap_or_default()
                    {
                        self.interrupt_run(&run, "operator requested interrupt");
                    }
                    let result =
                        self.db
                            .update_graph_status(graph_id, GraphStatus::Paused, None, None);
                    if matches!(result, Ok(true)) {
                        activity::publish(&self.db, &lp.workdir, graph_id, &lp.name, "Paused.");
                    }
                    result
                } else {
                    Ok(true) // Already pausing, nothing to do
                }
            }
            GraphStatus::Paused => Ok(true), // Already paused
            _ => Ok(false),                  // Not running
        }
    }

    /// Interrupt a running node, marking it as `Interrupted` (not `Fail`).
    pub(crate) fn interrupt_run(&self, run: &GraphNodeRun, reason: &str) {
        tracing::info!(
            run_id = %run.id,
            node_id = %run.node_id,
            reason,
            "node run interrupted by operator"
        );
        // Kill the process
        if let Some(pid) = run.pid {
            crate::daemon::process::terminate_process_group_async(pid, KILL_GRACE);
        }
        // Mark as interrupted (not fail)
        let _ = self.db.interrupt_graph_run(&run.id, reason);
    }

    /// Run `graph_id`'s specs through its graph (R2).
    ///
    /// With `queue_id`: runs the queue's pending members, in the queue's queue
    /// order, instead of the graph's own bound specs. Queue membership never
    /// mutates the specs themselves — they stay standalone (`graph_id: None`)
    /// so the same queue can be run by different graphs over time.
    ///
    /// A queue run is *live* (R6): the "next pending" spec is re-queried from
    /// the queue at every spec boundary via
    /// [`Database::queue_next_pending_spec_id`], never off a list captured at
    /// launch. That's what lets `queue_add_spec`/`queue_reorder` calls made
    /// while the run is in flight actually change what runs next — the run
    /// ends only when a pick finds no pending member left. A bound run (no
    /// `queue_id`) keeps the pre-queue behavior below: its spec list is fixed
    /// at launch.
    ///
    /// `workdir_override`, when set, wins over `graph.workdir` for this run
    /// only — the graph's own `workdir` is left untouched.
    ///
    /// Without `queue_id`: identical to the pre-queue behavior (bound specs,
    /// `graph.workdir`).
    ///
    /// Equivalent to a fresh (non-resumed) dispatch — see
    /// [`Self::run_graph_dispatch`] for the `is_resume` distinction that
    /// matters for `{{spec_start_head}}` (B10).
    pub async fn run_graph(
        &self,
        graph_id: String,
        queue_id: Option<String>,
        workdir_override: Option<String>,
        idea: Option<String>,
        sandbox: Option<Sandbox>,
        directed: Option<DirectedStart>,
    ) -> Result<()> {
        let mode = match directed {
            Some(d) => DispatchMode::fresh_from(d),
            None => DispatchMode::fresh(),
        };
        let result = self
            .run_graph_dispatch(
                graph_id.clone(),
                queue_id,
                workdir_override,
                mode,
                idea,
                sandbox,
            )
            .await;
        // CH4: clear the hook-launched flag on ALL exit paths (success, failure,
        // pause, block, etc.). This must happen here, not in run_graph_dispatch,
        // because that function has multiple early returns.
        let _ = self.db.clear_graph_hook_launched(&graph_id);
        result
    }
    pub fn resume_background(self: Arc<Self>, graph_id: String, directed: Option<DirectedStart>) {
        let queue_id = self
            .db
            .get_graph(&graph_id)
            .ok()
            .flatten()
            .and_then(|lp| lp.active_run_queue_id);
        // A sandboxed graph that was paused (daemon restart, `graph_pause`) must
        // resume inside its worktree and still merge back on completion —
        // otherwise the remaining nodes run against the user's real repo and
        // the sandbox is stranded.
        let sandbox = self
            .db
            .get_active_sandbox_for_owner("graph", &graph_id)
            .ok()
            .flatten();
        let mode = match directed {
            Some(d) => DispatchMode::resume_from(d),
            None => DispatchMode::resume(),
        };
        tokio::spawn(async move {
            let result = self
                .run_graph_dispatch(graph_id.clone(), queue_id, None, mode, None, sandbox)
                .await;
            // CH4: clear the hook-launched flag on ALL exit paths.
            let _ = self.db.clear_graph_hook_launched(&graph_id);
            if let Err(error) = result {
                if error.downcast_ref::<EmptySpecSetError>().is_some() {
                    tracing::error!("Graph '{}' launch refused: {error:#}", graph_id);
                } else {
                    tracing::error!("Graph '{}' failed to run: {error:#}", graph_id);
                    let _ = self
                        .fail_graph(&graph_id, None, None, None, &error.to_string())
                        .await;
                }
            }
        });
    }
}
