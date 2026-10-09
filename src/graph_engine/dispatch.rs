use super::*;

/// Action to take after handling a spec execution outcome.
/// Used by `run_graph_dispatch` to reduce nesting and duplication.
pub(crate) enum SpecOutcomeAction {
    Continue,
    Pause, // return Ok(()) from the dispatch
}
/// Immutable inputs shared by every dispatch helper.
pub(crate) struct DispatchCtx<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) graph_id: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) is_resume: bool,
    /// CM43: when set, the first spec this run executes starts at this node
    /// instead of at Start.
    pub(crate) directed: Option<&'a DirectedStart>,
    pub(crate) claimed_at: chrono::DateTime<chrono::Utc>,
}
/// Whether a dispatch helper wants the main loop to keep going or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DispatchFlow {
    Continue,
    Stop,
}

impl GraphEngine {
    /// Core of [`Self::run_graph`], plus the one bit `run_graph`'s public
    /// signature can't carry: whether this call is *resuming* an
    /// already-in-flight run ([`Self::resume_background`], the sole path
    /// behind `graph_continue` and interrupted-queue/autorun resumption) or a
    /// fresh dispatch (`graph_run`, including relaunching a `paused` graph
    /// directly, and the graph's initial launch).
    ///
    /// That distinction is exactly what `{{spec_start_head}}` (B10) needs: a
    /// spec can be left `running` in the DB either because this exact run is
    /// paused mid-node-graph (daemon restart, explicit `graph_pause`) — where
    /// the previously captured baseline is still correct and must be kept —
    /// or because a *prior, distinct* run/launch died without ever being
    /// reset — where reusing that baseline would silently compare against a
    /// HEAD from a different attempt entirely. Only `is_resume = true`
    /// (i.e. only [`Self::resume_background`]) is allowed to reuse it; every
    /// other entry point re-captures, per spec.
    pub(crate) async fn handle_spec_outcome(
        &self,
        outcome: SpecExecutionOutcome,
        spec: &GraphSpec,
        lp: &Graph,
        graph_id: &str,
        claimed_at: chrono::DateTime<chrono::Utc>,
        completed_specs: &mut Vec<(String, String)>,
    ) -> Result<SpecOutcomeAction> {
        match outcome {
            SpecExecutionOutcome::Completed { summary } => {
                completed_specs.push((spec.name.clone(), summary));
                self.fire_on_spec_completed_hooks(lp, spec).await;
                Ok(SpecOutcomeAction::Continue)
            }
            SpecExecutionOutcome::Paused | SpecExecutionOutcome::Superseded => {
                Ok(SpecOutcomeAction::Pause)
            }
            SpecExecutionOutcome::Failed(summary) => {
                self.fail_graph(
                    graph_id,
                    Some(claimed_at),
                    Some(&spec.name),
                    Some(&spec.id),
                    &summary,
                )
                .await?;
                Ok(SpecOutcomeAction::Pause) // returns early anyway
            }
            SpecExecutionOutcome::Blocked(blocker) => {
                self.block_graph(graph_id, Some(claimed_at), &blocker)
                    .await?;
                Ok(SpecOutcomeAction::Pause) // returns early anyway
            }
        }
    }

