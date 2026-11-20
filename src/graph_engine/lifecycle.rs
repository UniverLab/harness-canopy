use super::*;

impl GraphEngine {
    /// Daemon-initiated background start with an explicit trigger source for logging.
    pub fn start_background_with_trigger(
        self: Arc<Self>,
        graph_id: String,
        trigger: Option<GraphRunTrigger>,
    ) {
        let mode = match trigger {
            Some(trigger) => DispatchMode::fresh().with_daemon_trigger(trigger),
            None => DispatchMode::fresh(),
        };
        self.spawn_dispatch(graph_id, None, None, None, None, mode);
    }

    /// Same as [`Self::start_background`], but optionally drives the graph's
    /// pending queue specs (see [`Self::run_graph`]) and/or overrides the
    /// workdir for this run only.
    ///
    /// This is a fresh dispatch, not a resume — it backs `graph_control`, the tool
    /// a human/scheduler calls to launch or *relaunch* a graph (including
    /// directly relaunching a `paused` graph instead of going through
    /// `graph_control`). Every spec it reaches is treated as newly entered
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
        let mode = match directed {
            Some(directed) => DispatchMode::fresh_from(directed),
            None => DispatchMode::fresh(),
        };
        self.spawn_dispatch(graph_id, queue_id, workdir_override, idea, sandbox, mode);
    }

    /// FR3/FR5: a cron tick launches a spec-less graph with its
    /// `trigger_prompt` as the idea, through the same idea-run machinery as
    /// every other launch. The tick flag is what enables the idle carve-out —
    /// an operator's own `graph_control run` of the same graph never sets it.
    pub(crate) fn start_background_tick(self: Arc<Self>, graph_id: String, idea: String) {
        self.spawn_dispatch(graph_id, None, None, Some(idea), None, DispatchMode::tick());
    }

    /// Spawn a fire-and-forget dispatch in `mode`, translating a launch
    /// refusal (`EmptySpecSetError`) and a real failure exactly as the
    /// operator-facing launch paths always have.
    fn spawn_dispatch(
        self: Arc<Self>,
        graph_id: String,
        queue_id: Option<String>,
        workdir_override: Option<String>,
        idea: Option<String>,
        sandbox: Option<Sandbox>,
        mode: DispatchMode,
    ) {
        let trigger = mode.daemon_trigger;
        tokio::spawn(async move {
            if let Err(error) = self
                .run_graph_mode(
                    graph_id.clone(),
                    queue_id,
                    workdir_override,
                    idea,
                    sandbox,
                    mode,
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
                        .fail_graph(
                            &graph_id,
                            GraphFailure {
                                dispatch_started_at: None,
                                spec_name: None,
                                spec_id: None,
                                summary: &error.to_string(),
                                completed_specs: &[],
                                daemon_trigger: trigger,
                            },
                        )
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
                    let result = self.record_paused_if_accepted(
                        self.db
                            .update_graph_status(graph_id, GraphStatus::Paused, None, None),
                        &lp,
                    );
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
                    self.record_paused_if_accepted(
                        self.db
                            .update_graph_status(graph_id, GraphStatus::Paused, None, None),
                        &lp,
                    )
                } else {
                    // Wait-for-completion mode: a node is running. Record the
                    // pending pause; the engine transitions the graph to
                    // `Paused` once that node finishes on its own.
                    self.request_pause_wait_for_completion(&lp)
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
                    self.record_paused_if_accepted(
                        self.db
                            .update_graph_status(graph_id, GraphStatus::Paused, None, None),
                        &lp,
                    )
                } else {
                    Ok(true) // Already pausing, nothing to do
                }
            }
            GraphStatus::Paused => Ok(true), // Already paused
            _ => Ok(false),                  // Not running
        }
    }

    /// Record `graph_paused` for an accepted immediate pause, passing the
    /// status-write result through — one helper so the three immediate-pause
    /// branches narrate identically.
    fn record_paused_if_accepted(&self, result: Result<bool>, lp: &Graph) -> Result<bool> {
        if matches!(result, Ok(true)) {
            activity::record(
                &self.db,
                activity::GraphEvent {
                    graph_id: &lp.id,
                    graph_name: &lp.name,
                    workdir: &lp.workdir,
                    spec_id: None,
                    node_id: None,
                    run_id: None,
                    kind: activity::GraphEventKind::GraphPaused,
                    message: "Paused.".to_string(),
                    payload: None,
                },
            );
        }
        result
    }

    /// The wait-for-completion leg of [`Self::request_pause`]: record the
    /// pending pause and narrate it, once the caller has established a node
    /// is running.
    fn request_pause_wait_for_completion(&self, lp: &Graph) -> Result<bool> {
        match self.db.request_pause_pending(&lp.id) {
            Ok(true) => {
                self.record_pause_requested(lp);
                Ok(true)
            }
            // A lost compare-and-set raced a concurrent transition — the
            // winner narrated already, so report from the status left
            // behind rather than refusing a live pause as "not running".
            Ok(false) => Ok(lost_pause_race_accepted(
                self.db.get_graph(&lp.id)?.map(|lp| lp.status),
            )),
            Err(error) => Err(error),
        }
    }

    /// Narrate an accepted wait-for-completion pause: a `pause_requested`
    /// event naming every node run the pause is waiting for, so the request
    /// is visible from the moment it is made instead of only as `graph_paused`
    /// minutes later.
    fn record_pause_requested(&self, lp: &Graph) {
        let names = self.db.pause_waiting_node_names(&lp.id).unwrap_or_default();
        let quoted = names
            .iter()
            .map(|name| format!("'{name}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let message = if quoted.is_empty() {
            "Pause requested.".to_string()
        } else {
            format!("Pause requested; waiting for {quoted} to finish.")
        };
        activity::record(
            &self.db,
            activity::GraphEvent {
                graph_id: &lp.id,
                graph_name: &lp.name,
                workdir: &lp.workdir,
                spec_id: None,
                node_id: None,
                run_id: None,
                kind: activity::GraphEventKind::PauseRequested,
                message,
                payload: None,
            },
        );
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
        // CB84: end the harness session too, same as `terminate_run_row` —
        // `graph_control(interrupt: true)` kills a live node run for reason
        // "graph pause", which the objective names explicitly; without this
        // the opencode v2 service-side session keeps ticking beside the
        // resumed attempt. Best-effort, detached, never fails the caller.
        let platform = run.executed_platform.clone().unwrap_or_default();
        if !platform.is_empty() {
            let run_clone = run.clone();
            let workdir = workdir_for_run(&self.db, &run_clone);
            tokio::spawn(async move {
                end_harness_session(&platform, &run_clone, &workdir).await;
            });
        }
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
    /// launch. That's what lets `queue add_spec`/`queue reorder` calls made
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
        self.run_graph_mode(graph_id, queue_id, workdir_override, idea, sandbox, mode)
            .await
    }

    /// [`Self::run_graph`] with the dispatch mode spelled out — the one body
    /// every launch path (operator, hook, cron tick) shares.
    pub(crate) async fn run_graph_mode(
        &self,
        graph_id: String,
        queue_id: Option<String>,
        workdir_override: Option<String>,
        idea: Option<String>,
        sandbox: Option<Sandbox>,
        mode: DispatchMode,
    ) -> Result<()> {
        let trigger = mode.daemon_trigger;
        let engine = self.clone();
        let dispatch_graph_id = graph_id.clone();
        let result = run_guarded(async move {
            engine
                .run_graph_dispatch(
                    dispatch_graph_id,
                    queue_id,
                    workdir_override,
                    mode,
                    idea,
                    sandbox,
                )
                .await
        })
        .await;
        // CH4: clear the hook-launched flag on ALL exit paths (success, failure,
        // pause, block, etc.). This must happen here, not in run_graph_dispatch,
        // because that function has multiple early returns.
        let _ = self.db.clear_graph_hook_launched(&graph_id);
        if let (Some(trigger), Ok(())) = (trigger, &result) {
            if let Ok(Some(graph)) = self.db.get_graph(&graph_id) {
                if let Some(started_at) = graph.started_at {
                    let _ = crate::graph_engine::graph_run_log::append_graph_run_block(
                        &self.db, &graph_id, trigger, started_at,
                    );
                }
            }
        }
        result
    }

    /// Resolve the workdir this dispatch runs in and persist it with the run.
    /// A resume without an explicit override keeps the workdir the run was
    /// launched with — without that fallback the override would silently
    /// expire and the resumed nodes would run against the stored workdir
    /// instead. A fresh run never reads the persisted value; it overwrites
    /// it right after resolving. Lives here rather than in `dispatch.rs` so
    /// that file stays under the 800-line gate.
    pub(crate) fn resolve_and_persist_run_workdir(
        &self,
        graph_id: &str,
        lp: &Graph,
        workdir_override: Option<String>,
        is_resume: bool,
        sandbox: &Option<Sandbox>,
    ) -> Result<String> {
        let workdir_override = if is_resume && workdir_override.is_none() {
            self.db.graph_active_run_workdir(graph_id)?
        } else {
            workdir_override
        };
        let workdir = Self::resolve_dispatch_workdir(lp, workdir_override, sandbox);
        self.db
            .set_graph_active_run_workdir(graph_id, Some(&workdir))?;
        Ok(workdir)
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

    pub fn resume_background(self: Arc<Self>, graph_id: String, directed: Option<DirectedStart>) {
        self.resume_background_with_trigger(graph_id, directed, None);
    }

    /// Resume a graph in the background, optionally marking the dispatch as a
    /// daemon-initiated trigger for run logging.
    pub fn resume_background_with_trigger(
        self: Arc<Self>,
        graph_id: String,
        directed: Option<DirectedStart>,
        trigger: Option<GraphRunTrigger>,
    ) {
        let queue_id = self
            .db
            .get_graph(&graph_id)
            .ok()
            .flatten()
            .and_then(|lp| lp.active_run_queue_id);
        // A sandboxed graph that was paused (daemon restart, `graph_control`)
        // or failed must resume inside its worktree and still merge back on
        // completion — otherwise the remaining nodes run against the user's
        // real repo and the sandbox is stranded. A resume that cannot find
        // its sandbox refuses outright instead of falling back.
        let sandbox = match resolve_resume_sandbox(&self.db, &graph_id) {
            Ok(sandbox) => sandbox,
            Err(msg) => {
                tokio::spawn(async move {
                    tracing::error!("Graph '{graph_id}' resume refused: {msg}");
                    let _ = self
                        .fail_graph(
                            &graph_id,
                            GraphFailure {
                                dispatch_started_at: None,
                                spec_name: None,
                                spec_id: None,
                                summary: &msg,
                                completed_specs: &[],
                                daemon_trigger: trigger,
                            },
                        )
                        .await;
                });
                return;
            }
        };
        let mut mode = match directed {
            Some(d) => DispatchMode::resume_from(d),
            None => DispatchMode::resume(),
        };
        if let Some(trigger) = trigger {
            mode = mode.with_daemon_trigger(trigger);
        }
        tokio::spawn(async move {
            let engine = Arc::clone(&self);
            let dispatch_graph_id = graph_id.clone();
            let result = run_guarded(async move {
                engine
                    .run_graph_dispatch(dispatch_graph_id, queue_id, None, mode, None, sandbox)
                    .await
            })
            .await;
            // CH4: clear the hook-launched flag on ALL exit paths.
            let _ = self.db.clear_graph_hook_launched(&graph_id);
            if let Err(error) = result {
                if error.downcast_ref::<EmptySpecSetError>().is_some() {
                    tracing::error!("Graph '{}' launch refused: {error:#}", graph_id);
                } else {
                    tracing::error!("Graph '{}' failed to run: {error:#}", graph_id);
                    let _ = self
                        .fail_graph(
                            &graph_id,
                            GraphFailure {
                                dispatch_started_at: None,
                                spec_name: None,
                                spec_id: None,
                                summary: &error.to_string(),
                                completed_specs: &[],
                                daemon_trigger: trigger,
                            },
                        )
                        .await;
                }
            }
        });
    }
}

/// Run `fut` in its own task so a panic inside it comes back as an `Err`
/// instead of unwinding the caller.
///
/// Every production graph run awaits `run_graph_dispatch`; a panic there used
/// to drop the task before any `Err` handling ran, leaving the graph
/// `running` forever. Spawning the future and turning the `JoinError` into an
/// `Err` hands the panic to the existing `Err` arms, which log and
/// `fail_graph`. The panic's `file:line` is appended when the daemon's panic
/// hook recorded one.
async fn run_guarded<T, F>(fut: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    match tokio::spawn(fut).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => Err(anyhow!(
            "graph task panicked: {}{}",
            panic_payload(error),
            panic_location_suffix()
        )),
        Err(_) => Err(anyhow!("graph task cancelled")),
    }
}

