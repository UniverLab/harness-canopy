use super::*;

impl GraphEngine {
    /// Run one spec's node graph to completion, pause, or failure.
    ///
    /// `is_resume` (see [`Self::run_graph_dispatch`]) governs whether
    /// `{{spec_start_head}}` may be inherited from a value this spec already
    /// persisted (only valid when this call is genuinely continuing the same
    /// in-flight attempt) or must be captured fresh (every other case,
    /// including a spec that is stuck `running` from an unrelated, never-reset
    /// prior attempt).
    pub(crate) async fn run_spec(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        workdir: &str,
        is_resume: bool,
        queue_id: Option<&str>,
    ) -> Result<SpecExecutionOutcome> {
        activity::publish(
            &self.db,
            workdir,
            &lp.id,
            &lp.name,
            format!("Spec '{}' started.", spec.name),
        );
        let spec_details = self
            .db
            .get_graph_spec_details(&spec.id)?
            .ok_or_else(|| anyhow!("Graph spec '{}' not found.", spec.id))?;
        let graph_nodes = self.db.list_graph_nodes_for_graph(&lp.id)?;
        let graph_edges = self.db.list_graph_edges_for_graph(&lp.id)?;

        let load = self
            .load_spec_graph(lp, spec, &spec_details, &graph_nodes, &graph_edges, workdir)
            .await?;
        let (nodes, edges, ensembles) = match load {
            SpecGraphLoad::Ready {
                nodes,
                edges,
                ensembles,
            } => (nodes, edges, ensembles),
            SpecGraphLoad::NoNodes(outcome) => return Ok(outcome),
        };

        let nodes_by_id = build_nodes_by_id(nodes);
        // B37: whether this graph designates a committer at all. Resolved
        // once from whichever graph won the precedence above, so a spec-level
        // graph and the top-level fallback each answer for themselves.
        let enforce_commit_rights = graph_enforces_commit_rights(nodes, &ensembles);
        let existing_runs = self.db.list_graph_runs_for_spec(&spec.id)?;
        let all_node_names: Vec<String> = nodes.iter().map(|n| n.name.clone()).collect();
        let (cursor, node_outputs, resume_previous_output, iterations) =
            resolve_spec_start(nodes, edges, spec, &existing_runs, &ensembles, is_resume)?;
        let (spec_start_head, spec_committed_head) = self
            .resolve_spec_baseline(spec, &spec_details, workdir, is_resume)
            .await?;

        self.db.update_graph_spec_status(
            &spec.id,
            GraphSpecStatus::Running,
            Some(chrono::Utc::now()),
            None,
        )?;

        // RS2: session ids captured for each node during THIS dispatch, so a
        // fail-edge bounce back to a node can resume its prior session instead
        // of cold-starting. In-memory only and local to this call.
        // CM26: round-robin's per-spec pinned member index. Same lifetime:
        // fresh per spec dispatch.
        // RS3: the context group this spec belongs to within the running
        // queue, if any. The ONE deliberate exception to RS2's "first visit
        // is cold" rule.
        let spec_group = match queue_id {
            Some(pid) => self.db.queue_member_group(pid, &spec.id)?,
            None => None,
        };

        let mut state = SpecRunState {
            cursor,
            node_outputs,
            resume_previous_output,
            iterations,
            previous_node_name: None,
            spec_committed_head,
            resumable_sessions: HashMap::new(),
            round_robin_pins: HashMap::new(),
        };
        let ctx = SpecStepCtx {
            lp,
            spec,
            workdir,
            queue_id,
            enforce_commit_rights,
            edges,
            ensembles: &ensembles,
            nodes_by_id: &nodes_by_id,
            all_node_names: &all_node_names,
            spec_start_head: spec_start_head.as_deref(),
            spec_group: spec_group.as_deref(),
        };

        self.run_spec_loop(&ctx, &mut state).await
    }

