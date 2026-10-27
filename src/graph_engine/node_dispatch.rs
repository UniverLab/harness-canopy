use super::*;

impl GraphEngine {
    #[expect(clippy::too_many_arguments)]
    pub(crate) async fn execute_node(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        node: &GraphNode,
        previous_output: Option<&Value>,
        spec_start_head: Option<&str>,
        spec_committed_head: Option<&str>,
        run_id: &str,
        workdir: &str,
        resume_session_id: Option<&str>,
        resume_crosses_spec: bool,
        node_outputs: &HashMap<String, Value>,
        all_node_names: &[String],
    ) -> Result<NodeExecution> {
        match node.kind {
            GraphNodeKind::Check => {
                execute_check_node(
                    &self.db,
                    run_id,
                    lp,
                    spec,
                    node,
                    spec_start_head,
                    spec_committed_head,
                    workdir,
                )
                .await
            }
            GraphNodeKind::Gate => execute_gate_node(node, previous_output),
            GraphNodeKind::Agent => {
                execute_agent_node(
                    &self.db,
                    lp,
                    spec,
                    node,
                    previous_output,
                    run_id,
                    workdir,
                    resume_session_id,
                    resume_crosses_spec,
                    self.dynamic_skills.as_ref(),
                    node_outputs,
                    all_node_names,
                )
                .await
            }
            // A quorum node (F1) never reaches the single-node path: `run_spec`
            // detects the fan-out into its ensemble before this would ever be
            // called and runs `execute_ensemble` instead. This arm exists
            // only so the match stays exhaustive against future callers.
            GraphNodeKind::Join => bail!(
                "Quorum node '{}' cannot execute directly; it only runs as part of ensemble fan-out.",
                node.name
            ),
            GraphNodeKind::Router => {
                execute_router_node(&self.db, lp, spec, node, previous_output, run_id, workdir)
                    .await
            }
        }
    }

    /// Best-effort termination (B12) of `run`'s OS process, if it still has
    /// one recorded, and finalization of its DB row as `Fail` so it stops
    /// showing up as `running`. Every abnormal end that abandons a node run
    /// without letting it finish on its own — a stale row from a crashed
    /// prior attempt, `graph_pause`, `graph_reset` of a running spec, or this
    /// run failing elsewhere — goes through here. A no-op beyond the
    /// status/summary update if `run` never got a pid recorded (e.g. a gate
    /// node, or an agent/check node that hadn't finished spawning yet).
    ///
    /// CB84: the workdir is resolved from the run's graph here rather than
    /// threaded through every caller — the session-end command needs it as
    /// cwd, and the title lookup is scoped to it. A run with no graph (or a
    /// graph whose workdir is blank) just means the session end is skipped,
    /// same as a platform with no `session_end_args`.
    pub(crate) fn terminate_run(&self, run: &GraphNodeRun, reason: &str) {
        let workdir = workdir_for_run(&self.db, run);
        terminate_run_row(&self.db, run, reason, &workdir);
    }

    pub(crate) fn is_paused(&self, graph_id: &str) -> Result<bool> {
        Ok(self
            .db
            .get_graph(graph_id)?
            .is_some_and(|lp| lp.status == GraphStatus::Paused))
    }

    pub(crate) fn is_pausing(&self, graph_id: &str) -> Result<bool> {
        Ok(self
            .db
            .get_graph(graph_id)?
            .is_some_and(|lp| lp.status == GraphStatus::Pausing))
    }

