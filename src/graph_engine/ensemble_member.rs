use super::ensemble_member_settle::{RoundRobinSettle, RoundRobinWalkCtx};
use super::*;

/// Invariant inputs of a single ensemble-member run, gathered once by
/// [`GraphEngine::run_ensemble_member`] and driven by
/// [`GraphEngine::drive_ensemble_member`].
pub(crate) struct EnsembleMemberContext {
    pub(crate) db: Arc<Database>,
    pub(crate) lp: crate::domain::graphs::Graph,
    pub(crate) spec: GraphSpec,
    pub(crate) node: GraphNode,
    pub(crate) previous_output: Option<Value>,
    pub(crate) iteration: i64,
    pub(crate) workdir: String,
    pub(crate) dynamic_skills: Option<Arc<crate::dynamic_skills::SkillStore>>,
    pub(crate) node_outputs: HashMap<String, Value>,
    pub(crate) all_node_names: Vec<String>,
    pub(crate) bound_minutes: u64,
    pub(crate) member_timeout_minutes: Option<i64>,
    /// CB82: the engine's shutdown flag — `drive_ensemble_member` is a static
    /// method with no `&GraphEngine`, so the flag travels in the context.
    pub(crate) shutdown: Arc<AtomicBool>,
    /// Mutex set this member run acquires ahead of every attempt: its node's
    /// own plus the ensemble's.
    pub(crate) mutex_resources: Vec<String>,
}

/// Record one member's `node_started` event (CM46: one per member
/// dispatch, before it spawns). The member's infra-retry budget itself is
/// read by the drive loop's [`AttemptChain`], not here.
fn begin_ensemble_member_run(
    db: &Database,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    workdir: &str,
    member_run_id: &str,
) {
    activity::record(
        db,
        activity::GraphEvent {
            graph_id: &lp.id,
            graph_name: &lp.name,
            workdir,
            spec_id: Some(&spec.id),
            node_id: Some(&node.id),
            run_id: Some(member_run_id),
            kind: activity::GraphEventKind::NodeStarted,
            message: format!("Node '{}' started.", node.name),
            payload: None,
        },
    );
}

/// Settle one finished member attempt: apply the B19 infra-crash shape
/// rewrite, persist the verdict when this drive still owns the run row, and
/// return `(final_execution, member_had_no_verdict, settled_session_id)`.
async fn settle_ensemble_member_attempt(
    ctx: &EnsembleMemberContext,
    member_run_id: &str,
    mut execution: NodeExecution,
    mut run: GraphNodeRun,
    chain: &AttemptChain,
) -> Result<(NodeExecution, bool, Option<String>)> {
    let member_had_no_verdict = finalize_infra_crash_shape(
        &ctx.db,
        member_run_id,
        &ctx.node,
        &mut execution,
        &mut run,
        chain.attempt,
        chain.retry_limit,
        chain.crash_max_secs,
    )
    .await?;
    let settled_session_id = run.session_id.clone();
    let final_execution = if matches!(
        run.status,
        GraphRunStatus::Running | GraphRunStatus::Waiting
    ) {
        let _ = ctx.db.update_graph_run_result(
            member_run_id,
            execution.status,
            Some(&execution.output),
            Some(chrono::Utc::now()),
        );
        execution
    } else {
        // A self-reported member keeps its own output; carry the
        // process record (ended_after_verdict / session_end) onto it.
        let output = super::verdict_shutdown::merge_process_record(
            &ctx.db,
            member_run_id,
            &run,
            &execution.output,
        );
        NodeExecution {
            status: run.status,
            output,
            summary: execution.summary,
        }
    };
    Ok((final_execution, member_had_no_verdict, settled_session_id))
}

