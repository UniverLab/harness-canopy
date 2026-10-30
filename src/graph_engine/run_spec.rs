use super::verdict_shutdown::merge_process_record;
use super::*;

/// the directed-start activity message — the start node, the
/// nodes skipped between Start and it, the outputs reused (by node and run
/// id), and the counter restart.
fn directed_start_message(
    spec_label: &str,
    target: &super::directed::DirectedTarget,
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    existing_runs: &[GraphNodeRun],
) -> String {
    let skipped = if target.skipped_nodes.is_empty() {
        String::new()
    } else {
        format!(" [{}]", target.skipped_nodes.join(", "))
    };
    let reused = reused_output_records(nodes, edges, &target.start_node_ids, existing_runs);
    let reused = if reused.is_empty() {
        "none".to_string()
    } else {
        reused
            .iter()
            .map(|(name, run_id)| format!("{name} ({run_id})"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "Directed run: spec {} starts at {}; skipped {} node(s){}; \
         reused outputs: {reused}; iteration counters restarted at 1.",
        spec_label,
        target.label,
        target.skipped_nodes.len(),
        skipped
    )
}

/// Records the directed-start activity event for `start` when it targets
/// `spec`; an unresolvable target records nothing (the dispatch already
/// refused it upstream).
fn record_directed_start(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
    workdir: &str,
    spec: &GraphSpec,
    graph: (&[GraphNode], &[GraphEdge], &[EnsembleDetails]),
    existing_runs: &[GraphNodeRun],
    start: &DirectedStart,
) {
    let (nodes, edges, ensembles) = graph;
    let Ok(target) = resolve_directed_target(&start.node_arg, nodes, edges, ensembles) else {
        return;
    };
    activity::record(
        db,
        activity::GraphEvent {
            graph_id: &lp.id,
            graph_name: &lp.name,
            workdir,
            spec_id: Some(&spec.id),
            node_id: None,
            run_id: None,
            kind: activity::GraphEventKind::SpecStarted,
            message: directed_start_message(
                &activity::spec_label(spec),
                &target,
                nodes,
                edges,
                existing_runs,
            ),
            payload: None,
        },
    );
}

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
        directed: Option<&DirectedStart>,
    ) -> Result<SpecExecutionOutcome> {
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &lp.id,
                graph_name: &lp.name,
                workdir,
                spec_id: Some(&spec.id),
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::SpecStarted,
                message: format!("Spec {} started.", activity::spec_label(spec)),
                payload: None,
            },
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
        let (cursor, node_outputs, resume_previous_output, iterations) = resolve_spec_start(
            nodes,
            edges,
            spec,
            &existing_runs,
            &ensembles,
            is_resume,
            directed,
        )?;
        let (spec_start_head, spec_committed_head) = self
            .resolve_spec_baseline(spec, &spec_details, workdir, is_resume, directed.is_some())
            .await?;

        self.db.update_graph_spec_status(
            &spec.id,
            GraphSpecStatus::Running,
            Some(chrono::Utc::now()),
            None,
        )?;

        // record the directed start on the run's activity feed —
        // the start node, the nodes skipped between Start and it, the
        // outputs reused (by node and run id), and the counter restart.
        if let Some(start) = directed.filter(|d| d.spec_id == spec.id) {
            record_directed_start(
                &self.db,
                lp,
                workdir,
                spec,
                (nodes, edges, &ensembles),
                &existing_runs,
                start,
            );
        }

        // RS2: session ids captured for each node during THIS dispatch, so a
        // fail-edge bounce back to a node can resume its prior session instead
        // of cold-starting. In-memory only and local to this call.
        // CM26: round-robin's per-spec pinned member index. Same lifetime:
        // fresh per spec dispatch.
        // RS3: the context group this spec belongs to within the running
        // queue, if any. The ONE deliberate exception to RS2's "first visit
        // is cold" rule.
        let spec_group = self.spec_group_for(queue_id, &spec.id)?;

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

    /// RS3: the context group this spec belongs to within the running queue,
    /// if any.
    fn spec_group_for(&self, queue_id: Option<&str>, spec_id: &str) -> Result<Option<String>> {
        match queue_id {
            Some(pid) => self.db.queue_member_group(pid, spec_id),
            None => Ok(None),
        }
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
        // CM48: a single agent node whose pair is declared unavailable blocks
        // here, before any run row is inserted or attempt counted — the
        // existing `block_graph` path (via `SpecExecutionOutcome::Blocked`)
        // pauses the graph and fires the `on_blocked` hooks.
        if let Some(node) = self.db.get_graph_node(node_id)? {
            if let Some(blocker) = self.node_availability_blocker(&node) {
                return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Blocked(blocker)));
            }
        }
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
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: Some(&node.id),
                run_id: Some(&attempt_setup.run_id),
                kind: activity::GraphEventKind::NodeStarted,
                message: format!("Node '{}' started.", node.name),
                payload: None,
            },
        );
        let mut retry = NodeRetryCtx {
            node,
            previous_output,
            run_id: attempt_setup.run_id.clone(),
            resume_candidate: attempt_setup.resume_candidate.clone(),
            resume_crosses_spec: attempt_setup.resume_crosses_spec,
            iteration_value,
            crew: attempt_setup.crew.clone(),
            crew_index: attempt_setup.crew_index,
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

        // CB82: the daemon began graceful shutdown while this node ran (its
        // process was or will be SIGTERMed). Finalize as interrupted and
        // route no edge — never the fail edge a nonzero exit would take.
        if self.is_shutting_down() {
            return Ok(SpecStepFlow::Done(
                self.finalize_shutdown_interrupted(&run_id),
            ));
        }

        // B42/2026-08-05: something terminated this run out from
        // under us — a newer attempt at this node, a concurrent
        // `graph_control`, `graph_control`, or budget/`fail_graph`
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

        self.publish_node_outcome(ctx, node, &execution, &run_id);

        // CB31: a wait-for-completion pause was requested while this
        // node was running. It ran to its own completion and its
        // verdict is now recorded normally; flag that run so
        // `graph_control` re-executing this node does not spend one
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
        // across an await), so a graph_ensemble applied between
        // two dispatches lands on the second (FR2). The launch-time
        // `ensembles` snapshot stays authoritative for the ensemble's
        // WIRING (entry/exit edges, used by cursor_node_ids /
        // select_next_step) and its `kind`, which graph_ensemble
        // refuses to change while the graph is `running` (FR5).
        let details_fresh = self
            .db
            .get_ensemble_details(ensemble_id)
            .map_err(|e| anyhow!("Ensemble '{}' lookup failed: {e}", ensemble_id))?
            .ok_or_else(|| anyhow!("Ensemble '{}' not found in graph.", ensemble_id))?;
        // CM49 req 5a: lazy crew sync at dispatch, BEFORE any member runs this
        // visit (point (a) holds by construction; the sync's own in-flight
        // guard is the backstop). A pending binding whose crew does not exist
        // yet blocks naming the crew instead of dispatching with no members.
        let details_fresh = match self.db.sync_ensemble_with_crew(ensemble_id)? {
            crate::db::crews::SyncOutcome::Synced { .. } => self
                .db
                .get_ensemble_details(ensemble_id)?
                .ok_or_else(|| anyhow!("Ensemble '{}' not found in graph.", ensemble_id))?,
            crate::db::crews::SyncOutcome::MissingCrew => {
                let crew = details_fresh.ensemble.crew_name.clone().unwrap_or_default();
                let owner = format!("ensemble '{}'", details_fresh.ensemble.name);
                return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Blocked(
                    super::crew_resolve::crew_missing_blocker(&owner, &crew),
                )));
            }
            _ => details_fresh,
        };
        // CM48: resolve availability now, at dispatch — an unavailable member
        // is skipped, and an ensemble with no available member (or a parallel
        // one below `min_pass`) blocks the spec before any member runs. No
        // member is edited; availability is a filter, not a rewrite. The full
        // member list still drives the dispatch: `round_robin_index` indexes
        // it, so the rotation keeps naming the same members whatever the
        // roster says today.
        let available_members = match self.resolve_ensemble_roster(&details_fresh) {
            EnsembleRoster::Blocked(blocker) => {
                return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Blocked(blocker)));
            }
            EnsembleRoster::Ready(members) => members,
        };
        let details = &details_fresh;
        let execution = self
            .execute_ensemble(
                ctx.lp,
                ctx.spec,
                details,
                &available_members,
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

        // CB82: as in the single-node step, for the ensemble join — stamp the
        // join row interrupted and route no edge, starting no further member.
        // Member rows need no marker: the ensemble resume cursor is the join,
        // and members always re-run as a set.
        if self.is_shutting_down() {
            self.stamp_ensemble_join_interrupted(ctx.spec, details)?;
            return Ok(SpecStepFlow::Done(SpecExecutionOutcome::Superseded));
        }

        // CB31: as in the single-node step — a wait-for-completion
        // pause landed on this ensemble step. Its members ran to
        // completion; flag every run of this iteration so a
        // `graph_control` re-run of the step costs no iteration.
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
        // `Waiting` finalizes like `Running`: the gate settles a refused wait
        // itself, so reaching here still waiting means its write failed — the
        // execution in hand (a refused-wait Fail) is still the right verdict.
        if matches!(
            run.status,
            GraphRunStatus::Running | GraphRunStatus::Waiting
        ) {
            self.db.update_graph_run_result(
                run_id,
                execution.status,
                Some(&execution.output),
                Some(chrono::Utc::now()),
            )?;
            return Ok(execution);
        }
        // A self-reported run keeps its own output; only the process record
        // (ended_after_verdict / session_end) is merged onto it, never the verdict.
        let output = merge_process_record(&self.db, run_id, run, &execution.output);
        Ok(NodeExecution {
            status: run.status,
            output,
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
            ctx.workdir,
            execution.output,
        )
        .await;
        tracing::warn!(
            node = %node.name,
            head_before = %watch.head_before,
            head_after = %head_after,
            "node without commit rights: HEAD moved during its run"
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
        run_id: &str,
    ) {
        match node.kind {
            GraphNodeKind::Agent => {
                self.publish_agent_node_outcome(ctx, node, execution, run_id);
            }
            GraphNodeKind::Check | GraphNodeKind::Gate
                if execution.status != GraphRunStatus::Pass =>
            {
                self.publish_check_node_outcome(ctx, node, execution, run_id);
            }
            _ => {}
        }
    }

    /// CM46: the `node_finished` payload — verdict, executed platform/model and
    /// wall-clock duration, the fields a reader scans for.
    fn node_finished_payload(
        &self,
        node: &GraphNode,
        execution: &NodeExecution,
        run_id: &str,
    ) -> Value {
        let (platform, model) = executed_pair_for_node(node);
        let duration_s = self
            .db
            .get_graph_run(run_id)
            .ok()
            .flatten()
            .and_then(|run| {
                run.completed_at
                    .map(|done| (done - run.started_at).num_seconds())
            });
        serde_json::json!({
            "verdict": execution.status.as_str(),
            "platform": platform,
            "model": model,
            "duration_s": duration_s,
        })
    }

    pub(crate) fn publish_agent_node_outcome(
        &self,
        ctx: &SpecStepCtx<'_>,
        node: &GraphNode,
        execution: &NodeExecution,
        run_id: &str,
    ) {
        let reason = if execution.status == GraphRunStatus::Pass {
            None
        } else {
            failure_reason_text(&execution.output, &execution.summary)
        };
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: Some(&node.id),
                run_id: Some(run_id),
                kind: activity::GraphEventKind::NodeFinished,
                message: match (execution.status, &reason) {
                    (GraphRunStatus::Pass, _) => format!("Node '{}' passed.", node.name),
                    (_, Some(reason)) => format!("Node '{}' failed: {reason}", node.name),
                    (_, None) => format!("Node '{}' failed.", node.name),
                },
                payload: Some(self.node_finished_payload(node, execution, run_id)),
            },
        );
    }

    pub(crate) fn publish_check_node_outcome(
        &self,
        ctx: &SpecStepCtx<'_>,
        node: &GraphNode,
        execution: &NodeExecution,
        run_id: &str,
    ) {
        let exit_code = execution
            .output
            .get("exit_code")
            .and_then(serde_json::Value::as_i64);
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: Some(&node.id),
                run_id: Some(run_id),
                kind: activity::GraphEventKind::NodeFinished,
                message: match exit_code {
                    Some(code) => format!(
                        "Node '{}' ({}) failed: exit {code}.",
                        node.name,
                        node.kind.as_str()
                    ),
                    None => format!("Node '{}' ({}) failed.", node.name, node.kind.as_str()),
                },
                payload: Some(self.node_finished_payload(node, execution, run_id)),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two-node chain (`first` -> `second`) with a directed start at
    /// `second`, so the recorded line names one skipped node and no reused
    /// outputs.
    fn directed_label_fixture(
        code: Option<&str>,
    ) -> (
        tempfile::TempDir,
        Database,
        Graph,
        GraphSpec,
        Vec<GraphNode>,
        Vec<GraphEdge>,
        DirectedStart,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let lp = Graph {
            id: "g1".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/tmp/directed-label".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            allow_dirty_start: false,
            require_spec_type: false,
        };
        let spec = GraphSpec {
            id: "s1".to_string(),
            graph_id: Some(lp.id.clone()),
            name: "Add retry".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_type: None,
            spec_scope: None,
            spec_code: code.map(str::to_string),
        };
        let nodes = vec![
            GraphNode {
                id: "n-a".to_string(),
                spec_id: None,
                graph_id: Some(lp.id.clone()),
                name: "first".to_string(),
                kind: GraphNodeKind::Check,
                config: serde_json::json!({}),
                position: 1,
                created_at: chrono::Utc::now(),
            },
            GraphNode {
                id: "n-b".to_string(),
                spec_id: None,
                graph_id: Some(lp.id.clone()),
                name: "second".to_string(),
                kind: GraphNodeKind::Check,
                config: serde_json::json!({}),
                position: 2,
                created_at: chrono::Utc::now(),
            },
        ];
        let edges = vec![GraphEdge {
            id: "e1".to_string(),
            spec_id: None,
            graph_id: Some(lp.id.clone()),
            from_node: "n-a".to_string(),
            to_node: "n-b".to_string(),
            condition: GraphEdgeCondition::Pass,
        }];
        let start = DirectedStart {
            spec_id: spec.id.clone(),
            node_arg: "n-b".to_string(),
        };
        (dir, db, lp, spec, nodes, edges, start)
    }

    fn directed_activity_message(code: Option<&str>) -> String {
        let (_dir, db, lp, spec, nodes, edges, start) = directed_label_fixture(code);
        let ensembles: Vec<EnsembleDetails> = Vec::new();
        let runs: Vec<GraphNodeRun> = Vec::new();
        record_directed_start(
            &db,
            &lp,
            "/tmp/directed-label",
            &spec,
            (&nodes, &edges, &ensembles),
            &runs,
            &start,
        );
        let activity = db
            .list_activity_log_entries("/tmp/directed-label", 10)
            .unwrap();
        assert_eq!(activity.len(), 1);
        activity.into_iter().next().unwrap().message
    }

    #[test]
    fn directed_start_activity_shows_code_before_name() {
        let message = directed_activity_message(Some("FIX-7"));
        assert!(
            message.contains("[FIX-7] 'Add retry'"),
            "code before name: {message}"
        );
        assert!(
            !message.contains("''"),
            "the label carries its own quotes: {message}"
        );
    }

    #[test]
    fn directed_start_activity_keeps_legacy_text_without_code() {
        assert_eq!(
            directed_activity_message(None),
            "Directed run: spec 'Add retry' starts at node 'second'; \
             skipped 1 node(s) [first]; reused outputs: none; \
             iteration counters restarted at 1."
        );
    }
}