    /// Core of [`Self::run_graph`], plus the one bit `run_graph`'s public
    /// signature can't carry: whether this call is *resuming* an
    /// already-in-flight run ([`Self::resume_background`], the sole path
    /// behind `graph_continue` and interrupted-queue/autorun resumption) or a
    /// fresh dispatch (`graph_run`, including relaunching a `paused` graph
    /// directly, and the graph's initial launch).
    ///
    /// That distinction is exactly what `{{spec_start_head}}` (B10) needs: a
    /// spec can be left `running` in the DB either because this exact run is
    /// paused mid-node-graph (daemon restart, explicit `graph_pause`) — where
    /// the previously captured baseline is still correct and must be kept —
    /// or because a *prior, distinct* run/launch died without ever being
    /// reset — where reusing that baseline would silently compare against a
    /// HEAD from a different attempt entirely. Only `is_resume = true`
    /// (i.e. only [`Self::resume_background`]) is allowed to reuse it; every
    /// other entry point re-captures, per spec.
    pub(crate) async fn run_graph_dispatch(
        &self,
        graph_id: String,
        queue_id: Option<String>,
        workdir_override: Option<String>,
        mode: DispatchMode,
        idea: Option<String>,
        sandbox: Option<Sandbox>,
    ) -> Result<()> {
        let is_resume = mode.is_resume;
        let directed = mode.directed.as_ref();
        let Some(lp) = self.db.get_graph(&graph_id)? else {
            bail!("Graph '{}' not found.", graph_id);
        };
        if let Some(message) =
            self.empty_launch_check(&graph_id, queue_id.as_deref(), idea.as_deref())?
        {
            return Err(EmptySpecSetError(message).into());
        }
        let claimed_at = chrono::Utc::now();
        if !self.db.claim_graph_for_run(&graph_id, claimed_at)? {
            tracing::info!(
                "Graph '{}' is already running; this launch is a duplicate and was refused \
                 (another dispatch owns the run).",
                graph_id
            );
            return Ok(());
        }
        self.db
            .set_graph_active_run_queue(&graph_id, queue_id.as_deref())?;
        let workdir = Self::resolve_dispatch_workdir(&lp, workdir_override, &sandbox);
        self.announce_dispatch_start(&lp, &graph_id, queue_id.as_deref(), &workdir, is_resume)?;
        let mut completed_specs: Vec<(String, String)> = Vec::new();
        let ctx = DispatchCtx {
            lp: &lp,
            graph_id: &graph_id,
            workdir: &workdir,
            is_resume,
            directed,
            claimed_at,
        };
        match &queue_id {
            Some(queue_id) => {
                self.warn_stale_running_queue_members(&graph_id, queue_id)?;
                if self
                    .run_resumed_queue_spec(&ctx, queue_id, &mut completed_specs)
                    .await?
                    == DispatchFlow::Stop
                {
                    return Ok(());
                }
                if self
                    .run_queue_pick_loop(&ctx, queue_id, &mut completed_specs)
                    .await?
                    == DispatchFlow::Stop
                {
                    return Ok(());
                }
            }
            None => {
                self.purge_terminal_placeholder_specs(&graph_id)?;
                let specs = self.ensure_placeholder_spec(&ctx, &graph_id, idea).await?;
                if self
                    .run_bound_spec_loop(&ctx, specs, &mut completed_specs)
                    .await?
                    == DispatchFlow::Stop
                {
                    return Ok(());
                }
            }
        }
        if self.warn_incomplete_queue_members(&graph_id, queue_id.as_deref())? {
            return Ok(());
        }
        self.finish_dispatch(&ctx, queue_id.as_deref(), sandbox, &completed_specs)
            .await?;
        Ok(())
    }

    pub(crate) fn resolve_dispatch_workdir(
        lp: &Graph,
        workdir_override: Option<String>,
        sandbox: &Option<Sandbox>,
    ) -> String {
        sandbox
            .as_ref()
            .map(|s| s.worktree_path.to_string_lossy().to_string())
            .or(workdir_override)
            .unwrap_or_else(|| lp.workdir.clone())
    }

    pub(crate) fn announce_dispatch_start(
        &self,
        lp: &Graph,
        graph_id: &str,
        queue_id: Option<&str>,
        workdir: &str,
        is_resume: bool,
    ) -> Result<()> {
        let (done, total_specs) = self.spec_progress(graph_id, queue_id)?;
        let resumed = is_resume || done > 0;
        let first_pending = self.first_pending_spec_name(graph_id, queue_id)?;
        self.notification_service.notify_graph_started(
            &lp.name,
            total_specs,
            resumed,
            first_pending.as_deref(),
        );
        activity::publish(
            &self.db,
            workdir,
            graph_id,
            &lp.name,
            if resumed {
                format!("Resumed ({total_specs} spec(s), {done} done).")
            } else {
                format!("Started ({total_specs} spec(s) pending).")
            },
        );
        Ok(())
    }

    pub(crate) fn warn_stale_running_queue_members(
        &self,
        graph_id: &str,
        queue_id: &str,
    ) -> Result<()> {
        for spec_id in self
            .db
            .queue_stale_running_members(queue_id, crate::system::boot_id().as_deref())?
        {
            tracing::warn!(
                "Graph '{}' queue run against '{}': member spec '{}' is 'running' with no \
                 live node run in this daemon's lifetime; leaving it as-is. Reset it via \
                 graph_reset to resume if it's genuinely stuck.",
                graph_id,
                queue_id,
                spec_id
            );
        }
        Ok(())
    }

