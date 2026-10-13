use super::*;

/// Borrowed inputs for spawning all parallel ensemble members.
pub(crate) struct ParallelSpawnCtx<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) spec: &'a GraphSpec,
    pub(crate) details: &'a EnsembleDetails,
    pub(crate) member_nodes: &'a HashMap<String, GraphNode>,
    pub(crate) previous_output: Option<&'a Value>,
    pub(crate) workdir: &'a str,
    pub(crate) iteration: usize,
    pub(crate) node_outputs: &'a HashMap<String, Value>,
    pub(crate) all_node_names: &'a [String],
    pub(crate) resumable_sessions: &'a HashMap<String, String>,
    pub(crate) straggler_minutes: u64,
}

impl GraphEngine {
    /// Run an ensemble's members concurrently (F1), wait for every one of
    /// them to terminate (pass, fail, or straggler timeout — never early),
    /// and consolidate their outputs into the join's own [`NodeExecution`].
    ///
    /// Every member receives the exact same `previous_output` (the same
    /// input, in parallel — the defining shape of an ensemble). Concurrency
    /// is bounded by `self.ensemble_concurrency`, a semaphore shared across
    /// every graph this engine drives, so an 8-member ensemble queues past the
    /// cap rather than spawning all 8 processes at once.
    #[expect(clippy::too_many_arguments)]
    pub(crate) async fn execute_ensemble(
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
        round_robin_pins: &mut HashMap<String, i64>,
    ) -> Result<NodeExecution> {
        let ensemble = &details.ensemble;
        match ensemble.kind {
            EnsembleKind::Parallel => {}
            EnsembleKind::Cascade => {
                return self
                    .execute_ensemble_cascade(
                        lp,
                        spec,
                        details,
                        previous_output,
                        iteration,
                        workdir,
                        enforce_commit_rights,
                        node_outputs,
                        all_node_names,
                        resumable_sessions,
                    )
                    .await;
            }
            EnsembleKind::RoundRobin => {
                return self
                    .execute_ensemble_round_robin(
                        lp,
                        spec,
                        details,
                        previous_output,
                        iteration,
                        workdir,
                        enforce_commit_rights,
                        node_outputs,
                        all_node_names,
                        resumable_sessions,
                        round_robin_pins,
                    )
                    .await;
            }
        }
        // 0 is a legitimate value (mirrors `run_agent_process`'s own
        // `timeout_minutes`) — minute-granular timeouts otherwise have no way
        // to force an immediate one in a fast test.
        let straggler_minutes = ensemble.effective_straggler_timeout_minutes().max(0) as u64;

        let member_nodes = load_ensemble_member_nodes(&self.db, details)?;

        // B37: members run concurrently against one workdir, so a moved HEAD
        // cannot be attributed to a single member — enforcement is therefore
        // at ensemble granularity, and the quorum fails as a whole. Skipped
        // when the ensemble itself (CM28) is the graph's designated committer.
        let any_member_may_commit = details.ensemble.commit_rights;
        let commit_watch =
            CommitRightsWatch::begin(enforce_commit_rights, any_member_may_commit, workdir).await;

        let spawn_ctx = ParallelSpawnCtx {
            lp,
            spec,
            details,
            member_nodes: &member_nodes,
            previous_output,
            workdir,
            iteration,
            node_outputs,
            all_node_names,
            resumable_sessions,
            straggler_minutes,
        };
        let mut set = self.spawn_parallel_members(&spawn_ctx)?;

        let quorum_grace: Option<i64> = ensemble.quorum_grace_minutes;
        let collect_ctx = ParallelCollectCtx {
            lp,
            ensemble,
            details,
            member_nodes: &member_nodes,
            workdir,
        };
        let (mut results, quorum_met_at) = self
            .collect_ensemble_results(&collect_ctx, &mut set, quorum_grace)
            .await?;

        let tally =
            consolidate_parallel_results(details, &member_nodes, &mut results, resumable_sessions)?;
        let finish_ctx = ParallelFinishCtx {
            lp,
            spec,
            ensemble,
            details,
            workdir,
            iteration,
            input: previous_output,
            quorum_met_at,
            quorum_grace,
            commit_watch: &commit_watch,
        };
        if tally.verdicts == 0 {
            return self.finish_no_verdict_ensemble(&finish_ctx, tally).await;
        }
        self.finish_quorum_ensemble(&finish_ctx, tally).await
    }