impl GraphEngine {
    #[expect(clippy::too_many_arguments)]
    pub(crate) async fn run_ensemble_member(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        node: &GraphNode,
        previous_output: Option<&Value>,
        iteration: usize,
        workdir: &str,
        member_timeout_minutes: Option<i64>,
        ensemble_timeout_minutes: u64,
        node_outputs: &HashMap<String, Value>,
        all_node_names: &[String],
        initial_resume_candidate: Option<String>,
        resumable_sessions: &mut HashMap<String, String>,
        ensemble_mutex: &[String],
    ) -> Result<(NodeExecution, bool)> {
        let bound_minutes = member_timeout_minutes
            .map(|minutes| minutes.max(0) as u64)
            .unwrap_or(ensemble_timeout_minutes);
        let run_id = uuid::Uuid::new_v4().to_string();
        // CB43: member run — resolved from the node config at dispatch.
        let (executed_platform, executed_model) = executed_pair_for_node(node);
        let (event_platform, event_model) = (executed_platform.clone(), executed_model.clone());
        // The member run holds the union of its node's and its ensemble's
        // mutexes, per member run — it starts `waiting` when that union is
        // non-empty and the drive flips it to `running` at the grant.
        let mutex_resources = union_mutex_resources(node, ensemble_mutex);
        let initial_status = if mutex_resources.is_empty() {
            GraphRunStatus::Running
        } else {
            GraphRunStatus::Waiting
        };
        self.db.insert_graph_run(&GraphNodeRun {
            id: run_id.clone(),
            graph_id: lp.id.clone(),
            spec_id: spec.id.clone(),
            node_id: node.id.clone(),
            status: initial_status,
            input: previous_output.cloned(),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: iteration as i64,
            pid: None,
            boot_id: crate::system::boot_id(),
            session_id: None,
            executed_platform,
            executed_model,
        })?;

        let member_run_id = run_id.clone();
        begin_ensemble_member_run(&self.db, lp, spec, node, workdir, &member_run_id);
        let ctx = EnsembleMemberContext {
            db: Arc::clone(&self.db),
            lp: lp.clone(),
            spec: spec.clone(),
            node: node.clone(),
            previous_output: previous_output.cloned(),
            iteration: iteration as i64,
            workdir: workdir.to_string(),
            dynamic_skills: self.dynamic_skills.clone(),
            node_outputs: node_outputs.clone(),
            all_node_names: all_node_names.to_vec(),
            bound_minutes,
            member_timeout_minutes,
            shutdown: Arc::clone(&self.shutdown),
            mutex_resources,
        };

        let (result, member_had_no_verdict, settled_session_id) =
            Self::drive_ensemble_member(ctx, run_id, initial_resume_candidate).await?;

        if result.status == GraphRunStatus::Pass {
            if let Some(sid) = settled_session_id {
                resumable_sessions.insert(node.id.clone(), sid);
            }
        } else {
            resumable_sessions.remove(&node.id);
        }

        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &lp.id,
                graph_name: &lp.name,
                workdir,
                spec_id: Some(&spec.id),
                node_id: Some(&node.id),
                run_id: Some(&member_run_id),
                kind: activity::GraphEventKind::NodeFinished,
                message: match result.status {
                    GraphRunStatus::Pass => format!("Ensemble member '{}' passed.", node.name),
                    _ => format!(
                        "Ensemble member '{}' failed: {}",
                        node.name,
                        failure_reason_text(&result.output, &result.summary).unwrap_or_default()
                    ),
                },
                payload: Some(serde_json::json!({
                    "verdict": result.status.as_str(),
                    "platform": event_platform,
                    "model": event_model,
                    "duration_s": Value::Null,
                })),
            },
        );

        Ok((result, member_had_no_verdict))
    }

    pub(crate) fn member_timeout_source(
        member_timeout_minutes: Option<i64>,
    ) -> (&'static str, &'static str) {
        if member_timeout_minutes.is_some() {
            ("member's own timeout", "its own timeout_minutes")
        } else {
            (
                "ensemble's timeout_minutes",
                "the ensemble's timeout_minutes",
            )
        }
    }

    pub(crate) async fn drive_ensemble_member(
        ctx: EnsembleMemberContext,
        initial_run_id: String,
        initial_resume_candidate: Option<String>,
    ) -> Result<(NodeExecution, bool, Option<String>)> {
        let mut chain = AttemptChain::from_node(&ctx.node);
        let mut member_run_id = initial_run_id;
        let mut resume_candidate: Option<String> = initial_resume_candidate;
        let mut settled_session_id: Option<String> = None;

        // CB67: each iteration below re-arms `tokio::time::timeout` with the
        // SAME `bound_minutes` but a FRESH deadline, timed from that
        // iteration's own start — the initial attempt and every infra retry
        // each get the member's full effective timeout, never a shared
        // ensemble-wide (or even single-member-wide) clock.
        let result: (NodeExecution, bool) = loop {
            // The mutex gate sits ahead of every member attempt: the member's
            // own full timeout starts at the grant, not at the enqueue. A
            // refused wait ends the member with no verdict.
            if !acquire_mutexes_before_run(
                &ctx.db,
                &ctx.shutdown,
                &ctx.lp,
                &ctx.spec.id,
                &ctx.node,
                &member_run_id,
                &ctx.mutex_resources,
            )
            .await?
            {
                break (
                    interrupted_execution(format!(
                        "Ensemble member '{}' stopped while waiting for its mutex.",
                        ctx.node.name
                    )),
                    true,
                );
            }
            let execution = match run_member_attempt(
                &ctx,
                &member_run_id,
                resume_candidate.as_deref(),
                chain.nudged,
            )
            .await?
            {
                MemberAttempt::Execution(execution) => execution,
                MemberAttempt::NoVerdict(execution) => break (execution, true),
            };

            let run = ctx.db.get_graph_run(&member_run_id)?.ok_or_else(|| {
                anyhow!("Graph run '{}' not found after execution.", member_run_id)
            })?;
            match chain.decide(&ctx.node, &execution, &run) {
                AttemptRetry::Settle => {
                    let (final_execution, member_had_no_verdict, settled_session) =
                        settle_ensemble_member_attempt(
                            &ctx,
                            &member_run_id,
                            execution,
                            run,
                            &chain,
                        )
                        .await?;
                    settled_session_id = settled_session;
                    break (final_execution, member_had_no_verdict);
                }
                kind => {
                    // CB82: no new retry row is inserted during shutdown — settle
                    // the crashed attempt as interrupted and stop.
                    if ctx.shutdown.load(Ordering::SeqCst) {
                        let _ = ctx.db.update_graph_run_result(
                            &member_run_id,
                            GraphRunStatus::Fail,
                            Some(&shutdown_interrupted_output()),
                            Some(chrono::Utc::now()),
                        );
                        break (
                            interrupted_execution(format!(
                                "Ensemble member '{}' interrupted: daemon shutdown.",
                                ctx.node.name
                            )),
                            false,
                        );
                    }
                    resume_candidate = run.session_id.clone();
                    member_run_id = begin_infra_retry(
                        &ctx.db,
                        &ctx.lp,
                        &ctx.spec,
                        &ctx.node,
                        ctx.previous_output.as_ref(),
                        ctx.iteration,
                        &member_run_id,
                        &execution.output,
                        chain.attempt,
                        chain.backoff_secs,
                        &ctx.mutex_resources,
                    )
                    .await?;
                    chain.advance(kind);
                    continue;
                }
            }
        };

        Ok((result.0, result.1, settled_session_id))
    }

    #[expect(clippy::too_many_arguments)]
    pub(crate) async fn execute_ensemble_cascade(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        details: &EnsembleDetails,
        previous_output: Option<&Value>,
        iteration: usize,
        workdir: &str,
        enforce_commit_rights: bool,
        node_outputs: &HashMap<String, Value>,
        all_node_names: &[String],
        resumable_sessions: &mut HashMap<String, String>,
    ) -> Result<NodeExecution> {
        let ensemble = &details.ensemble;
        let ensemble_timeout_minutes = ensemble.timeout_minutes.max(0) as u64;
        let member_nodes = load_ensemble_member_nodes(&self.db, details)?;
        let any_member_may_commit = details.ensemble.commit_rights;
        let commit_watch =
            CommitRightsWatch::begin(enforce_commit_rights, any_member_may_commit, workdir).await;
        let previous_output_owned = previous_output.cloned();
        let join_ctx = EnsembleJoinCtx {
            lp,
            spec,
            join_node_id: &ensemble.join_node_id,
            workdir,
            iteration,
            input: previous_output_owned.as_ref(),
        };
        let (walk_start, initial_resume_candidate) =
            cascade_walk_start(details, resumable_sessions);
        let mut saw_verdict = false;
        let mut tried_members: Vec<(String, String, Value, String)> = Vec::new();
        for (offset, member) in details.members.iter().enumerate().skip(walk_start) {
            // CB82: stop before running another member — settle its active run
            // if any, return interrupted with no join row and no advance.
            if self.is_shutting_down() {
                return Ok(self.shutdown_interrupted_member_execution(
                    &member.node_id,
                    format!(
                        "Ensemble '{}' (cascade member) interrupted: daemon shutdown.",
                        ensemble.name
                    ),
                ));
            }
            let is_resume_start = offset == walk_start;
            let candidate = if is_resume_start {
                initial_resume_candidate.clone()
            } else {
                None
            };
            let node = round_robin_member_node(&member_nodes, member)?;
            let (execution, member_had_no_verdict) = self
                .run_ensemble_member(
                    lp,
                    spec,
                    &node,
                    previous_output,
                    iteration,
                    workdir,
                    member.timeout_minutes,
                    ensemble_timeout_minutes,
                    node_outputs,
                    all_node_names,
                    candidate.clone(),
                    resumable_sessions,
                    &ensemble.mutex,
                )
                .await?;
            let resumed_start_failed = cascade_is_resumed_start_failed(
                is_resume_start,
                candidate.is_some(),
                member_had_no_verdict,
                execution.status,
            );
            saw_verdict |= !member_had_no_verdict;
            tried_members.push((
                node.name.clone(),
                member.platform.clone(),
                execution.output.clone(),
                execution.summary.clone(),
            ));
            if !member_had_no_verdict && !resumed_start_failed {
                let join_execution = cascade_winner_execution(
                    ensemble, details, member, &node, &execution, offset, walk_start,
                );
                let join_execution = apply_round_robin_commit_watch(
                    join_execution,
                    &commit_watch,
                    workdir,
                    format!("Ensemble '{}' (cascade member)", ensemble.name),
                    &ensemble.join_node_id,
                )
                .await;
                self.record_ensemble_join(&join_ctx, &join_execution)
                    .await?;
                return Ok(join_execution);
            }
            log_cascade_fallthrough(&ensemble.id, &node, member, resumed_start_failed);
        }
        let execution =
            cascade_terminal_execution(ensemble, details, walk_start, saw_verdict, &tried_members);
        let execution = apply_round_robin_commit_watch(
            execution,
            &commit_watch,
            workdir,
            format!("Ensemble '{}' (cascade)", ensemble.name),
            &ensemble.join_node_id,
        )
        .await;
        self.record_ensemble_join(&join_ctx, &execution).await?;
        Ok(execution)
    }

    #[expect(clippy::too_many_arguments)]
    pub(crate) async fn execute_ensemble_round_robin(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        details: &EnsembleDetails,
        available_members: &[EnsembleMember],
        previous_output: Option<&Value>,
        iteration: usize,
        workdir: &str,
        enforce_commit_rights: bool,
        node_outputs: &HashMap<String, Value>,
        all_node_names: &[String],
        resumable_sessions: &mut HashMap<String, String>,
        round_robin_pins: &mut HashMap<String, i64>,
    ) -> Result<NodeExecution> {
        let setup = self
            .prepare_round_robin(
                details,
                available_members,
                enforce_commit_rights,
                workdir,
                round_robin_pins,
                resumable_sessions,
            )
            .await?;
        let ensemble = &details.ensemble;
        let previous_output_owned = previous_output.cloned();
        let join_ctx = EnsembleJoinCtx {
            lp,
            spec,
            join_node_id: &ensemble.join_node_id,
            workdir,
            iteration,
            input: previous_output_owned.as_ref(),
        };
        let mut tried_no_verdict: Vec<(String, String, Value, String)> = Vec::new();
        let mut members_ran: i64 = 0;
        let mut skipped_unavailable = false;
        for offset in 0..setup.member_count {
            let current_index = (setup.start_index + offset).rem_euclid(setup.member_count);
            let member = &details.members[current_index as usize];
            // CM48: an unavailable member is stepped over, not counted.
            if !setup.available_members.contains(member.node_id.as_str()) {
                skipped_unavailable = true;
                continue;
            }
            members_ran += 1;
            // The rotation cursor is the member that actually ran, so a bounce
            // back into the ensemble continues past the roster's skips.
            if skipped_unavailable {
                round_robin_pins.insert(ensemble.id.clone(), current_index);
            }
            // CB82: no further round-robin member starts during shutdown.
            if self.is_shutting_down() {
                return Ok(self.shutdown_member_interruption(ensemble, member));
            }
            let node = ensemble_member_node(&setup.member_nodes, member)?;
            let resume_candidate = (offset == 0)
                .then(|| setup.initial_resume_candidate.clone())
                .flatten();
            let (execution, member_had_no_verdict) = self
                .run_ensemble_member(
                    lp,
                    spec,
                    &node,
                    previous_output,
                    iteration,
                    workdir,
                    member.timeout_minutes,
                    ensemble.timeout_minutes.max(0) as u64,
                    node_outputs,
                    all_node_names,
                    resume_candidate,
                    resumable_sessions,
                    &ensemble.mutex,
                )
                .await?;
            if member_had_no_verdict {
                push_round_robin_no_verdict(
                    &mut tried_no_verdict,
                    &ensemble.id,
                    &node,
                    member,
                    &execution,
                );
                continue;
            }
            return self
                .settle_round_robin_winner(RoundRobinSettle {
                    join_ctx: &join_ctx,
                    ensemble,
                    details,
                    member,
                    node: &node,
                    execution,
                    members_ran,
                    skipped_unavailable,
                    next_after_winner: (current_index + 1).rem_euclid(setup.member_count),
                    next_index: setup.next_index,
                    commit_watch: &setup.commit_watch,
                    workdir,
                })
                .await;
        }
        self.settle_round_robin_no_verdict(
            &RoundRobinWalkCtx {
                join_ctx: &join_ctx,
                commit_watch: &setup.commit_watch,
                workdir,
                ensemble,
            },
            details,
            &tried_no_verdict,
            setup.next_index,
        )
        .await
    }

    pub(crate) fn resolve_round_robin_start(
        &self,
        details: &EnsembleDetails,
        member_count: i64,
        round_robin_pins: &mut HashMap<String, i64>,
        resumable_sessions: &HashMap<String, String>,
    ) -> Result<(i64, Option<String>, i64)> {
        let ensemble = &details.ensemble;
        if !round_robin_pins.contains_key(&ensemble.id) {
            return self.resolve_round_robin_first_visit(details, member_count, round_robin_pins);
        }
        self.resolve_round_robin_bounce(details, member_count, round_robin_pins, resumable_sessions)
    }

    pub(crate) fn resolve_round_robin_first_visit(
        &self,
        details: &EnsembleDetails,
        member_count: i64,
        round_robin_pins: &mut HashMap<String, i64>,
    ) -> Result<(i64, Option<String>, i64)> {
        let ensemble = &details.ensemble;
        let persisted_index = self
            .db
            .get_ensemble(&ensemble.id)
            .ok()
            .flatten()
            .and_then(|e| e.round_robin_index)
            .or(ensemble.round_robin_index)
            .unwrap_or(0);
        let start = persisted_index.rem_euclid(member_count);
        let next = (start + 1).rem_euclid(member_count);
        self.db
            .update_ensemble_kind(&ensemble.id, None, Some(Some(next)))?;
        round_robin_pins.insert(ensemble.id.clone(), start);
        Ok((start, None, next))
    }

    pub(crate) fn resolve_round_robin_bounce(
        &self,
        details: &EnsembleDetails,
        member_count: i64,
        round_robin_pins: &mut HashMap<String, i64>,
        resumable_sessions: &HashMap<String, String>,
    ) -> Result<(i64, Option<String>, i64)> {
        let ensemble = &details.ensemble;
        let pinned = round_robin_pins[&ensemble.id];
        let pinned_node_id = details.members[pinned as usize].node_id.clone();
        if let Some(sid) = resumable_sessions.get(&pinned_node_id).cloned() {
            return Ok((pinned, Some(sid), pinned));
        }
        let next = (pinned + 1).rem_euclid(member_count);
        self.db
            .update_ensemble_kind(&ensemble.id, None, Some(Some(next)))?;
        round_robin_pins.insert(ensemble.id.clone(), next);
        Ok((next, None, next))
    }

    pub(crate) async fn record_ensemble_join(
        &self,
        ctx: &EnsembleJoinCtx<'_>,
        execution: &NodeExecution,
    ) -> Result<String> {
        let join_run_id = uuid::Uuid::new_v4().to_string();
        self.db.insert_graph_run(&GraphNodeRun {
            id: join_run_id.clone(),
            graph_id: ctx.lp.id.clone(),
            spec_id: ctx.spec.id.clone(),
            node_id: ctx.join_node_id.to_string(),
            status: execution.status,
            input: ctx.input.cloned(),
            output: Some(execution.output.clone()),
            started_at: chrono::Utc::now(),
            completed_at: Some(chrono::Utc::now()),
            iteration: ctx.iteration as i64,
            pid: None,
            boot_id: crate::system::boot_id(),
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })?;
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &ctx.lp.id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&ctx.spec.id),
                node_id: Some(ctx.join_node_id),
                run_id: Some(&join_run_id),
                kind: activity::GraphEventKind::NodeFinished,
                message: execution.summary.clone(),
                payload: Some(serde_json::json!({
                    "verdict": execution.status.as_str(),
                    "platform": Value::Null,
                    "model": Value::Null,
                    "duration_s": Value::Null,
                })),
            },
        );
        Ok(join_run_id)
    }

    /// CB82 loop-head guard shared by the cascade/round-robin walks: settle
    /// the member's active run as interrupted if it has one, and build the
    /// interrupted execution — no join row is written, no advance happens.
    pub(crate) fn shutdown_interrupted_member_execution(
        &self,
        node_id: &str,
        summary: String,
    ) -> NodeExecution {
        if let Ok(Some(active)) = self.db.get_active_graph_run_for_node(node_id) {
            let _ = self.db.update_graph_run_result(
                &active.id,
                GraphRunStatus::Fail,
                Some(&shutdown_interrupted_output()),
                Some(chrono::Utc::now()),
            );
        }
        interrupted_execution(summary)
    }

    /// CB82: stamp the latest join row for `details` as interrupted (it was
    /// written by the ensemble walk just before the shutdown flag was seen).
    /// Member rows need no marker: the ensemble resume cursor is the join.
    pub(crate) fn stamp_ensemble_join_interrupted(
        &self,
        spec: &GraphSpec,
        details: &EnsembleDetails,
    ) -> Result<()> {
        let join_id = details.ensemble.join_node_id.as_str();
        let join = self
            .db
            .list_graph_runs_for_spec(&spec.id)?
            .into_iter()
            .rfind(|run| run.node_id == join_id);
        if let Some(join) = join {
            let _ = self.db.update_graph_run_result(
                &join.id,
                GraphRunStatus::Fail,
                Some(&shutdown_interrupted_output()),
                Some(chrono::Utc::now()),
            );
        }
        Ok(())
    }
}

/// The graph node for an ensemble member, or an error naming the member when
/// the node is missing.
fn ensemble_member_node(
    nodes: &HashMap<String, GraphNode>,
    member: &EnsembleMember,
) -> Result<GraphNode> {
    nodes
        .get(&member.node_id)
        .cloned()
        .ok_or_else(|| anyhow!("Ensemble member node '{}' not found.", member.node_id))
}