    pub(crate) async fn run_resumed_queue_spec(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: &str,
        completed_specs: &mut Vec<(String, String)>,
    ) -> Result<DispatchFlow> {
        if !ctx.is_resume {
            return Ok(DispatchFlow::Continue);
        }
        let Some(running_spec_id) = self.db.queue_running_spec_id(queue_id)? else {
            return Ok(DispatchFlow::Continue);
        };
        let Some(spec) = self.db.get_graph_spec(&running_spec_id)? else {
            return Ok(DispatchFlow::Continue);
        };
        let outcome = self
            .run_spec(
                ctx.lp,
                &spec,
                ctx.workdir,
                ctx.is_resume,
                Some(queue_id),
                ctx.directed.filter(|d| d.spec_id == spec.id),
            )
            .await?;
        let action = self
            .handle_spec_outcome(
                outcome,
                &spec,
                ctx.lp,
                ctx.graph_id,
                ctx.claimed_at,
                completed_specs,
            )
            .await?;
        match action {
            SpecOutcomeAction::Continue => Ok(DispatchFlow::Continue),
            SpecOutcomeAction::Pause => Ok(DispatchFlow::Stop),
        }
    }

    pub(crate) async fn run_queue_pick_loop(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: &str,
        completed_specs: &mut Vec<(String, String)>,
    ) -> Result<DispatchFlow> {
        // CM43 queue caveat: a directed `graph_run` only picks the queue's
        // *pending* member, so it cannot start at a member that is currently
        // `running`/`interrupted`. Use `graph_continue run_from_node` for that.
        loop {
            if self.is_paused(ctx.graph_id)? {
                return Ok(DispatchFlow::Stop);
            }
            let Some(spec_id) = self.db.queue_next_pending_spec_id(queue_id)? else {
                break;
            };
            let Some(spec) = self.db.get_graph_spec(&spec_id)? else {
                continue;
            };
            let outcome = self
                .run_spec(
                    ctx.lp,
                    &spec,
                    ctx.workdir,
                    ctx.is_resume,
                    Some(queue_id),
                    ctx.directed.filter(|d| d.spec_id == spec.id),
                )
                .await?;
            let action = self
                .handle_spec_outcome(
                    outcome,
                    &spec,
                    ctx.lp,
                    ctx.graph_id,
                    ctx.claimed_at,
                    completed_specs,
                )
                .await?;
            match action {
                SpecOutcomeAction::Continue => continue,
                SpecOutcomeAction::Pause => return Ok(DispatchFlow::Stop),
            }
        }
        Ok(DispatchFlow::Continue)
    }

    pub(crate) fn purge_terminal_placeholder_specs(&self, graph_id: &str) -> Result<()> {
        for spec in self.db.list_graph_specs(graph_id)? {
            if is_no_spec_placeholder(&spec)
                && matches!(
                    spec.status,
                    GraphSpecStatus::Completed | GraphSpecStatus::Failed | GraphSpecStatus::Skipped
                )
            {
                self.db.delete_graph_spec(&spec.id)?;
            }
        }
        Ok(())
    }

    pub(crate) async fn ensure_placeholder_spec(
        &self,
        ctx: &DispatchCtx<'_>,
        graph_id: &str,
        idea: Option<String>,
    ) -> Result<Vec<GraphSpec>> {
        let specs = self.db.list_graph_specs(graph_id)?;
        if !specs.is_empty() {
            return Ok(specs);
        }
        let Some(idea_text) = idea.as_ref() else {
            let message = self.empty_spec_set_message(ctx.lp, None)?;
            self.fail_graph(graph_id, Some(ctx.claimed_at), None, None, &message)
                .await?;
            return Err(EmptySpecSetError(message).into());
        };
        if idea_text.trim().is_empty() {
            let message = self.empty_spec_set_message(ctx.lp, None)?;
            self.fail_graph(graph_id, Some(ctx.claimed_at), None, None, &message)
                .await?;
            return Err(EmptySpecSetError(message).into());
        }
        let mut placeholder = no_spec_placeholder(graph_id);
        placeholder.description = Some(idea_text.clone());
        self.db.insert_graph_spec(&placeholder)?;
        Ok(vec![placeholder])
    }