    pub(crate) fn spawn_parallel_members(
        &self,
        ctx: &ParallelSpawnCtx<'_>,
    ) -> Result<tokio::task::JoinSet<MemberJoinResult>> {
        let mut set = tokio::task::JoinSet::new();
        for member in &ctx.details.members {
            let node = round_robin_member_node(ctx.member_nodes, member)?;
            let run_spec = MemberRunSpec {
                lp: ctx.lp,
                spec: ctx.spec,
                node: &node,
                member,
                previous_output: ctx.previous_output,
                iteration: ctx.iteration,
                ensemble_id: &ctx.details.ensemble.id,
            };
            let run_id = self.insert_ensemble_member_run(&run_spec)?;
            // CM46: parallel members record `node_started` here, so every
            // `node_finished` they later produce (normal finish, quorum kill,
            // straggler timeout) has its start in the log — the sequential
            // path records its start in `run_ensemble_member`.
            activity::record(
                &self.db,
                activity::GraphEvent {
                    graph_id: &ctx.lp.id,
                    graph_name: &ctx.lp.name,
                    workdir: ctx.workdir,
                    spec_id: Some(&ctx.spec.id),
                    node_id: Some(&node.id),
                    run_id: Some(&run_id),
                    kind: activity::GraphEventKind::NodeStarted,
                    message: format!("Node '{}' started.", node.name),
                    payload: None,
                },
            );
            let task = ParallelMemberTask {
                db: Arc::clone(&self.db),
                lp: ctx.lp.clone(),
                spec: ctx.spec.clone(),
                node,
                previous_output: ctx.previous_output.cloned(),
                workdir: ctx.workdir.to_string(),
                semaphore: Arc::clone(&self.ensemble_concurrency),
                label: member_label(member),
                ensemble_id: ctx.details.ensemble.id.clone(),
                dynamic_skills: self.dynamic_skills.clone(),
                node_outputs: ctx.node_outputs.clone(),
                all_node_names: ctx.all_node_names.to_vec(),
                resume_candidate_seed: ctx.resumable_sessions.get(&member.node_id).cloned(),
                run_id,
                iteration: ctx.iteration,
                straggler_minutes: ctx.straggler_minutes,
            };
            set.spawn(run_parallel_member_task(task));
        }
        Ok(set)
    }

