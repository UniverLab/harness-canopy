use super::*;

/// Owned setup for one node dispatch attempt.
pub(crate) struct NodeAttemptSetup {
    pub(crate) node: GraphNode,
    pub(crate) resume_candidate: Option<String>,
    pub(crate) resume_crosses_spec: bool,
    pub(crate) run_id: String,
    pub(crate) commit_watch: Option<CommitRightsWatch>,
    pub(crate) committer_head_before: Option<String>,
    /// CM49: the crew this node is bound to, if any, and the index of the
    /// member resolved at dispatch (the rotation's starting point on an
    /// infra retry).
    pub(crate) crew: Option<String>,
    pub(crate) crew_index: Option<usize>,
}
/// Mutable retry state for one node dispatch, borrowed by the retry driver.
pub(crate) struct NodeRetryCtx<'a> {
    pub(crate) node: &'a GraphNode,
    pub(crate) previous_output: Option<&'a Value>,
    pub(crate) run_id: String,
    pub(crate) resume_candidate: Option<String>,
    pub(crate) resume_crosses_spec: bool,
    pub(crate) iteration_value: usize,
    /// CM49: crew binding for this dispatch, and the index last resolved —
    /// an infra retry rotates past it (wrapping).
    pub(crate) crew: Option<String>,
    pub(crate) crew_index: Option<usize>,
}

impl GraphEngine {
    pub(crate) async fn resolve_node_resume(
        &self,
        ctx: &SpecStepCtx<'_>,
        resumable_sessions: &HashMap<String, String>,
        node_id: &str,
    ) -> Result<(Option<String>, bool)> {
        // RS2: resume candidate for this attempt. On the first
        // visit to a node the map is empty → `None` → cold start.
        // On a fail-edge bounce it holds the session captured on the
        // node's previous run in this dispatch → resume.
        let mut resume_candidate = resumable_sessions.get(node_id).cloned();

        // RS3/R1: whether `resume_candidate` (once populated below)
        // crosses a spec boundary — i.e. was captured by a DIFFERENT
        // spec, not this one. `group_session_for_node` only ever
        // returns a session belonging to an earlier-positioned
        // sibling (see its query), so any hit from it is a
        // cross-spec resume by construction; the RS2 in-dispatch map
        // above is always this spec's own session, so a hit there
        // never is. This is the ONE thing `render_resume_prompt`
        // needs to know to decide whether the new spec has ever
        // been shown to the resumed session.
        let mut resume_crosses_spec = false;

        // RS3 group-session handoff: a grouped spec's FIRST visit to
        // this node (nothing yet in `resumable_sessions` for it)
        // resumes the group's live session for this node — the
        // session captured by the previous successfully-completed
        // grouped sibling on the same node. Derived from the DB so a
        // daemon restart mid-queue keeps group context. Taint is
        // enforced inside `group_session_for_node`: a failed nearest
        // sibling yields `None` here, so this spec cold-starts and
        // its fresh session becomes the group's new session. Bounces
        // (map already populated) keep RS2's in-dispatch session and
        // never re-consult the group.
        if resume_candidate.is_none() {
            if let (Some(group), Some(pid)) = (ctx.spec_group, ctx.queue_id) {
                resume_candidate =
                    self.db
                        .group_session_for_node(pid, group, &ctx.spec.id, node_id)?;
                resume_crosses_spec = resume_candidate.is_some();
            }
        }
        Ok((resume_candidate, resume_crosses_spec))
    }