/// The panic's payload as text, or `non-string panic` when it was not one.
fn panic_payload(error: tokio::task::JoinError) -> String {
    let payload = error.into_panic();
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic".to_string()
    }
}

/// ` at <file:line>` when the panic hook recorded a location, empty otherwise.
fn panic_location_suffix() -> String {
    match crate::daemon::panic_hook::take_last_panic_location() {
        Some(location) => format!(" at {location}"),
        None => String::new(),
    }
}

/// Whether a wait-for-completion pause whose compare-and-set lost still
/// counts as accepted, read off the status the race winner left behind:
/// `Pausing` (the winner recorded `pause_requested`) or `Paused` (the node
/// boundary completed the pause first) — matching [`GraphEngine::request_pause`]'s
/// own arms for those states, without writing a duplicate event. Anything
/// else means the graph genuinely left `Running` (finished, failed, reset)
/// and the pause is refused.
fn lost_pause_race_accepted(status: Option<GraphStatus>) -> bool {
    matches!(status, Some(GraphStatus::Pausing | GraphStatus::Paused))
}

/// The sandbox a resumed graph must run inside: the `active` — or
/// operator-`kept` — row whose worktree still exists. `Ok(None)` when the
/// graph was never sandboxed. `Err`, naming the sandbox id, when the row
/// exists but its worktree is gone (or the lookup itself failed), so the
/// resume refuses instead of silently running against the user's real
/// checkout.
pub(crate) fn resolve_resume_sandbox(
    db: &Database,
    graph_id: &str,
) -> Result<Option<Sandbox>, String> {
    let row = db
        .get_active_sandbox_for_owner("graph", graph_id)
        .map_err(|e| format!("could not look up the sandbox for graph '{graph_id}': {e:#}"))?;
    let Some(sandbox) = row else {
        return Ok(None);
    };
    if sandbox.worktree_path.is_dir() {
        return Ok(Some(sandbox));
    }
    Err(format!(
        "sandbox '{}' for graph '{graph_id}' has no worktree at '{}'; refusing to resume against the original workdir",
        sandbox.id,
        sandbox.worktree_path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::notification_service::DefaultNotificationService;

    fn test_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db = Database::new(&dir.path().join("test.db")).expect("open db");
        (dir, db)
    }

    #[tokio::test]
    async fn run_guarded_reports_a_str_panic() {
        let error = run_guarded::<(), _>(async {
            panic!("str-panic-payload");
        })
        .await
        .expect_err("a panicked future must become an Err");
        let message = error.to_string();
        assert!(message.contains("panicked"), "message: {message}");
        assert!(message.contains("str-panic-payload"), "message: {message}");
    }

    #[tokio::test]
    async fn run_guarded_reports_a_string_panic() {
        let payload = String::from("string-panic-payload");
        let error = run_guarded::<(), _>(async move {
            panic!("{payload}");
        })
        .await
        .expect_err("a panicked future must become an Err");
        let message = error.to_string();
        assert!(message.contains("panicked"), "message: {message}");
        assert!(
            message.contains("string-panic-payload"),
            "message: {message}"
        );
    }

    #[tokio::test]
    async fn run_guarded_returns_ok_unchanged() {
        let value = run_guarded::<i32, _>(async { Ok(42) })
            .await
            .expect("an Ok future stays Ok");
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn run_guarded_returns_err_unchanged() {
        let error = run_guarded::<(), _>(async { Err(anyhow!("boom-err")) })
            .await
            .expect_err("an Err future stays Err");
        assert!(error.to_string().contains("boom-err"), "error: {error:#}");
    }

    async fn wait_for_status(db: &Database, graph_id: &str, want: GraphStatus) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let lp = db.get_graph(graph_id).unwrap().unwrap();
            if lp.status == want {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "graph '{graph_id}' never reached {want:?} (status {:?})",
                lp.status
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Insert a second graph on the same DB/workdir that completes on its own:
    /// one passing check node bound to one pending spec.
    fn insert_passing_graph(
        db: &Database,
        template_graph: &Graph,
        template_spec: &GraphSpec,
        workdir: &str,
    ) -> String {
        let graph_id = format!("second-{}", uuid::Uuid::new_v4());
        let mut graph = template_graph.clone();
        graph.id = graph_id.clone();
        graph.name = "Second".to_string();
        graph.workdir = workdir.to_string();
        graph.status = GraphStatus::Draft;
        graph.started_at = None;
        graph.completed_at = None;
        graph.active_run_queue_id = None;
        graph.hooks.clear();
        db.insert_graph(&graph).unwrap();

        let spec_id = format!("second-spec-{}", uuid::Uuid::new_v4());
        let mut spec = template_spec.clone();
        spec.id = spec_id.clone();
        spec.graph_id = Some(graph_id.clone());
        spec.name = "Second spec".to_string();
        spec.status = GraphSpecStatus::Pending;
        spec.started_at = None;
        spec.completed_at = None;
        db.insert_graph_spec(&spec).unwrap();

        db.insert_graph_node(&GraphNode {
            id: format!("second-check-{}", uuid::Uuid::new_v4()),
            spec_id: Some(spec_id),
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        graph_id
    }

    /// Insert a second, already-completed spec on the same graph so the test
    /// can prove a panic leaves spec statuses alone (FR4).
    fn insert_completed_spec(db: &Database, template_spec_id: &str) -> String {
        let mut spec = db.get_graph_spec(template_spec_id).unwrap().unwrap();
        let done_id = format!("done-spec-{}", uuid::Uuid::new_v4());
        spec.id = done_id.clone();
        spec.name = "Done spec".to_string();
        spec.position = 0;
        spec.status = GraphSpecStatus::Completed;
        spec.completed_at = Some(chrono::Utc::now());
        db.insert_graph_spec(&spec).unwrap();
        done_id
    }

    /// A panic inside `run_graph_dispatch` must reach the existing `Err` arm:
    /// the graph ends `Failed` with the panic text as its failure reason, the
    /// `on_failed` hook fires once, the hook-launched flag is cleared, no spec
    /// is marked completed, and the engine can start another graph afterwards.
    #[tokio::test]
    async fn a_panicking_dispatch_fails_the_graph_and_leaves_the_engine_usable() {
        let (dir, db, engine, graph_id, spec_id) =
            crate::graph_engine::tests::graph_fixture().unwrap();
        let workdir = dir.path().to_string_lossy().to_string();
        let done_spec_id = insert_completed_spec(&db, &spec_id);
        db.mark_graph_as_hook_launched(&graph_id).unwrap();
        let marker = dir.path().join("on-failed-fired");
        let mut hooks = std::collections::BTreeMap::new();
        hooks.insert(
            GraphHookEvent::OnFailed,
            vec![GraphCompletionHook {
                platform: None,
                model: None,
                effort: None,
                prompt: None,
                command: Some(format!("touch {}", marker.display())),
                target_session_id: None,
                target_session_name: None,
                timeout_minutes: Some(1),
                target_graph_id: None,
                queue_id: None,
                workdir_override: None,
                idea: None,
                crew: None,
                spec_ids: None,
            }],
        );
        db.update_graph_hooks(&graph_id, &hooks).unwrap();

        let mut engine = engine;
        engine.panic_at_dispatch = Some(|| panic!("boom-x"));
        Arc::new(engine).start_background_with_trigger(graph_id.clone(), None);
        wait_for_status(&db, &graph_id, GraphStatus::Failed).await;

        let events = db
            .list_graph_events(&graph_id, None, None, None, 200)
            .unwrap();
        let failed = events
            .iter()
            .find(|event| event.kind == "graph_failed")
            .unwrap_or_else(|| panic!("the panic must record a graph_failed event: {events:?}"));
        assert!(
            failed.message.contains("panicked"),
            "failure reason must name the panic: {}",
            failed.message
        );
        assert!(
            failed.message.contains("boom-x"),
            "failure reason must carry the panic payload: {}",
            failed.message
        );

        // `fail_graph` sets the status before it fires the hooks, so wait for
        // the command hook's own effect instead of racing it.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(marker.exists(), "the on_failed command hook must have run");
        let hook_runs = db.list_graph_completion_hook_runs(&graph_id).unwrap();
        let failed_hooks = hook_runs
            .iter()
            .filter(|run| run.event == GraphHookEvent::OnFailed)
            .count();
        assert_eq!(failed_hooks, 1, "on_failed must fire exactly once");
        assert!(
            !db.is_graph_hook_launched(&graph_id).unwrap(),
            "the panic path must still clear the hook-launched flag"
        );
        // FR4: the panic must never mark a spec completed nor rewrite a
        // completed spec — the in-flight spec stays resumable via retry.
        let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
        assert_eq!(
            spec.status,
            GraphSpecStatus::Pending,
            "the panic must leave the in-flight spec pending, not completed"
        );
        let done = db.get_graph_spec(&done_spec_id).unwrap().unwrap();
        assert_eq!(
            done.status,
            GraphSpecStatus::Completed,
            "the panic must not touch the already-completed spec"
        );

        let template_graph = db.get_graph(&graph_id).unwrap().unwrap();
        let template_spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
        let second_id = insert_passing_graph(&db, &template_graph, &template_spec, &workdir);
        let second_engine = GraphEngine::new(Arc::clone(&db), Arc::new(DefaultNotificationService));
        second_engine
            .run_graph_mode(
                second_id.clone(),
                None,
                None,
                None,
                None,
                DispatchMode::fresh(),
            )
            .await
            .expect("a second graph must run after the panic");
        let second = db.get_graph(&second_id).unwrap().unwrap();
        assert_eq!(second.status, GraphStatus::Completed);
    }

    fn test_sandbox(id: &str, worktree: &std::path::Path) -> Sandbox {
        Sandbox {
            id: id.to_string(),
            project_hash: "abcd1234".to_string(),
            base_branch: "main".to_string(),
            sandbox_branch: "canopy/sandbox-test".to_string(),
            worktree_path: worktree.to_path_buf(),
            cli_name: "opencode".to_string(),
            original_workdir: "/home/user/project".to_string(),
            created_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn none_without_row() {
        let (_dir, db) = test_db();
        assert!(resolve_resume_sandbox(&db, "graph-1").unwrap().is_none());
    }

    #[test]
    fn lost_pause_race_accepts_pausing_and_paused_only() {
        use GraphStatus::*;
        for status in [Pausing, Paused] {
            assert!(
                lost_pause_race_accepted(Some(status)),
                "{status:?} left behind by the race winner still accepts the pause"
            );
        }
        for status in [
            None,
            Some(Draft),
            Some(Running),
            Some(Completed),
            Some(Failed),
        ] {
            assert!(
                !lost_pause_race_accepted(status),
                "{status:?} after a lost pause race refuses the pause"
            );
        }
    }

    #[test]
    fn finds_kept() {
        let (_dir, db) = test_db();
        let worktree = tempfile::tempdir().expect("worktree dir");
        let sandbox = test_sandbox("sb-kept", worktree.path());
        db.insert_sandbox_run(&sandbox, "graph", "graph-1")
            .expect("insert sandbox run");
        db.update_sandbox_run_status("sb-kept", "kept")
            .expect("mark kept");

        let found = resolve_resume_sandbox(&db, "graph-1")
            .expect("kept sandbox resolves")
            .expect("some sandbox");
        assert_eq!(found.id, "sb-kept");
        assert_eq!(found.worktree_path, worktree.path());
    }

    #[test]
    fn refuses_missing_worktree() {
        let (_dir, db) = test_db();
        let dir = tempfile::tempdir().expect("parent dir");
        let gone = dir.path().join("gone");
        let sandbox = test_sandbox("sb-gone", &gone);
        db.insert_sandbox_run(&sandbox, "graph", "graph-1")
            .expect("insert sandbox run");

        let err = resolve_resume_sandbox(&db, "graph-1").expect_err("missing worktree refuses");
        assert!(
            err.contains("sb-gone"),
            "refusal must name the sandbox id: {err}"
        );
    }
}