    pub(crate) async fn run_bound_spec_loop(
        &self,
        ctx: &DispatchCtx<'_>,
        specs: Vec<GraphSpec>,
        completed_specs: &mut Vec<(String, String)>,
    ) -> Result<DispatchFlow> {
        // CM43: a directed run makes its target spec the FIRST spec it
        // executes — bound specs before it are skipped, and every spec after
        // it runs normally from Start.
        let mut reached_directed = ctx.directed.is_none();
        for spec in specs {
            if self.is_paused(ctx.graph_id)? {
                return Ok(DispatchFlow::Stop);
            }
            let directed_for_spec = ctx.directed.filter(|d| d.spec_id == spec.id);
            if let Some(target) = ctx.directed {
                if !reached_directed {
                    if spec.id != target.spec_id {
                        continue; // specs before the directed target are not run
                    }
                    reached_directed = true;
                }
            } else if matches!(
                spec.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            ) {
                continue;
            }
            let outcome = self
                .run_spec(
                    ctx.lp,
                    &spec,
                    ctx.workdir,
                    ctx.is_resume,
                    None,
                    directed_for_spec,
                )
                .await?;
            let action = self
                .handle_spec_outcome(
                    outcome,
                    &spec,
                    ctx.lp,
                    ctx.graph_id,
                    ctx.claimed_at,
                    completed_specs,
                )
                .await?;
            match action {
                SpecOutcomeAction::Continue => continue,
                SpecOutcomeAction::Pause => return Ok(DispatchFlow::Stop),
            }
        }
        Ok(DispatchFlow::Continue)
    }

    pub(crate) fn warn_incomplete_queue_members(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<bool> {
        let Some(queue_id) = queue_id else {
            return Ok(false);
        };
        if !self.db.queue_has_incomplete_members(queue_id)? {
            return Ok(false);
        }
        tracing::warn!(
            "Graph '{}' queue run against '{}' found no pending member to pick, but the \
             queue still has incomplete (non completed/skipped) member(s); leaving the \
             graph as-is rather than marking it completed. Reset the stuck member(s) via \
             graph_reset to resume.",
            graph_id,
            queue_id
        );
        Ok(true)
    }

    pub(crate) async fn finish_dispatch(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: Option<&str>,
        sandbox: Option<Sandbox>,
        completed_specs: &[(String, String)],
    ) -> Result<()> {
        self.db.update_graph_status(
            ctx.graph_id,
            GraphStatus::Completed,
            None,
            Some(chrono::Utc::now()),
        )?;
        let (done, total) = self.spec_progress(ctx.graph_id, queue_id)?;
        activity::publish(
            &self.db,
            ctx.workdir,
            ctx.graph_id,
            &ctx.lp.name,
            format!("Completed ({done}/{total} specs)."),
        );
        let executed_any_spec = !completed_specs.is_empty();
        let hook_launched = executed_any_spec
            && ctx
                .lp
                .hooks
                .get(&GraphHookEvent::OnCompleted)
                .is_some_and(|hooks| !hooks.is_empty());
        self.notification_service.notify_graph_finished(
            &ctx.lp.name,
            GraphFinishOutcome::Completed {
                done,
                total,
                hook_launched,
            },
        );
        if executed_any_spec {
            self.fire_completion_hook(ctx.lp, ctx.workdir, completed_specs)
                .await;
        }
        self.teardown_sandbox_after_final_status(sandbox, "completed".into())
            .await;
        Ok(())
    }
    /// Build the actionable error text for [`Self::empty_launch_check`].
    ///
    /// Queue membership doesn't record which graph(s) normally draw from it
    /// (queue specs stay standalone — see [`Self::run_graph`]'s doc), so the
    /// one concrete, discoverable link back to "which queue should this graph
    /// use?" is the graph's own [`crate::domain::graphs::Graph::active_run_queue_id`]
    /// — the queue its last real run drew from. This is exactly requirement 3's
    /// guard rail: a queue-less relaunch of a graph that was last queue-driven
    /// names that queue so a recovery agent can retry correctly instead of
    /// the launch silently discarding the queue context.
    pub(crate) fn empty_spec_set_message(
        &self,
        lp: &crate::domain::graphs::Graph,
        queue_id: Option<&str>,
    ) -> Result<String> {
        match queue_id {
            Some(queue_id) => {
                let total = self.db.list_queue_member_spec_ids(queue_id)?.len();
                Ok(format!(
                    "Graph '{}' has no specs to run: queue '{}' has {} member(s), none pending \
                     (all already completed/skipped, or the queue is empty). Add pending specs \
                     to the queue, or pass a different queue_id.",
                    lp.name, queue_id, total
                ))
            }
            None => {
                let mut message = format!(
                    "Graph '{}' has no specs to run: it has 0 bound specs and no queue_id was \
                     given.",
                    lp.name
                );
                match &lp.active_run_queue_id {
                    Some(last_queue) => {
                        message.push_str(&format!(
                            " Its last run drew from queue '{last_queue}' — pass queue_id: \
                             \"{last_queue}\" to relaunch against it."
                        ));
                    }
                    None => {
                        message.push_str(" Pass queue_id to run it against a queue instead.");
                    }
                }
                Ok(message)
            }
        }
    }
}
