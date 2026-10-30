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
    /// When set, the first spec this run executes starts at this node
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
    /// behind `graph_control` and interrupted-queue/autorun resumption) or a
    /// fresh dispatch (`graph_control`, including relaunching a `paused` graph
    /// directly, and the graph's initial launch).
    ///
    /// That distinction is exactly what `{{spec_start_head}}` (B10) needs: a
    /// spec can be left `running` in the DB either because this exact run is
    /// paused mid-node-graph (daemon restart, explicit `graph_control`) — where
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
                    &[],
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
    /// behind `graph_control` and interrupted-queue/autorun resumption) or a
    /// fresh dispatch (`graph_control`, including relaunching a `paused` graph
    /// directly, and the graph's initial launch).
    ///
    /// That distinction is exactly what `{{spec_start_head}}` (B10) needs: a
    /// spec can be left `running` in the DB either because this exact run is
    /// paused mid-node-graph (daemon restart, explicit `graph_control`) — where
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
        // FR1/FR2: a resume (or directed start) of an idea graph resumes the
        // idea it last ran with — before the emptiness guard below and before
        // the placeholder is (re)created, so both see the stored idea exactly
        // as they would an explicit one.
        let idea = self.effective_idea(&lp, idea, is_resume || directed.is_some())?;
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
        self.clear_directed_spec_limit(directed.map(|d| d.spec_id.as_str()))?;
        self.db
            .set_graph_active_run_queue(&graph_id, queue_id.as_deref())?;
        self.persist_run_idea(&graph_id, idea.as_deref())?;
        let workdir = self.resolve_and_persist_run_workdir(
            &graph_id,
            &lp,
            workdir_override,
            is_resume,
            &sandbox,
        )?;
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
                // Members still `running` at dispatch start — besides the
                // directed spec — are strays of a stale or inherited state.
                // The directed resume resets them to `Pending` and defers
                // them: the pick loop below leaves them for a later
                // dispatch instead of running them right after the directed
                // spec.
                let stray_specs = self.stray_running_member_ids(&ctx, queue_id)?;
                if self
                    .run_resumed_queue_spec(&ctx, queue_id, &mut completed_specs, &stray_specs)
                    .await?
                    == DispatchFlow::Stop
                {
                    return Ok(());
                }
                if self
                    .run_queue_pick_loop(&ctx, queue_id, &mut completed_specs, &stray_specs)
                    .await?
                    == DispatchFlow::Stop
                {
                    return Ok(());
                }
            }
            None => {
                // FR4: a resumed (or directed) run of an IDEA graph keeps the
                // failed idea placeholder — its spec id and node runs carry
                // `{{previous_feedback}}`. Scoped to idea graphs so a stale
                // blank bookkeeping row on a spec-driven graph keeps being
                // purged exactly as before (FR5); a fresh, undirected launch
                // always purges a prior attempt's terminal row.
                let keep_idea_placeholder =
                    (ctx.directed.is_some() || ctx.is_resume) && self.is_idea_graph(&lp)?;
                if !keep_idea_placeholder {
                    self.purge_terminal_placeholder_specs(&graph_id)?;
                }
                self.ensure_placeholder_spec(&ctx, &graph_id, idea).await?;
                if self.run_bound_spec_loop(&ctx, &mut completed_specs).await? == DispatchFlow::Stop
                {
                    return Ok(());
                }
            }
        }
        if self.warn_incomplete_queue_members(&graph_id, queue_id.as_deref())? {
            return Ok(());
        }
        if self
            .fail_dispatch_on_queue_failures(&ctx, queue_id.as_deref(), &completed_specs)
            .await?
        {
            return Ok(());
        }
        self.finish_dispatch(&ctx, queue_id.as_deref(), sandbox, &completed_specs)
            .await?;
        Ok(())
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
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id,
                graph_name: &lp.name,
                workdir,
                spec_id: None,
                node_id: None,
                run_id: None,
                kind: if resumed {
                    activity::GraphEventKind::GraphResumed
                } else {
                    activity::GraphEventKind::GraphStarted
                },
                message: if resumed {
                    format!("Resumed ({total_specs} spec(s), {done} done).")
                } else {
                    format!("Started ({total_specs} spec(s) pending).")
                },
                payload: None,
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
                 graph_control reset to resume if it's genuinely stuck.",
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
        stray_specs: &[String],
    ) -> Result<DispatchFlow> {
        if !ctx.is_resume {
            return Ok(DispatchFlow::Continue);
        }
        // A directed resume targets its own `spec_id` first — it must not be
        // displaced by the queue's still-`running` member, which a stale or
        // inherited state may have left stranded. Before running the directed
        // spec, reset every *other* Running member back to Pending so the
        // directed spec is unambiguously the one the graph runs. When no
        // directed spec is named, the running member stays the resume target
        // exactly as before (and stale members are merely warned about).
        let spec_id = if let Some(directed) = ctx.directed {
            self.reset_stray_running_members(stray_specs)?;
            directed.spec_id.clone()
        } else {
            let Some(running_spec_id) = self.db.queue_running_spec_id(queue_id)? else {
                return Ok(DispatchFlow::Continue);
            };
            running_spec_id
        };
        let Some(spec) = self.db.get_graph_spec(&spec_id)? else {
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

    /// The still-`Running` members of `queue_id` at dispatch start, besides
    /// the directed spec — strays of a stale or inherited state. Empty when
    /// this dispatch is not directed: a plain resume keeps the running
    /// member as its target.
    fn stray_running_member_ids(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: &str,
    ) -> Result<Vec<String>> {
        let Some(directed) = ctx.directed else {
            return Ok(Vec::new());
        };
        let mut strays = Vec::new();
        for spec_id in self.db.list_queue_member_spec_ids(queue_id)? {
            if spec_id == directed.spec_id {
                continue;
            }
            let Some(spec) = self.db.get_graph_spec(&spec_id)? else {
                continue;
            };
            if spec.status == GraphSpecStatus::Running {
                strays.push(spec_id);
            }
        }
        Ok(strays)
    }

    /// Reset the directed run's strays ([`Self::stray_running_member_ids`])
    /// back to `Pending`. A directed resume makes its target spec the one
    /// the graph runs; any other member left `running` by a stale or
    /// inherited state would otherwise block the pick loop (it skips
    /// `running` members) or displace the directed spec. Called only on the
    /// directed path; the non-directed path warns instead
    /// ([`warn_stale_running_queue_members`]).
    fn reset_stray_running_members(&self, stray_specs: &[String]) -> Result<()> {
        for spec_id in stray_specs {
            self.db
                .update_graph_spec_status(spec_id, GraphSpecStatus::Pending, None, None)?;
        }
        Ok(())
    }

    pub(crate) async fn run_queue_pick_loop(
        &self,
        ctx: &DispatchCtx<'_>,
        queue_id: &str,
        completed_specs: &mut Vec<(String, String)>,
        stray_specs: &[String],
    ) -> Result<DispatchFlow> {
        // A directed run makes its target spec the FIRST spec it executes —
        // queue members before it are skipped, and every member after it runs
        // normally from Start. `queue_next_pending_spec_id` re-queries the DB
        // every iteration (so a queue run picks up live reordering and members
        // added mid-run), so the skip decision must re-read the order with it:
        // a member missing from a stale snapshot would loop forever, and a live
        // reordering would be judged against yesterday's order. Skipped
        // before-directed members are materialized as `Skipped`, matching the
        // bound-spec loop's "directed run starts later" message.
        // Strays the directed run reset to `Pending` are deferred, not run —
        // but they stay `Pending`, so the pick query keeps re-picking them;
        // remember them here and stop when one comes back around.
        let mut deferred: Vec<String> = Vec::new();
        loop {
            if self.is_paused(ctx.graph_id)? {
                return Ok(DispatchFlow::Stop);
            }
            let Some(spec_id) = self.db.queue_next_pending_spec_id(queue_id)? else {
                break;
            };
            if deferred.contains(&spec_id) {
                break;
            }
            let Some(spec) = self.db.get_graph_spec(&spec_id)? else {
                continue;
            };
            // A stray waits for a later dispatch — running it here would
            // immediately re-run a spec some other node marked `running` by
            // mistake.
            if stray_specs.contains(&spec.id) {
                deferred.push(spec.id);
                continue;
            }
            let order = self.db.list_queue_member_spec_ids(queue_id)?;
            let directed_pos = ctx
                .directed
                .and_then(|d| order.iter().position(|id| id == &d.spec_id));
            if let Some(d_pos) = directed_pos {
                let Some(idx) = order.iter().position(|id| id == &spec.id) else {
                    continue;
                };
                if idx < d_pos {
                    let label = activity::spec_label(&spec);
                    activity::record(
                        &self.db,
                        activity::GraphEvent {
                            graph_id: ctx.graph_id,
                            graph_name: &ctx.lp.name,
                            workdir: ctx.workdir,
                            spec_id: Some(&spec.id),
                            node_id: None,
                            run_id: None,
                            kind: activity::GraphEventKind::SpecSkipped,
                            message: format!("Spec {label} skipped (directed run starts later)."),
                            payload: None,
                        },
                    );
                    self.db.update_graph_spec_status(
                        &spec.id,
                        GraphSpecStatus::Skipped,
                        None,
                        Some(chrono::Utc::now()),
                    )?;
                    continue;
                }
            }
            // FR2: a directed spec is the operator's named start, so the
            // advance guard does not second-guess it; every other pick is a
            // move from one queue spec to the next and must pass the guard.
            let directed_here = ctx.directed.is_some_and(|d| d.spec_id == spec.id);
            if !directed_here && self.refuse_dirty_queue_advance(ctx, &spec).await? {
                return Ok(DispatchFlow::Stop);
            }
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

    /// FR2: the dirty-tree guard between the specs of one queue run. Before the
    /// pick loop starts `spec` — after a completed, failed, skipped, interrupted
    /// or limit-blocked predecessor alike — it runs the launch guard
    /// ([`Self::dirty_start_check`]) against the worktree. A refusal pauses the
    /// graph with the guard's text, the dirty paths and the ways out; it never
    /// fails the graph. Returns `true` when the graph was paused and the loop
    /// must stop.
    async fn refuse_dirty_queue_advance(
        &self,
        ctx: &DispatchCtx<'_>,
        spec: &GraphSpec,
    ) -> Result<bool> {
        let Some(notice) = self
            .dirty_start_check(
                ctx.graph_id,
                ctx.workdir,
                Some(&spec.id),
                ctx.lp.allow_dirty_start,
            )
            .await?
        else {
            return Ok(false);
        };
        if !notice.refuse {
            return Ok(false);
        }
        let blocker =
            super::launch_check::dirty_advance_blocker(&spec.name, &notice.message, &notice.paths);
        self.block_graph(ctx.graph_id, Some(ctx.claimed_at), &blocker)
            .await?;
        Ok(true)
    }

    /// FR1: an explicit directive is the operator's ruling on the per-spec
    /// execution limit; zero the directed spec's count before its run starts.
    fn clear_directed_spec_limit(&self, spec_id: Option<&str>) -> Result<()> {
        if let Some(spec_id) = spec_id {
            self.db.clear_graph_spec_cross_run_attempts(spec_id)?;
        }
        Ok(())
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

    /// FR1: make sure a graph launched with an idea has a spec row to run.
    /// Hands back nothing: the bound-spec advance re-reads the graph's specs
    /// from the DB every iteration ([`Self::run_bound_spec_loop`]), so
    /// returning a captured list here would be the very snapshot this fix
    /// removes.
    pub(crate) async fn ensure_placeholder_spec(
        &self,
        ctx: &DispatchCtx<'_>,
        graph_id: &str,
        idea: Option<String>,
    ) -> Result<()> {
        if !self.db.list_graph_specs(graph_id)?.is_empty() {
            return Ok(());
        }
        let Some(idea_text) = idea.as_ref() else {
            let message = self.empty_spec_set_message(ctx.lp, None)?;
            self.fail_graph(graph_id, Some(ctx.claimed_at), None, None, &message, &[])
                .await?;
            return Err(EmptySpecSetError(message).into());
        };
        if idea_text.trim().is_empty() {
            let message = self.empty_spec_set_message(ctx.lp, None)?;
            self.fail_graph(graph_id, Some(ctx.claimed_at), None, None, &message, &[])
                .await?;
            return Err(EmptySpecSetError(message).into());
        }
        let mut placeholder = no_spec_placeholder(graph_id);
        placeholder.description = Some(idea_text.clone());
        self.db.insert_graph_spec(&placeholder)?;
        Ok(())
    }

    /// FR1/FR2: run the graph's bound specs to completion, deciding the next
    /// one from a **live** read of the graph at every advance — never from a
    /// list captured when the run started. A spec bound with `graph_edit`
    /// while the run is in flight is therefore seen by the very next advance:
    /// at its own position when it comes after the spec that just ran, and
    /// right after it when it was bound at a lower position (the scan restarts
    /// at position 0 and skips the finished ones). The 2026-10-26 loss — a
    /// spec bound at position 6 while spec 4 ran, left `pending` forever as the
    /// graph reported `Completed (5/6 specs).` — is exactly what the fresh read
    /// rules out.
    pub(crate) async fn run_bound_spec_loop(
        &self,
        ctx: &DispatchCtx<'_>,
        completed_specs: &mut Vec<(String, String)>,
    ) -> Result<DispatchFlow> {
        // A directed run makes its target spec the FIRST spec it executes —
        // bound specs before it are skipped, and every spec after it runs
        // normally from Start.
        let mut reached_directed = ctx.directed.is_none();
        // Specs this dispatch has already dealt with: the live read shows a
        // finished spec again on every later advance, and re-announcing it (or
        // re-running it) on each one would flood the log and loop forever.
        let mut settled: Vec<String> = Vec::new();
        loop {
            if self.is_paused(ctx.graph_id)? {
                return Ok(DispatchFlow::Stop);
            }
            let specs = self.db.list_graph_specs(ctx.graph_id)?;
            let Some(spec) =
                self.pick_next_bound_spec(ctx, &specs, &mut reached_directed, &mut settled)?
            else {
                break;
            };
            let directed_for_spec = ctx.directed.filter(|d| d.spec_id == spec.id);
            settled.push(spec.id.clone());
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

    /// FR1/FR2: the next spec to run out of a live read of the graph's bound
    /// specs (position order) — the first one this dispatch has not settled
    /// yet, skipping specs that are already terminal (announced once). A
    /// directed run first deals with every spec before its target
    /// ([`Self::skip_bound_spec_before_directed`]), so the target is the first
    /// thing that runs — whatever its own status, unlike every other spec.
    fn pick_next_bound_spec(
        &self,
        ctx: &DispatchCtx<'_>,
        specs: &[GraphSpec],
        reached_directed: &mut bool,
        settled: &mut Vec<String>,
    ) -> Result<Option<GraphSpec>> {
        for spec in specs {
            if let Some(target) = ctx.directed.filter(|_| !*reached_directed) {
                if spec.id != target.spec_id {
                    self.skip_bound_spec_before_directed(ctx, spec, settled)?;
                    continue;
                }
                *reached_directed = true;
                // The directed target runs whatever its status: a directed run
                // accepts a `completed` spec (`graph_control run_from_node`,
                // docs/graphs.md), and the queue path's resumed target runs
                // unconditionally the same way. Falling through to the
                // terminal skip would swallow the run the operator just asked
                // for with an "already completed; skipping" line.
                if settled.contains(&spec.id) {
                    continue;
                }
                return Ok(Some(spec.clone()));
            }
            if settled.contains(&spec.id) {
                continue;
            }
            if matches!(
                spec.status,
                GraphSpecStatus::Completed | GraphSpecStatus::Skipped
            ) {
                self.announce_settled_bound_spec(ctx, spec);
                settled.push(spec.id.clone());
                continue;
            }
            return Ok(Some(spec.clone()));
        }
        Ok(None)
    }

    /// FR2 (directed): a bound spec before the directed target does not run in
    /// this dispatch. Announce it once and — for a spec still runnable —
    /// materialize it `Skipped`, the same status the queue pick loop gives a
    /// member before its directed target. A `pending` row left as-is would come
    /// back at the head of this very dispatch's next live read and then leave a
    /// pending spec behind at finish.
    ///
    /// A spec that already reached a verdict keeps it: rewriting
    /// `completed`/`failed` to `skipped` erases the verdict and drops the spec
    /// out of `spec_progress`'s done count, turning the completion line into
    /// `Completed (n/m)` with n < m — the message FR4 exists to make
    /// impossible. A stale `running` row is left alone too (the queue path
    /// defers its strays rather than skipping them); the finish guard names it
    /// instead of letting the graph report completion over in-flight work.
    fn skip_bound_spec_before_directed(
        &self,
        ctx: &DispatchCtx<'_>,
        spec: &GraphSpec,
        settled: &mut Vec<String>,
    ) -> Result<()> {
        let label = activity::spec_label(spec);
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: ctx.graph_id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&spec.id),
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::SpecSkipped,
                message: format!("Spec {label} skipped (directed run starts later)."),
                payload: None,
            },
        );
        if matches!(
            spec.status,
            GraphSpecStatus::Pending | GraphSpecStatus::Interrupted
        ) {
            self.db.update_graph_spec_status(
                &spec.id,
                GraphSpecStatus::Skipped,
                None,
                Some(chrono::Utc::now()),
            )?;
        }
        settled.push(spec.id.clone());
        Ok(())
    }

    /// Announce a spec that was already terminal when this dispatch's live read
    /// saw it — once per dispatch, never for a spec this dispatch ran itself.
    fn announce_settled_bound_spec(&self, ctx: &DispatchCtx<'_>, spec: &GraphSpec) {
        let label = activity::spec_label(spec);
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: ctx.graph_id,
                graph_name: &ctx.lp.name,
                workdir: ctx.workdir,
                spec_id: Some(&spec.id),
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::SpecSkipped,
                message: format!("Spec {label} already completed; skipping."),
                payload: None,
            },
        );
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