    pub(crate) async fn run_spec_loop(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
    ) -> Result<SpecExecutionOutcome> {
        loop {
            if self.is_paused(&ctx.lp.id)? {
                return Ok(SpecExecutionOutcome::Paused);
            }

            let previous_output = current_previous_output(state);

            let budget_key = budget_key(&state.cursor);

            // Reap a `running` row left by a prior attempt at this exact
            // node/ensemble that was never finalized (crashed mid-execution,
            // daemon restarted before its own timeout handler ran, etc.) —
            // B12. Execution here is strictly sequential (one cursor step in
            // flight per spec at a time — an `Ensemble` step's own members
            // run concurrently with each other, but never alongside another
            // step), so anything still `running` at this point can only be a
            // leftover, never the legitimately active run: this iteration's
            // own rows don't exist yet.
            self.reap_stale_cursor_runs(&state.cursor, ctx.ensembles)?;

            let (iteration_value, budget_outcome) =
                self.check_iteration_budget(ctx, state, &budget_key).await?;
            if let Some(outcome) = budget_outcome {
                return Ok(outcome);
            }

            let stepped_cursor = state.cursor.clone();
            let flow = match stepped_cursor {
                SpecCursor::Node(node_id) => {
                    self.run_spec_node_step(
                        ctx,
                        state,
                        &node_id,
                        previous_output.as_ref(),
                        iteration_value,
                    )
                    .await?
                }
                SpecCursor::Ensemble(ensemble_id) => {
                    self.run_spec_ensemble_step(
                        ctx,
                        state,
                        &ensemble_id,
                        previous_output.as_ref(),
                        iteration_value,
                    )
                    .await?
                }
            };
            let step = match flow {
                SpecStepFlow::Done(outcome) => return Ok(outcome),
                SpecStepFlow::Routed(step) => step,
            };
            match self.apply_spec_step(ctx, state, step).await? {
                SpecLoopFlow::Continue => continue,
                SpecLoopFlow::Done(outcome) => return Ok(outcome),
            }
        }
    }