    pub(crate) async fn collect_ensemble_results(
        &self,
        ctx: &ParallelCollectCtx<'_>,
        set: &mut tokio::task::JoinSet<MemberJoinResult>,
        quorum_grace: Option<i64>,
    ) -> Result<(
        HashMap<String, (String, NodeExecution, Option<String>, bool)>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> {
        let Some(grace) = quorum_grace else {
            return Ok((drain_join_set(set).await?, None));
        };
        self.collect_with_quorum_grace(ctx, set, grace).await
    }

    pub(crate) async fn collect_with_quorum_grace(
        &self,
        ctx: &ParallelCollectCtx<'_>,
        set: &mut tokio::task::JoinSet<MemberJoinResult>,
        grace: i64,
    ) -> Result<(
        HashMap<String, (String, NodeExecution, Option<String>, bool)>,
        Option<chrono::DateTime<chrono::Utc>>,
    )> {
        let mut results = HashMap::new();
        let mut quorum_met_at: Option<chrono::DateTime<chrono::Utc>> = None;
        let mut passed_so_far = 0i64;
        let mut grace_deadline: Option<tokio::time::Instant> = None;
        loop {
            if results.len() >= ctx.details.members.len() {
                break;
            }
            let completed = self.await_next_member(set, &grace_deadline).await?;
            let Some(completed) = completed else {
                break;
            };
            record_completed_member(
                &mut results,
                &mut passed_so_far,
                Some(completed),
                ctx.ensemble.min_pass,
                grace,
                &mut quorum_met_at,
                &mut grace_deadline,
            );
            if grace_deadline.is_some() && results.len() >= ctx.details.members.len() {
                break;
            }
        }
        if self.terminate_members_after_quorum(ctx, &mut results) {
            set.abort_all();
            while set.join_next().await.is_some() {}
        } else {
            let drained = drain_join_set(set).await?;
            for (node_id, entry) in drained {
                results.insert(node_id, entry);
            }
        }
        Ok((results, quorum_met_at))
    }

    pub(crate) async fn await_next_member(
        &self,
        set: &mut tokio::task::JoinSet<MemberJoinResult>,
        grace_deadline: &Option<tokio::time::Instant>,
    ) -> Result<Option<MemberJoinResult>> {
        let Some(deadline) = grace_deadline else {
            return match set.join_next().await {
                Some(done) => {
                    Ok(Some(done.map_err(|error| {
                        anyhow!("Ensemble member task panicked: {error}")
                    })?))
                }
                None => Ok(None),
            };
        };
        tokio::select! {
            biased;
            joined = set.join_next() => match joined {
                Some(done) => Ok(Some(
                    done.map_err(|error| anyhow!("Ensemble member task panicked: {error}"))?,
                )),
                None => Ok(None),
            },
            _ = tokio::time::sleep_until(*deadline) => Ok(None),
        }
    }

    pub(crate) fn terminate_members_after_quorum(
        &self,
        ctx: &ParallelCollectCtx<'_>,
        results: &mut HashMap<String, (String, NodeExecution, Option<String>, bool)>,
    ) -> bool {
        let mut terminated_any = false;
        for member in &ctx.details.members {
            if results.contains_key(&member.node_id) {
                continue;
            }
            terminated_any = true;
            if let Ok(Some(run)) = self.db.get_active_graph_run_for_node(&member.node_id) {
                let workdir = crate::graph_engine::routing::workdir_for_run(&self.db, &run);
                terminate_run_row(&self.db, &run, "quorum met", &workdir);
            }
            let node_name = ctx
                .member_nodes
                .get(&member.node_id)
                .map(|node| node.name.clone())
                .unwrap_or_else(|| member.node_id.clone());
            results.insert(
                member.node_id.clone(),
                (
                    member_label(member),
                    NodeExecution {
                        status: GraphRunStatus::Fail,
                        output: serde_json::json!({
                            "terminated": true,
                            "reason": "quorum met",
                            "kind": "agent",
                            "node_id": member.node_id,
                        }),
                        summary: format!("Ensemble member '{node_name}' terminated: quorum met."),
                    },
                    None,
                    true,
                ),
            );
            activity::record(
                &self.db,
                activity::GraphEvent {
                    graph_id: &ctx.lp.id,
                    graph_name: &ctx.lp.name,
                    workdir: ctx.workdir,
                    spec_id: None,
                    node_id: Some(&member.node_id),
                    run_id: None,
                    kind: activity::GraphEventKind::NodeFinished,
                    message: format!("Ensemble member '{node_name}' failed: quorum met."),
                    payload: Some(serde_json::json!({
                        "verdict": GraphRunStatus::Fail.as_str(),
                        "platform": Value::Null,
                        "model": Value::Null,
                        "duration_s": Value::Null,
                    })),
                },
            );
        }
        terminated_any
    }

    pub(crate) async fn finish_no_verdict_ensemble(
        &self,
        ctx: &ParallelFinishCtx<'_>,
        tally: Consolidation,
    ) -> Result<NodeExecution> {
        let execution = quorum_no_verdict_execution(
            ctx.ensemble,
            ctx.details,
            &tally,
            ctx.quorum_met_at,
            ctx.quorum_grace,
        );
        let execution = apply_parallel_commit_watch(
            execution,
            ctx.commit_watch,
            ctx.workdir,
            &ctx.ensemble.name,
            &ctx.ensemble.join_node_id,
        )
        .await;
        let join_run_id = uuid::Uuid::new_v4().to_string();
        self.db.insert_graph_run(&GraphNodeRun {
            id: join_run_id.clone(),
            graph_id: ctx.lp.id.clone(),
            spec_id: ctx.spec.id.clone(),
            node_id: ctx.ensemble.join_node_id.clone(),
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
                node_id: Some(&ctx.ensemble.join_node_id),
                run_id: Some(&join_run_id),
                kind: activity::GraphEventKind::NodeFinished,
                message: format!(
                    "Ensemble '{}' produced no verdict ({}/{} members, all infrastructure failures).",
                    ctx.ensemble.name,
                    ctx.details.members.len(),
                    ctx.details.members.len()
                ),
                payload: Some(serde_json::json!({
                    "verdict": execution.status.as_str(),
                    "platform": Value::Null,
                    "model": Value::Null,
                    "duration_s": Value::Null,
                })),
            },
        );
        Ok(execution)
    }

    pub(crate) async fn finish_quorum_ensemble(
        &self,
        ctx: &ParallelFinishCtx<'_>,
        tally: Consolidation,
    ) -> Result<NodeExecution> {
        let execution = quorum_execution(
            ctx.ensemble,
            ctx.details,
            &tally,
            ctx.quorum_met_at,
            ctx.quorum_grace,
        );
        let execution = apply_parallel_commit_watch(
            execution,
            ctx.commit_watch,
            ctx.workdir,
            &ctx.ensemble.name,
            &ctx.ensemble.join_node_id,
        )
        .await;
        let join_ctx = EnsembleJoinCtx {
            lp: ctx.lp,
            spec: ctx.spec,
            join_node_id: &ctx.ensemble.join_node_id,
            workdir: ctx.workdir,
            iteration: ctx.iteration,
            input: ctx.input,
        };
        self.record_ensemble_join(&join_ctx, &execution).await?;
        Ok(execution)
    }

    /// CM3/CB67: run a single ensemble member to a verdict, honouring its own
    /// infra-retry budget (`begin_infra_retry`). Bounded only by ITS OWN
    /// effective timeout — `member_timeout_minutes`, or else
    /// `ensemble.timeout_minutes` (never `straggler_timeout_minutes`, which is
    /// a parallel-quorum concept with no meaning for a sequential walk) — and
    /// CB67: every attempt, the initial one AND every infra retry, gets that
    /// full timeout FRESH, timed from that attempt's own start; no ensemble-
    /// wide clock is ever armed here. Returns the member's `NodeExecution`
    /// plus a `had_no_verdict` flag that is `true` only when the member
    /// produced no usable result — a retry-exhausted infra crash, a hard
    /// error, or a timeout kill. Cascade and round-robin share this so
    /// "advance to the next member only on no verdict" has exactly one
    /// implementation; a real `fail` verdict returns `had_no_verdict == false`
    /// and must stop the walk.
    pub(crate) fn insert_ensemble_member_run(
        &self,
        run_spec: &MemberRunSpec<'_>,
    ) -> Result<String> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let (executed_platform, executed_model) = executed_pair_for_platform_model(
            Some(run_spec.member.platform.as_str()),
            run_spec.member.model.as_deref(),
        );
        self.db.insert_graph_run(&GraphNodeRun {
            id: run_id.clone(),
            graph_id: run_spec.lp.id.clone(),
            spec_id: run_spec.spec.id.clone(),
            node_id: run_spec.node.id.clone(),
            status: GraphRunStatus::Running,
            input: run_spec.previous_output.cloned(),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: run_spec.iteration as i64,
            pid: None,
            boot_id: crate::system::boot_id(),
            session_id: None,
            executed_platform,
            executed_model,
        })?;
        let member_platform = run_spec.member.platform.as_str();
        let member_model = run_spec.member.model.as_deref().unwrap_or("");
        tracing::info!(
            graph_id = %run_spec.lp.id,
            spec_id = %run_spec.spec.id,
            node_id = %run_spec.node.id,
            node = %run_spec.node.name,
            run_id = %run_id,
            platform = %member_platform,
            model = %member_model,
            ensemble_id = %run_spec.ensemble_id,
            "ensemble member run launched"
        );
        Ok(run_id)
    }
}