    /// Fail `graph_id`, sweeping every run still `running` under it — but
    /// only when `dispatch_started_at` (this call's claimed generation, from
    /// `run_graph_dispatch`'s own atomic claim) still matches the graph's
    /// current `started_at`. A mismatch means a newer dispatch has since
    /// claimed the graph (a reset + relaunch raced this one), so this call is
    /// itself the stale one: it must not flip status out from under the
    /// fresher dispatch, and — critically — must not sweep `list_running_graph_runs`,
    /// which would otherwise terminate that fresher dispatch's entirely
    /// healthy runs (the 2026-08-05 incident this guards against). `None`
    /// skips the check (the two catch-all call sites in
    /// `start_background_run`/`resume_background` have no captured
    /// generation to compare, since the error they're reacting to already
    /// unwound out of `run_graph_dispatch`'s scope) — decision-4's broadened
    /// `run_was_terminated_out_of_band` check is what keeps a stale run's
    /// completion from reaching either of those paths in the first place.
    /// CB42: tear down a sandboxed run's worktree/branch after its final
    /// status is written. Only the completed path uses it — a failed graph
    /// keeps its sandbox for the operator (see `fail_graph`). Takes
    /// `Option`; `None` is a no-op. A teardown failure never changes the
    /// graph's status — it is recorded on the sandbox row by
    /// `teardown_sandbox_at_end`, never silent.
    pub(crate) async fn teardown_sandbox_after_final_status(
        &self,
        sandbox: Option<Sandbox>,
        reason: String,
    ) {
        let Some(sb) = sandbox else { return };
        let row = match self.db.get_sandbox_run(&sb.id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                tracing::warn!(
                    "Sandbox '{}' has no sandbox_runs row; leaving its worktree in place",
                    sb.id
                );
                return;
            }
            Err(e) => {
                tracing::warn!("Could not load sandbox run '{}': {e:#}", sb.id);
                return;
            }
        };
        crate::domain::sandbox::teardown_sandbox_at_end(&self.db, &row, &reason).await;
    }

    pub(crate) async fn fail_graph(
        &self,
        graph_id: &str,
        dispatch_started_at: Option<chrono::DateTime<chrono::Utc>>,
        spec_name: Option<&str>,
        spec_id: Option<&str>,
        summary: &str,
        completed_specs: &[(String, String)],
    ) -> Result<()> {
        if let Some(expected) = dispatch_started_at {
            if self.dispatch_is_stale(graph_id, expected)? {
                return Ok(());
            }
        }

        self.db.update_graph_status(
            graph_id,
            GraphStatus::Failed,
            None,
            Some(chrono::Utc::now()),
        )?;
        if let Ok(Some(lp)) = self.db.get_graph(graph_id) {
            self.record_failed_activity(graph_id, &lp, spec_id, spec_name, summary);
        }
        // Resolve the human-readable ending node name BEFORE sweeping the
        // running runs below — afterwards there is nothing left to resolve.
        let ending_node = self.ending_node_name(graph_id, spec_name);
        // B12 catch-all: whatever hard-error path got us here (a node
        // timeout already kills its own process before bubbling up, but a
        // DB error or any other error class reaching this point wouldn't
        // have), make sure nothing is left running under this now-failed
        // graph. Safe to sweep every run still `running` under `graph_id`
        // unscoped: the generation check above already established that no
        // newer dispatch has claimed the graph since this one did, so
        // anything still `running` here can only belong to this dispatch.
        for run in self
            .db
            .list_running_graph_runs(graph_id)
            .unwrap_or_default()
        {
            self.terminate_run(&run, "graph run failed");
        }

        // Fire `on_failed` hooks if any are registered.
        if let Ok(Some(lp)) = self.db.get_graph(graph_id) {
            self.fire_on_failed_hooks(&lp, spec_id, ending_node.clone(), summary, completed_specs)
                .await;
        }

        let graph_name = self
            .db
            .get_graph(graph_id)?
            .map(|lp| lp.name)
            .unwrap_or_else(|| graph_id.to_string());
        self.notification_service.notify_graph_finished(
            &graph_name,
            GraphFinishOutcome::Failed {
                spec_name: spec_name.unwrap_or(summary),
            },
        );
        // A failed graph never tears down its sandbox automatically,
        // whatever its contents: the operator lands, continues or discards
        // it explicitly. Mark the row `kept` (the worktree and branch stay
        // in place) so a later `graph_continue` resumes inside it.
        if let Ok(Some(sb)) = self.db.get_active_sandbox_for_owner("graph", graph_id) {
            let _ = self.db.update_sandbox_run_status(&sb.id, "kept");
            let _ = self.db.set_sandbox_cleanup_error(
                &sb.id,
                &format!(
                    "{}: failed: {summary}: kept for the operator",
                    sb.worktree_path.display()
                ),
            );
        }
        Ok(())
    }

    /// Whether the dispatch that is failing `graph_id` has been superseded: a
    /// newer dispatch has claimed the graph since `expected`, so this failure
    /// is stale and must be ignored (traced when so).
    fn dispatch_is_stale(
        &self,
        graph_id: &str,
        expected: chrono::DateTime<chrono::Utc>,
    ) -> Result<bool> {
        let current_started_at = self.db.get_graph(graph_id)?.and_then(|lp| lp.started_at);
        let still_current =
            current_started_at.is_some_and(|at| at.timestamp() == expected.timestamp());
        if still_current {
            return Ok(false);
        }
        tracing::info!(
            "Graph '{}' failure from a stale dispatch (claimed at {}) ignored — a newer \
             dispatch has since taken over; this attempt's own run row already records \
             its own outcome.",
            graph_id,
            expected.to_rfc3339()
        );
        Ok(true)
    }

    /// Record the failed-graph activity: the spec failure (when a spec is
    /// named) plus the graph failure.
    fn record_failed_activity(
        &self,
        graph_id: &str,
        lp: &Graph,
        spec_id: Option<&str>,
        spec_name: Option<&str>,
        summary: &str,
    ) {
        let spec_id = spec_id.map(str::to_string).or_else(|| {
            spec_name.and_then(|name| {
                self.db
                    .resolve_spec_id_by_name(graph_id, name)
                    .ok()
                    .flatten()
            })
        });
        if let Some(spec_name) = spec_name {
            activity::record(
                &self.db,
                activity::GraphEvent {
                    graph_id,
                    graph_name: &lp.name,
                    workdir: &lp.workdir,
                    spec_id: spec_id.as_deref(),
                    node_id: None,
                    run_id: None,
                    kind: activity::GraphEventKind::SpecFailed,
                    message: format!("Spec '{spec_name}' failed: {summary}"),
                    payload: None,
                },
            );
        }
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id,
                graph_name: &lp.name,
                workdir: &lp.workdir,
                spec_id: None,
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::GraphFailed,
                message: format!("Failed: {summary}"),
                payload: None,
            },
        );
    }

    /// Fire `lp`'s `on_failed` hooks, naming the ending node (with its dirty
    /// note when the spec ended dirty) as the blocker's node.
    async fn fire_on_failed_hooks(
        &self,
        lp: &Graph,
        spec_id: Option<&str>,
        ending_node: Option<String>,
        summary: &str,
        completed_specs: &[(String, String)],
    ) {
        let ending_node_with_dirty_note = spec_id
            .and_then(|id| self.db.get_graph_spec(id).ok().flatten())
            .and_then(|spec| {
                dirty_tree_note(
                    spec.spec_end_dirty,
                    &spec.spec_end_dirty_paths.unwrap_or_default(),
                )
            })
            .map(|note| match ending_node.as_deref() {
                Some(node) => format!("{node} ({note})"),
                None => note,
            })
            .or_else(|| ending_node.clone());
        let ctx = HookContext {
            graph_name: &lp.name,
            workdir: &lp.workdir,
            completed_specs,
            spec_name: None,
            spec_id: None,
            blocker: Some(summary),
            node_name: ending_node_with_dirty_note.as_deref(),
            spec_code: None,
            spec_commit_message: None,
            spec_scope: None,
            spec_type: None,
        };
        self.fire_hooks(lp, GraphHookEvent::OnFailed, &ctx).await;
    }

    /// C19: the `Blocked` counterpart to [`Self::fail_graph`] — same
    /// stale-dispatch guard (a reset + relaunch that's already claimed the
    /// graph must not be paused out from under it) and the same B12 sweep of
    /// any run still `running` under `graph_id`, but pauses the graph instead
    /// of failing it and fires [`Self::notify_blocked`] instead of the
    /// ordinary failed-graph notification. This is what a spec that exceeded
    /// its persisted cross-run attempt budget routes through: unlike
    /// `Failed`, `Paused` is not accepted by a pending autorun
    /// ([`crate::domain::graphs::Graph::is_autorun_due`] already excludes it
    /// unless `paused_by_reconciliation`, which `update_graph_status` always
    /// clears) and is refused by `graph_run` once it carries a blocker (see
    /// the daemon's `graph_run` tool) — exactly the "not started again until
    /// a human clears it" FR4 asks for, reusing the graph_report_blocker
    /// mechanism wholesale rather than inventing a parallel one.
    pub(crate) async fn block_graph(
        &self,
        graph_id: &str,
        dispatch_started_at: Option<chrono::DateTime<chrono::Utc>>,
        blocker: &str,
    ) -> Result<()> {
        if let Some(expected) = dispatch_started_at {
            let current_started_at = self.db.get_graph(graph_id)?.and_then(|lp| lp.started_at);
            let still_current =
                current_started_at.is_some_and(|at| at.timestamp() == expected.timestamp());
            if !still_current {
                tracing::info!(
                    "Graph '{}' block from a stale dispatch (claimed at {}) ignored — a newer \
                     dispatch has since taken over; this attempt's own run row already records \
                     its own outcome.",
                    graph_id,
                    expected.to_rfc3339()
                );
                return Ok(());
            }
        }

        // Resolve the ending node BEFORE sweeping running runs, then share
        // the single post-transition blocked helper with `graph_report_blocker`
        // (notification + exactly one `on_blocked` firing per transition).
        let ending_node = self.ending_node_name(graph_id, None);
        self.db
            .update_graph_status(graph_id, GraphStatus::Paused, None, None)?;
        if let Ok(Some(lp)) = self.db.get_graph(graph_id) {
            activity::record(
                &self.db,
                activity::GraphEvent {
                    graph_id,
                    graph_name: &lp.name,
                    workdir: &lp.workdir,
                    spec_id: None,
                    node_id: None,
                    run_id: None,
                    kind: activity::GraphEventKind::GraphBlocked,
                    message: format!("Blocked: {blocker}"),
                    payload: None,
                },
            );
        }
        for run in self
            .db
            .list_running_graph_runs(graph_id)
            .unwrap_or_default()
        {
            self.terminate_run(&run, "spec exceeded cross-run attempt budget");
        }
        self.notify_blocked(graph_id, blocker)?;
        self.fire_on_blocked_hooks(graph_id, blocker, ending_node.as_deref())
            .await;
        Ok(())
    }
}