    pub(crate) async fn run_spec_node_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        node_id: &str,
        previous_output: Option<&Value>,
        iteration_value: usize,
    ) -> Result<SpecStepFlow> {
        let attempt_setup = self
            .prepare_node_attempt(
                ctx,
                &state.resumable_sessions,
                node_id,
                previous_output,
                iteration_value,
            )
            .await?;
        let node = &attempt_setup.node;
        let mut retry = NodeRetryCtx {
            node,
            previous_output,
            run_id: attempt_setup.run_id.clone(),
            resume_candidate: attempt_setup.resume_candidate.clone(),
            resume_crosses_spec: attempt_setup.resume_crosses_spec,
            iteration_value,
        };
        let (execution, run, had_infra_crash) = self
            .run_node_with_infra_retries(ctx, state, &mut retry)
            .await?;
        let run_id = retry.run_id;

        tracing::info!(
            run_id = %run_id,
            status = ?run.status,
            node = %node.name,
            "node run completed"
        );

        // B42/2026-08-05: something terminated this run out from
        // under us — a newer attempt at this node, a concurrent
        // `graph_reset`, `graph_pause`, or budget/`fail_graph`
        // sweep (see `run_was_terminated_out_of_band`). That is
        // engine bookkeeping, not a node failure — so this
        // dispatch stops here: it evaluates NO edge (never the
        // fail edge to a resilience node), fails nothing, and
        // leaves the graph to whichever dispatch now owns it.
        // Checked before any routing so the termination can
        // never be routed as a fail (the runaway that
        // manufactured a resilience run per killed implementer,
        // and the incident where a stale dispatch's late
        // completion failed a graph out from under a healthy
        // sibling dispatch).
        if run_was_terminated_out_of_band(&run) {
            return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Superseded));
        }

        // RS2: remember this node's captured session so a later
        // fail-edge bounce back to it resumes instead of cold
        // starting. A resumed run recorded the same id it continued;
        // a cold run recorded whatever it captured (or nothing).
        if let Some(sid) = run.session_id.clone() {
            state.resumable_sessions.insert(node.id.clone(), sid);
        }

        let execution = self.resolve_running_node_execution(&run_id, execution, &run)?;

        // B37: applied AFTER the node's own verdict is settled,
        // so it overrides every way a node can report success —
        // a clean exit code, or a `graph_complete_node` self-report
        // of `pass`. A node that moved history without the right
        // to fails, and the fail is persisted on the run row so
        // `canopy graph info` shows it.
        let execution = self
            .apply_commit_rights_violation(ctx, &attempt_setup, &run_id, execution)
            .await?;

        // CB39: verify spec_start_head is still an ancestor of
        // HEAD after a committing node's turn. Runs whether the
        // node reported pass or fail. Skipped when there is no
        // spec_start_head (non-git workdir) — the engine stays
        // usable outside git.
        let (execution, had_infra_crash) = self
            .enforce_commit_ancestry(
                ctx,
                state,
                &attempt_setup,
                &run_id,
                execution,
                had_infra_crash,
            )
            .await?;

        self.publish_node_outcome(ctx, node, &execution);

        // CB31: a wait-for-completion pause was requested while this
        // node was running. It ran to its own completion and its
        // verdict is now recorded normally; flag that run so
        // `graph_continue` re-executing this node does not spend one
        // of its iterations (the pause is not an attempt), then
        // transition to paused and stop before the next node.
        if let Some(outcome) = self
            .pause_after_step_if_requested(ctx, state, iteration_value)
            .await?
        {
            return Ok(SpecStepFlow::Done(outcome));
        }

        if should_advance_to_next_spec(node, execution.status) {
            return Ok(SpecStepFlow::Done(
                self.finish_advanced_spec(ctx, state, execution).await?,
            ));
        }

        Ok(SpecStepFlow::Routed(NodeStepResult {
            execution,
            from_node_id: node.id.clone(),
            run_id: Some(run_id),
            had_infra_crash,
        }))
    }

    pub(crate) async fn run_spec_ensemble_step(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        ensemble_id: &str,
        previous_output: Option<&Value>,
        iteration_value: usize,
    ) -> Result<SpecStepFlow> {
        // CM15: the ensemble's MEMBER SET and shared prompt are read
        // from the database HERE, once per dispatch of this ensemble
        // step (NFR3: the whole set in one read, never member-by-member
        // across an await), so a graph_update_ensemble applied between
        // two dispatches lands on the second (FR2). The launch-time
        // `ensembles` snapshot stays authoritative for the ensemble's
        // WIRING (entry/exit edges, used by cursor_node_ids /
        // select_next_step) and its `kind`, which graph_update_ensemble
        // refuses to change while the graph is `running` (FR5).
        let details_fresh = self
            .db
            .get_ensemble_details(ensemble_id)
            .map_err(|e| anyhow!("Ensemble '{}' lookup failed: {e}", ensemble_id))?
            .ok_or_else(|| anyhow!("Ensemble '{}' not found in graph.", ensemble_id))?;
        let details = &details_fresh;
        let execution = self
            .execute_ensemble(
                ctx.lp,
                ctx.spec,
                details,
                previous_output,
                iteration_value,
                ctx.workdir,
                ctx.enforce_commit_rights,
                &state.node_outputs,
                ctx.all_node_names,
                &mut state.resumable_sessions,
                &mut state.round_robin_pins,
            )
            .await?;

        // CB31: as in the single-node step — a wait-for-completion
        // pause landed on this ensemble step. Its members ran to
        // completion; flag every run of this iteration so a
        // `graph_continue` re-run of the step costs no iteration.
        if let Some(outcome) = self
            .pause_after_step_if_requested(ctx, state, iteration_value)
            .await?
        {
            return Ok(SpecStepFlow::Done(outcome));
        }

        Ok(SpecStepFlow::Routed(NodeStepResult {
            execution,
            from_node_id: details.ensemble.join_node_id.clone(),
            run_id: None,
            had_infra_crash: false,
        }))
    }

    /// Settle a node's own verdict against its run row: a row still
    /// `Running` is finalized with this execution, otherwise the row
    /// (written by `graph_complete_node` or the timeout handler) wins
    /// and the execution is rebuilt from it.
    pub(crate) fn resolve_running_node_execution(
        &self,
        run_id: &str,
        execution: NodeExecution,
        run: &GraphNodeRun,
    ) -> Result<NodeExecution> {
        if run.status == GraphRunStatus::Running {
            self.db.update_graph_run_result(
                run_id,
                execution.status,
                Some(&execution.output),
                Some(chrono::Utc::now()),
            )?;
            return Ok(execution);
        }
        Ok(NodeExecution {
            status: run.status,
            output: run.output.clone().unwrap_or_else(|| serde_json::json!({})),
            summary: execution.summary,
        })
    }

    pub(crate) async fn apply_commit_rights_violation(
        &self,
        ctx: &SpecStepCtx<'_>,
        setup: &NodeAttemptSetup,
        run_id: &str,
        execution: NodeExecution,
    ) -> Result<NodeExecution> {
        let Some(watch) = setup.commit_watch.as_ref() else {
            return Ok(execution);
        };
        let Some(head_after) = watch.violation(ctx.workdir).await else {
            return Ok(execution);
        };
        let node = &setup.node;
        let violation = commit_rights_failure(
            &format!("Node '{}'", node.name),
            &node.id,
            &watch.head_before,
            &head_after,
            execution.output,
        );
        tracing::warn!(
            node = %node.name,
            head_before = %watch.head_before,
            head_after = %head_after,
            "node committed but has no commit rights"
        );
        self.db.update_graph_run_result(
            run_id,
            GraphRunStatus::Fail,
            Some(&violation.output),
            Some(chrono::Utc::now()),
        )?;
        Ok(violation)
    }

    pub(crate) async fn enforce_commit_ancestry(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &mut SpecRunState,
        setup: &NodeAttemptSetup,
        run_id: &str,
        execution: NodeExecution,
        had_infra_crash: bool,
    ) -> Result<(NodeExecution, bool)> {
        let (execution, had_infra_crash, ancestry_broken) = self
            .check_commit_ancestry(ctx, setup, run_id, execution, had_infra_crash)
            .await?;
        // C15: only write spec_committed_head when ancestry check
        // passed (or was skipped). A broken ancestry means the start
        // head is orphaned — recording a committed head would mask
        // the problem.
        if ancestry_broken {
            return Ok((execution, had_infra_crash));
        }
        let Some(head_before) = setup.committer_head_before.as_deref() else {
            return Ok((execution, had_infra_crash));
        };
        let Some(head_after) = capture_workdir_head(ctx.workdir).await else {
            return Ok((execution, had_infra_crash));
        };
        if head_after == head_before {
            return Ok((execution, had_infra_crash));
        }
        self.db
            .set_graph_spec_committed_head(&ctx.spec.id, Some(&head_after))?;
        state.spec_committed_head = Some(head_after);
        Ok((execution, had_infra_crash))
    }

    pub(crate) async fn check_commit_ancestry(
        &self,
        ctx: &SpecStepCtx<'_>,
        setup: &NodeAttemptSetup,
        run_id: &str,
        execution: NodeExecution,
        had_infra_crash: bool,
    ) -> Result<(NodeExecution, bool, bool)> {
        let (Some(start_head), Some(_head_before)) =
            (ctx.spec_start_head, setup.committer_head_before.as_deref())
        else {
            return Ok((execution, had_infra_crash, false));
        };
        match check_start_head_ancestry(ctx.workdir, start_head).await {
            Some(true) | None => Ok((execution, had_infra_crash, false)),
            Some(false) => {
                let current_head = capture_workdir_head(ctx.workdir)
                    .await
                    .unwrap_or_else(|| "<unknown>".to_string());
                let ancestry_fail = ancestry_failure(
                    &ctx.spec.name,
                    start_head,
                    &current_head,
                    execution.output.clone(),
                );
                tracing::warn!(
                    spec = %ctx.spec.name,
                    spec_start_head = %start_head,
                    current_head = %current_head,
                    "spec_start_head is no longer an ancestor of HEAD"
                );
                self.db.update_graph_run_result(
                    run_id,
                    GraphRunStatus::Fail,
                    Some(&ancestry_fail.output),
                    Some(chrono::Utc::now()),
                )?;
                Ok((ancestry_fail, true, true))
            }
        }
    }

    pub(crate) fn publish_node_outcome(
        &self,
        ctx: &SpecStepCtx<'_>,
        node: &GraphNode,
        execution: &NodeExecution,
    ) {
        match node.kind {
            GraphNodeKind::Agent => self.publish_agent_node_outcome(ctx, node, execution),
            GraphNodeKind::Check | GraphNodeKind::Gate
                if execution.status != GraphRunStatus::Pass =>
            {
                self.publish_check_node_outcome(ctx, node, execution);
            }
            _ => {}
        }
    }

    pub(crate) fn publish_agent_node_outcome(
        &self,
        ctx: &SpecStepCtx<'_>,
        node: &GraphNode,
        execution: &NodeExecution,
    ) {
        let reason = if execution.status == GraphRunStatus::Pass {
            None
        } else {
            failure_reason_text(&execution.output, &execution.summary)
        };
        activity::publish(
            &self.db,
            ctx.workdir,
            &ctx.lp.id,
            &ctx.lp.name,
            match (execution.status, &reason) {
                (GraphRunStatus::Pass, _) => format!("Node '{}' passed.", node.name),
                (_, Some(reason)) => format!("Node '{}' failed: {reason}", node.name),
                (_, None) => format!("Node '{}' failed.", node.name),
            },
        );
    }

    pub(crate) fn publish_check_node_outcome(
        &self,
        ctx: &SpecStepCtx<'_>,
        node: &GraphNode,
        execution: &NodeExecution,
    ) {
        let exit_code = execution
            .output
            .get("exit_code")
            .and_then(serde_json::Value::as_i64);
        activity::publish(
            &self.db,
            ctx.workdir,
            &ctx.lp.id,
            &ctx.lp.name,
            match exit_code {
                Some(code) => format!(
                    "Node '{}' ({}) failed: exit {code}.",
                    node.name,
                    node.kind.as_str()
                ),
                None => format!("Node '{}' ({}) failed.", node.name, node.kind.as_str()),
            },
        );
    }
}