    pub(crate) async fn prepare_node_attempt(
        &self,
        ctx: &SpecStepCtx<'_>,
        resumable_sessions: &HashMap<String, String>,
        node_id: &str,
        previous_output: Option<&Value>,
        iteration_value: usize,
    ) -> Result<NodeAttemptSetup> {
        // CM15: the dispatched node's CONFIG (platform, model, effort,
        // prompt, timeout, commit_rights, resume — everything read by
        // execute_agent_node / execute_check_node / execute_router_node)
        // is read from the database HERE, at the moment of dispatch —
        // not from the per-run `nodes` snapshot. So a graph_update_node
        // applied between two dispatches of this node lands on the
        // second dispatch (FR1). This read happens exactly once, before
        // the run row below is created, and the same `node` value is
        // reused for every infra retry of this dispatch, so a config
        // change never disturbs a run already in flight (FR3). The run
        // record still names the platform/model actually used because
        // execute_agent_node derives them from this same `node` (FR4).
        // `nodes_by_id` (the launch snapshot) stays authoritative for
        // graph TOPOLOGY — kind, position, edges, router routes — which
        // graph_update_node / graph_update_ensemble refuse to change while
        // the graph is `running` (FR5).
        let node_fresh = self
            .db
            .get_graph_node(node_id)
            .map_err(|e| anyhow!("Graph node '{}' lookup failed: {e}", node_id))?
            .ok_or_else(|| anyhow!("Graph node '{}' not found.", node_id))?;
        let mut node = node_fresh;
        // CM49: a crew-bound agent node resolves its (platform, model) from the
        // crew here, at dispatch, so the run row records the RESOLVED pair
        // (`executed_pair_for_node` reads this config) and the node never names
        // a harness. `node_availability_blocker` already refused a missing crew
        // or one with no available member, so a pair is expected here.
        let mut crew_bound: Option<(String, usize)> = None;
        if let Some(crew) = node
            .config
            .get("crew")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
        {
            if let Some((platform, model, index)) = self.db.resolve_crew_pair(&crew, None)? {
                inject_crew_pair(&mut node, platform, model);
                crew_bound = Some((crew, index));
            }
        }
        let (resume_candidate, resume_crosses_spec) = self
            .resolve_node_resume(ctx, resumable_sessions, node_id)
            .await?;
        let run_id = uuid::Uuid::new_v4().to_string();
        // CB43: record the pair resolved at dispatch, on the row
        // itself — never re-derived from the node config later.
        let (executed_platform, executed_model) = executed_pair_for_node(&node);
        // A node that declares mutexes starts `waiting` — `run_node_with_infra_retries`
        // flips it to `running` at the grant, which is also when its timeout starts.
        let initial_status = if node_mutex_resources(&node).is_empty() {
            GraphRunStatus::Running
        } else {
            GraphRunStatus::Waiting
        };
        self.db.insert_graph_run(&GraphNodeRun {
            id: run_id.clone(),
            graph_id: ctx.lp.id.clone(),
            spec_id: ctx.spec.id.clone(),
            node_id: node.id.clone(),
            status: initial_status,
            input: previous_output.cloned(),
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: iteration_value as i64,
            pid: None,
            boot_id: crate::system::boot_id(),
            session_id: None,
            executed_platform,
            executed_model,
        })?;

        {
            let node_platform = node
                .config
                .get("platform")
                .or_else(|| node.config.get("cli"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty());
            let node_model = node.config.get("model").and_then(Value::as_str);
            tracing::info!(
                graph_id = %ctx.lp.id,
                spec_id = %ctx.spec.id,
                node_id = %node.id,
                node = %node.name,
                run_id = %run_id,
                platform = node_platform.unwrap_or(""),
                model = node_model.unwrap_or(""),
                "node run launched"
            );
        }

        // B37: baseline HEAD for this node's whole visit, infra
        // retries included — a retry that commits is as much a
        // violation as a first attempt that does.
        let commit_watch = CommitRightsWatch::begin(
            ctx.enforce_commit_rights,
            node_has_commit_rights(&node, ctx.ensembles),
            ctx.workdir,
        )
        .await;

        // C15: mirror of the watch above, but for the node the
        // graph actually trusts to commit rather than every node
        // that must not. Captured only when this node carries
        // `commit_rights: true` — otherwise there is nothing to
        // attribute a HEAD move to, so `spec_committed_head`
        // stays whatever it already was.
        let committer_head_before = if node_has_commit_rights(&node, ctx.ensembles) {
            capture_workdir_head(ctx.workdir).await
        } else {
            None
        };

        let (crew, crew_index) = match crew_bound {
            Some((crew, index)) => (Some(crew), Some(index)),
            None => (None, None),
        };
        Ok(NodeAttemptSetup {
            node,
            resume_candidate,
            resume_crosses_spec,
            run_id,
            commit_watch,
            committer_head_before,
            crew,
            crew_index,
        })
    }

    /// A clone of `base` with the resolved crew pair injected (`platform` +
    /// `model`), used to run a crew-bound node on a specific member.
    fn crew_node_with_pair(
        &self,
        base: &GraphNode,
        platform: String,
        model: Option<String>,
    ) -> GraphNode {
        let mut clone = base.clone();
        inject_crew_pair(&mut clone, platform, model);
        clone
    }

    pub(crate) async fn run_node_with_infra_retries(
        &self,
        ctx: &SpecStepCtx<'_>,
        state: &SpecRunState,
        retry: &mut NodeRetryCtx<'_>,
    ) -> Result<(NodeExecution, GraphNodeRun, bool)> {
        let mut chain = AttemptChain::from_node(retry.node);
        // CM49: for a crew-bound node the pair for the NEXT attempt is chosen
        // as soon as the current attempt infra-fails, so the retry run row
        // records the member it will actually run. Cached here and consumed at
        // the top of the next iteration. A crew with no available member stops
        // the retries and surfaces the block naming the crew.
        let mut pending: Option<GraphNode> = None;
        loop {
            let node_owned: GraphNode = match retry.crew.clone() {
                Some(crew) => {
                    match self.crew_attempt_node(&crew, retry, &mut pending, chain.attempt)? {
                        Some(next) => next,
                        None => return self.crew_exhausted_return(retry, &crew),
                    }
                }
                None => retry.node.clone(),
            };
            let node: &GraphNode = &node_owned;
            if let Some(stopped) = self.acquire_run_mutexes(ctx, node, &retry.run_id).await? {
                return Ok(stopped);
            }
            let execution = self
                .execute_node(
                    ctx.lp,
                    ctx.spec,
                    node,
                    retry.previous_output,
                    ctx.spec_start_head,
                    state.spec_committed_head.as_deref(),
                    &retry.run_id,
                    ctx.workdir,
                    retry.resume_candidate.as_deref(),
                    retry.resume_crosses_spec,
                    chain.nudged,
                    &state.node_outputs,
                    ctx.all_node_names,
                )
                .await?;
            let run = self.db.get_graph_run(&retry.run_id)?.ok_or_else(|| {
                anyhow!("Graph run '{}' not found after execution.", retry.run_id)
            })?;

            match chain.decide(node, &execution, &run) {
                AttemptRetry::Settle => {
                    // CM2: the graph is settling on this attempt (no more
                    // retries). Route `Error` only if this final attempt is
                    // itself a no-verdict infra crash — a retry that
                    // recovered (Pass) or a genuine negative verdict (Fail)
                    // must NOT take the `Error` edge.
                    let mut execution = execution;
                    let mut run = run;
                    let had_infra_crash = finalize_infra_crash_shape(
                        &self.db,
                        &retry.run_id,
                        node,
                        &mut execution,
                        &mut run,
                        chain.attempt,
                        chain.retry_limit,
                        chain.crash_max_secs,
                    )
                    .await?;

                    return Ok((execution, run, had_infra_crash));
                }
                kind => {
                    // CB82: a shutdown kill must take no edge at all (via the
                    // run_spec/spec_step guards) — never the `Error` edge. Settle
                    // the attempt as interrupted without inserting a new retry row.
                    if self.is_shutting_down() {
                        return self.settle_interrupted_attempt(&retry.run_id, node);
                    }
                    // B19: retry resuming the crashed attempt's own
                    // session if it managed to create one before dying;
                    // an infra crash at spawn usually created none, so
                    // this is normally `None` → the retry cold-starts.
                    // If the crashed attempt was itself a cross-spec
                    // (RS3) resume and it continued the SAME foreign
                    // session before dying, the retry is still that
                    // same cross-spec resume; any other outcome (a
                    // fresh session captured on cold fallback, or none
                    // at all) means the retry is not, and the next
                    // `execute_node` call renders normally for
                    // whichever case it's in.
                    let new_candidate = run.session_id.clone();
                    retry.resume_crosses_spec = retry.resume_crosses_spec
                        && new_candidate.is_some()
                        && new_candidate == retry.resume_candidate;
                    retry.resume_candidate = new_candidate;
                    // CM49: pick the next crew member for an infra retry
                    // now, so the retry run row records the pair it will
                    // run. A reporting nudge never rotates: it re-runs
                    // the same member (and resumes its session), so the
                    // current node is cached as-is for the next attempt.
                    if kind == AttemptRetry::InfraCrash {
                        self.pick_next_crew_member(retry, &mut pending)?;
                    } else if retry.crew.is_some() {
                        pending = Some(node.clone());
                    }
                    let retry_node = pending.as_ref().unwrap_or(node);
                    let retry_mutex = node_mutex_resources(retry_node);
                    retry.run_id = begin_infra_retry(
                        &self.db,
                        ctx.lp,
                        ctx.spec,
                        retry_node,
                        retry.previous_output,
                        retry.iteration_value as i64,
                        &retry.run_id,
                        &execution.output,
                        chain.attempt,
                        chain.backoff_secs,
                        &retry_mutex,
                    )
                    .await?;
                    chain.advance(kind);
                    continue;
                }
            }
        }
    }

    /// CM49: the node for one attempt of a crew-bound run — the cached
    /// `pending` member if one was chosen when the previous attempt
    /// infra-failed, else the next available crew member. `None` when no
    /// member is available.
    fn crew_attempt_node(
        &self,
        crew: &str,
        retry: &mut NodeRetryCtx<'_>,
        pending: &mut Option<GraphNode>,
        attempt: u32,
    ) -> Result<Option<GraphNode>> {
        if let Some(next) = pending.take() {
            return Ok(Some(next));
        }
        let after = if attempt == 0 { None } else { retry.crew_index };
        match self.db.resolve_crew_pair(crew, after)? {
            Some((platform, model, index)) => {
                retry.crew_index = Some(index);
                Ok(Some(self.crew_node_with_pair(retry.node, platform, model)))
            }
            None => Ok(None),
        }
    }

    /// CM49: pick the next crew member for the retry, caching it in `pending`
    /// so the retry run row records the pair it will run.
    fn pick_next_crew_member(
        &self,
        retry: &mut NodeRetryCtx<'_>,
        pending: &mut Option<GraphNode>,
    ) -> Result<()> {
        let Some(crew) = retry.crew.clone() else {
            return Ok(());
        };
        if let Some((platform, model, index)) =
            self.db.resolve_crew_pair(&crew, retry.crew_index)?
        {
            retry.crew_index = Some(index);
            *pending = Some(self.crew_node_with_pair(retry.node, platform, model));
        }
        Ok(())
    }

    /// CM49: the terminal return for a crew-bound run whose crew has no
    /// available member — the run row as it stands, and the exhausted
    /// execution naming the crew.
    fn crew_exhausted_return(
        &self,
        retry: &NodeRetryCtx<'_>,
        crew: &str,
    ) -> Result<(NodeExecution, GraphNodeRun, bool)> {
        let owner = format!("node '{}'", retry.node.name);
        let run = self
            .db
            .get_graph_run(&retry.run_id)?
            .ok_or_else(|| anyhow!("Graph run '{}' not found after execution.", retry.run_id))?;
        Ok((
            super::crew_resolve::crew_exhausted_execution(&owner, crew),
            run,
            true,
        ))
    }

    /// The mutex gate ahead of one execution (initial attempt or infra
    /// retry): the run waits in `waiting` until its full set is granted, and
    /// only then starts its timeout. Returns `None` once granted (the caller
    /// executes), or `Some` early-return triple when the wait was refused
    /// (interrupt, pause, shutdown, dead run) — ending the dispatch the way a
    /// shutdown kill does. The settled row underneath tells `run_spec`
    /// whether this was bookkeeping or a verdict.
    async fn acquire_run_mutexes(
        &self,
        ctx: &SpecStepCtx<'_>,
        node: &GraphNode,
        run_id: &str,
    ) -> Result<Option<(NodeExecution, GraphNodeRun, bool)>> {
        let mutex_resources = node_mutex_resources(node);
        if acquire_mutexes_before_run(
            &self.db,
            &self.shutdown,
            ctx.lp,
            &ctx.spec.id,
            node,
            run_id,
            &mutex_resources,
        )
        .await?
        {
            return Ok(None);
        }
        let run = self.db.get_graph_run(run_id)?.ok_or_else(|| {
            anyhow!("Graph run '{run_id}' was deleted while waiting for its mutex.")
        })?;
        Ok(Some((
            interrupted_execution(format!(
                "Node '{}' stopped while waiting for its mutex.",
                node.name
            )),
            run,
            false,
        )))
    }

    /// CB82: settle an attempt killed by daemon shutdown — record it as
    /// interrupted without inserting a new retry row, and return the run row
    /// with `false` (no infra crash shape).
    fn settle_interrupted_attempt(
        &self,
        run_id: &str,
        node: &GraphNode,
    ) -> Result<(NodeExecution, GraphNodeRun, bool)> {
        let _ = self.db.update_graph_run_result(
            run_id,
            GraphRunStatus::Fail,
            Some(&shutdown_interrupted_output()),
            Some(chrono::Utc::now()),
        );
        let run = self
            .db
            .get_graph_run(run_id)?
            .ok_or_else(|| anyhow!("Graph run '{}' not found after execution.", run_id))?;
        Ok((
            interrupted_execution(format!(
                "Node '{}' interrupted: daemon shutdown.",
                node.name
            )),
            run,
            false,
        ))
    }

    pub(crate) async fn load_spec_graph<'a>(
        &self,
        lp: &Graph,
        spec: &GraphSpec,
        spec_details: &'a GraphSpecDetails,
        graph_nodes: &'a [GraphNode],
        graph_edges: &'a [GraphEdge],
        workdir: &str,
    ) -> Result<SpecGraphLoad<'a>> {
        // A spec with its own graph always uses it (full backwards
        // compatibility). Only a spec with no nodes of its own falls back to
        // the top-level graph. Ensembles (F1) follow the exact same
        // precedence — a spec-level ensemble only exists when the spec has
        // its own graph, so it is fetched alongside it.
        if !spec_details.nodes.is_empty() {
            let ensembles = self.db.list_ensembles_for_spec(&spec.id)?;
            Ok(SpecGraphLoad::Ready {
                nodes: &spec_details.nodes,
                edges: &spec_details.edges,
                ensembles,
            })
        } else if !graph_nodes.is_empty() {
            let ensembles = self.db.list_ensembles_for_graph(&lp.id)?;
            Ok(SpecGraphLoad::Ready {
                nodes: graph_nodes,
                edges: graph_edges,
                ensembles,
            })
        } else {
            let summary = format!(
                "Spec '{}' has no nodes of its own and graph '{}' has no top-level graph to fall back to.",
                spec.name, lp.name
            );
            self.db.update_graph_spec_status(
                &spec.id,
                GraphSpecStatus::Failed,
                Some(chrono::Utc::now()),
                Some(chrono::Utc::now()),
            )?;
            let (dirty, paths) = self.record_spec_end_dirty(&spec.id, workdir).await;
            let summary = match dirty_tree_note(dirty, &paths) {
                Some(note) => format!("{summary} ({note})"),
                None => summary,
            };
            Ok(SpecGraphLoad::NoNodes(SpecExecutionOutcome::Failed(
                summary,
            )))
        }
    }

    pub(crate) async fn resolve_spec_baseline(
        &self,
        spec: &GraphSpec,
        spec_details: &GraphSpecDetails,
        workdir: &str,
        is_resume: bool,
        directed: bool,
    ) -> Result<(Option<String>, Option<String>)> {
        // B10: capture the workdir's git HEAD once, at the moment the engine
        // starts executing this spec in the *current run attempt*. The
        // persisted value is only ever reused, never re-derived, and only
        // when resuming the same in-flight attempt (`is_resume` with the spec
        // still `Running`); every other case re-captures strictly before any
        // node of this attempt executes.
        //
        // A directed run is a third case. It reuses the spec's recorded
        // start head when one exists — the earlier nodes it skipped already
        // established the baseline — and otherwise captures a fresh one, which
        // the response reports.
        let (spec_start_head, _spec_start_dirty) = if directed {
            match spec_details.spec.spec_start_head.clone() {
                Some(head) => (Some(head), spec_details.spec.spec_start_dirty),
                None => {
                    let head = capture_workdir_head(workdir).await;
                    let dirty = capture_workdir_dirty(workdir).await;
                    self.db
                        .set_graph_spec_start_state(&spec.id, head.as_deref(), dirty)?;
                    (head, dirty)
                }
            }
        } else if is_resume && spec.status == GraphSpecStatus::Running {
            (
                spec_details.spec.spec_start_head.clone(),
                spec_details.spec.spec_start_dirty,
            )
        } else {
            let head = capture_workdir_head(workdir).await;
            let dirty = capture_workdir_dirty(workdir).await;
            self.db
                .set_graph_spec_start_state(&spec.id, head.as_deref(), dirty)?;
            (head, dirty)
        };
        // C15: `spec_committed_head` follows the exact same
        // same-attempt-vs-fresh-attempt rule as `spec_start_head` above.
        // A directed run reuses it for the same reason it reuses the
        // start head.
        let spec_committed_head =
            if directed || (is_resume && spec.status == GraphSpecStatus::Running) {
                spec_details.spec.spec_committed_head.clone()
            } else {
                self.db.set_graph_spec_committed_head(&spec.id, None)?;
                None
            };
        Ok((spec_start_head, spec_committed_head))
    }

    pub(crate) async fn record_spec_end_dirty(
        &self,
        spec_id: &str,
        workdir: &str,
    ) -> (Option<i64>, Vec<String>) {
        let (dirty, paths) = capture_workdir_dirty_with_paths(workdir).await;
        if let Err(e) = self.db.set_graph_spec_end_state(spec_id, dirty, &paths) {
            tracing::warn!(
                "Failed to record spec end dirty state for '{}': {}",
                spec_id,
                e
            );
        }
        (dirty, paths)
    }

    /// A spec that terminates because a FAILING node had no outgoing edge
    /// leaves no blocker anywhere a human can see unless something writes
    /// one — see the module-level defect this closes. Writes a derived
    /// blocker onto the terminating run's `output.blocker`, the exact key
    /// `graph_run_blocker` (daemon/handler.rs) already reads for `graph_list`
    /// and `graph_get`'s `blocked`/`blocker` fields, so no reader needs to
    /// change. Never invoked for a PASSING termination (the normal, correct
    /// end of a spec — see `Check committed`) and never overwrites a
    /// blocker `graph_report_blocker` already recorded, since that text is
    /// more specific than anything derived here.
    pub(crate) fn record_terminal_blocker(
        &self,
        run_id: &str,
        node_name: &str,
        final_execution: &NodeExecution,
    ) -> anyhow::Result<()> {
        if final_execution.output.get("blocker").is_some() {
            return Ok(());
        }
        let blocker = format!(
            "Spec terminated: node '{}' ended '{}' with no outgoing edge for that status. {}",
            node_name,
            final_execution.status.as_str(),
            final_execution.summary,
        );
        let mut output = final_execution.output.clone();
        match output.as_object_mut() {
            Some(map) => {
                map.insert("blocker".to_string(), Value::String(blocker));
            }
            None => output = serde_json::json!({ "blocker": blocker }),
        }
        self.db
            .update_graph_run_result(run_id, final_execution.status, Some(&output), None)?;
        Ok(())
    }

    /// Overwrite (not merge-and-preserve, unlike [`Self::record_terminal_blocker`])
    /// `run_id`'s `output.blocker` with C19's cross-run budget text. Called
    /// only once [`Self::record_spec_attempt`] has confirmed the budget is
    /// actually exceeded, at which point this attempt's blocker is strictly
    /// more informative than whatever dead-end text (if any) is already
    /// there — naming the spec, the attempt count, and the last failure
    /// rather than just this one node.
    pub(crate) fn set_run_blocker(
        &self,
        run_id: &str,
        status: GraphRunStatus,
        output: &Value,
        blocker: &str,
    ) -> Result<()> {
        let mut merged = output.clone();
        match merged.as_object_mut() {
            Some(map) => {
                map.insert("blocker".to_string(), Value::String(blocker.to_string()));
            }
            None => merged = serde_json::json!({ "blocker": blocker }),
        }
        self.db
            .update_graph_run_result(run_id, status, Some(&merged), None)?;
        Ok(())
    }

    /// C19: called from [`Self::run_spec`] every time a spec is about to end
    /// this attempt as `Failed`, with `is_infra_failure` (from
    /// [`execution_is_infra_failure`]) telling it whether the terminating
    /// execution reflects a genuine verdict or an infrastructure failure
    /// that never produced one — an infra failure touches nothing and
    /// returns `None` immediately.
    ///
    /// A genuine failure increments the spec's own persisted
    /// `cross_run_attempts` counter (`graph_specs.cross_run_attempts`) —
    /// unlike the per-node `iterations` map `run_spec` builds fresh on every
    /// call, this survives `graph_reset`, a relaunch, and a daemon restart,
    /// which is the entire point: an unsatisfiable spec must not get a
    /// fresh budget every time an operator resets and relaunches after a
    /// quota failure. Once the count reaches `self.spec_attempt_limit`,
    /// returns `Some(blocker text)` naming the spec, the attempt count, and
    /// `summary` (the last failure) — the caller is responsible for
    /// recording it and converting this attempt's outcome to `Blocked`
    /// instead of `Failed`.
    pub(crate) fn record_spec_attempt(
        &self,
        spec: &GraphSpec,
        summary: &str,
        is_infra_failure: bool,
    ) -> Result<Option<String>> {
        if is_infra_failure {
            return Ok(None);
        }
        let attempts = self.db.increment_graph_spec_cross_run_attempts(&spec.id)?;
        if (attempts as usize) < self.spec_attempt_limit {
            return Ok(None);
        }
        Ok(Some(format!(
            "Spec '{}' failed {} time(s) across separate graph executions (limit {}); last \
             failure: {}. To clear the limit: `graph_reset` with `specs: [{}]` clears this \
             spec's execution count (the worktree is not touched); then `graph_run` or \
             `run_from_node`.",
            spec.name, attempts, self.spec_attempt_limit, summary, spec.id
        )))
    }
}

/// CM49: inject a resolved crew pair into a node's config (`platform`, and
/// `model` — removed when the member carries none).
fn inject_crew_pair(node: &mut GraphNode, platform: String, model: Option<String>) {
    if let Value::Object(map) = &mut node.config {
        map.insert("platform".to_string(), Value::String(platform));
        match model {
            Some(model) => {
                map.insert("model".to_string(), Value::String(model));
            }
            None => {
                map.remove("model");
            }
        }
    }
}
