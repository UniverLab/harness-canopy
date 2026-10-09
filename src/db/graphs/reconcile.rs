use super::*;

impl Database {
    /// Node runs still `running` for a graph.
    pub fn list_running_graph_runs(&self, graph_id: &str) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE graph_id = ?1 AND status = 'running'",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_run_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Reconcile graphs orphaned by a daemon restart.
    ///
    /// No graph run survives the process that spawned it, so any graph still
    /// `Running` at startup was interrupted mid-execution by the previous
    /// daemon. Pause it, mark its dangling node runs as failed/interrupted,
    /// and mark its in-flight spec (graph-bound or queue member — either way
    /// `run.spec_id` names it) `Interrupted` (not `Pending` — the run was cut
    /// short by something external, not a failure of the work), all in one
    /// transaction so there is no window where the graph is recoverable but
    /// the spec is not (B18). The engine never touches git here — no more
    /// `git stash` — so any uncommitted work the interrupted run left behind
    /// stays exactly where it is; restarting the spec from its entry node on
    /// resume finds it there, and its node prompt says so. Leaving the spec
    /// `running` made it invisible to queue selection
    /// (`queue_next_pending_spec_id` only ever picks a `pending` or
    /// `interrupted` member), permanently orphaning it — `Interrupted` is
    /// just as selectable as `Pending`, so this still can't happen.
    /// `graph_continue` alone is enough to resume it (no `graph_pause` detour
    /// needed). Idempotent: a graph already `Paused` isn't touched by a later
    /// call.
    ///
    /// Daemon-lifecycle recovery only — **never** call this from anything
    /// other than the daemon's own startup path. On 2026-08-03, `canopy
    /// bridge`'s embedded stdio fallback (spawned when the reachability probe
    /// to a live daemon lost a race under concurrent load) ran this via
    /// `run_stdio_server`'s startup sequence: a short-lived helper process
    /// declared a graph the *live* daemon owned "orphaned", SIGKILLed its
    /// node's process group, `git stash`ed the workdir, and paused the graph
    /// — four times, silently, before it was noticed. The doc comment above
    /// only holds for the process that actually owns the daemon's lifecycle;
    /// it is false for any other process that happens to open the same
    /// database. `run_stdio_server` must never call this again. `data_dir`
    /// is used for the ownership gate below, which is the second line of
    /// defence against exactly that mistake, not a substitute for keeping
    /// the caller list to one entry.
    pub fn reconcile_orphaned_graphs(
        &self,
        data_dir: &std::path::Path,
    ) -> Result<Vec<(Graph, GraphNodeRun)>> {
        // Ownership gate: if the on-disk pid file names a *live* process
        // that isn't us, some other process owns the daemon lifecycle right
        // now and this call has no business touching graph state — signal
        // nothing, pause nothing, quarantine nothing. Belt and braces with
        // keeping `run_stdio_server` from calling this at all: that removes
        // the caller, this makes the function itself safe to call by
        // mistake.
        if crate::daemon::process::read_pid(data_dir).is_some_and(|pid| {
            pid != std::process::id() && crate::daemon::process::is_process_running(pid)
        }) {
            return Ok(Vec::new());
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;

        let orphaned: Vec<Graph> = {
            let mut stmt = tx.prepare(
                "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
                 FROM graphs WHERE status IN (?1, ?2)",
            )?;
            // CB31: a graph caught mid-`Pausing` (wait-for-completion pause
            // requested, running node not yet finished) by a daemon restart is
            // just as orphaned as a `Running` one — its dispatch is gone and
            // the pending pause will never complete on its own. Reconcile it
            // the same way: pause, mark the dangling run interrupted, mark the
            // spec interrupted, so `graph_continue` alone can resume it.
            let rows = stmt.query_map(
                params![GraphStatus::Running.as_str(), GraphStatus::Pausing.as_str()],
                map_graph_row,
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut reconciled: Vec<(Graph, GraphNodeRun)> = Vec::new();
        for lp in &orphaned {
            let dangling_runs: Vec<GraphNodeRun> = {
                let mut stmt = tx.prepare(
                    "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
                      FROM graph_runs WHERE graph_id = ?1 AND status = 'running'",
                )?;
                let rows = stmt.query_map(params![lp.id], map_graph_run_row)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if dangling_runs.is_empty() {
                tracing::warn!(
                    "Reconciling orphaned graph '{}': no active node run found; pausing.",
                    lp.id
                );
            }
            for run in &dangling_runs {
                Self::reconcile_orphan_run(&tx, lp, run)?;
                reconciled.push((lp.clone(), run.clone()));
            }
            // CB82: graceful-shutdown rows were finalized Fail+`interrupted`
            // by the engine (or the shutdown sweep), so the `running` query
            // above misses them while their spec is still `Running` — repair
            // those specs so retry resumes at the interrupted node itself.
            Self::reconcile_interrupted_shutdown_runs(&tx, lp)?
                .into_iter()
                .for_each(|run| {
                    reconciled.push((lp.clone(), run));
                });
            // C1: flag this pause as reconciliation's own, distinct from an
            // operator's `graph_pause`/`graph_report_blocker` (both go through
            // `update_graph_status`, which always clears this flag) — see
            // [`crate::domain::graphs::Graph::paused_by_reconciliation`].
            tx.execute(
                "UPDATE graphs SET status = ?1, paused_by_reconciliation = 1 WHERE id = ?2",
                params![GraphStatus::Paused.as_str(), lp.id],
            )?;
        }

        tx.commit()?;
        Ok(reconciled)
    }

    /// Attempt a best-effort kill of a same-boot orphan's process group,
    /// skipping the kill when the recorded pid is an ancestor of this process.
    pub(super) fn terminate_same_boot_orphan(lp: &Graph, run: &GraphNodeRun) {
        // B12: this new daemon process never held a `Child` for
        // `run` — it may not even share the previous process's
        // memory — so a persisted pid is all reconciliation has to
        // go on, and a pid alone can't tell a genuine survivor from
        // an unrelated process that reused the same pid after a
        // reboot recycled the pid space. Only attempt the kill when
        // the run's recorded boot id still matches the machine's
        // current one (same boot, i.e. the *daemon* crashed/restarted
        // without the OS rebooting) — otherwise the pid is
        // meaningless and killing it could hit an unrelated process.
        let (Some(pid), Some(run_boot_id)) = (run.pid, run.boot_id.as_deref()) else {
            return;
        };
        if crate::system::boot_id().as_deref() != Some(run_boot_id) {
            return;
        }
        let ancestors = crate::daemon::process::ancestor_pids();
        if ancestors.contains(&(pid as u32)) {
            tracing::warn!(
                "Reconciling orphaned graph '{}': skipping kill of pid {} — it is an ancestor of this process",
                lp.id,
                pid
            );
        } else {
            tracing::warn!(
                "Reconciling orphaned graph '{}': attempting best-effort kill of survivor pid {} from the same boot.",
                lp.id,
                pid
            );
            crate::daemon::process::terminate_process_group_async(
                pid,
                crate::daemon::process::KILL_GRACE,
            );
        }
    }

    /// Reconcile a single dangling run: log it, best-effort kill any same-boot
    /// survivor, fail the run, and mark its spec interrupted.
    pub(super) fn reconcile_orphan_run(
        tx: &rusqlite::Transaction<'_>,
        lp: &Graph,
        run: &GraphNodeRun,
    ) -> Result<()> {
        tracing::warn!(
            "Reconciling orphaned graph '{}': was running node '{}' (spec '{}') when the daemon last stopped; pausing graph, marking its run as interrupted, and marking the spec interrupted.",
            lp.id,
            run.node_id,
            run.spec_id
        );
        Self::terminate_same_boot_orphan(lp, run);
        let output = serde_json::json!({
            "interrupted": true,
            "reason": "daemon restarted while this node was running"
        });

        let now = Utc::now();
        tx.execute(
            "UPDATE graph_runs
             SET status = ?1, output = ?2, completed_at = ?3, pid = NULL
             WHERE id = ?4",
            params![
                GraphRunStatus::Fail.as_str(),
                serde_json::to_string(&output)?,
                now.timestamp(),
                run.id,
            ],
        )?;
        Self::mark_spec_interrupted(tx, &run.spec_id)?;
        Ok(())
    }

    /// Mark `spec_id` interrupted and clear its attempt baselines — the
    /// shared half of every orphan reconciliation path. The engine never
    /// touches git (no more `git stash`): the partial work an interrupted
    /// run left in the workdir stays exactly where it is. Marking the spec
    /// `Interrupted` (not reset to `Pending`) is what used to be the stash's
    /// job — it's what tells the next pickup's rendered prompt to say "a
    /// previous attempt exists, continue it" instead of silently starting
    /// fresh. `spec_start_head` and `spec_committed_head` (C15) are still
    /// cleared: the next attempt captures its own fresh baseline and starts
    /// with no committed-head evidence of its own (`run_spec` only reuses a
    /// persisted baseline for a same-attempt resume of a `Running` spec,
    /// which an `Interrupted` pickup is not) — a restart must never let a
    /// *stale* `spec_committed_head` from the interrupted attempt pass a
    /// check for an attempt that hasn't committed anything itself yet.
    pub(super) fn mark_spec_interrupted(
        tx: &rusqlite::Transaction<'_>,
        spec_id: &str,
    ) -> Result<()> {
        tx.execute(
            "UPDATE graph_specs
             SET status = ?1, started_at = NULL, completed_at = NULL, spec_start_head = NULL, spec_committed_head = NULL,
                 updated_at = ?3
             WHERE id = ?2",
            params![
                GraphSpecStatus::Interrupted.as_str(),
                spec_id,
                Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    }

    /// CB82 sweep for graceful-shutdown rows: bound specs (plus queue members
    /// when the graph ran a queue) still `Running` whose latest run carries
    /// the `interrupted` marker. Without this the spec would stay `Running`
    /// instead of `Interrupted`, and `retry_current_node` could not resume
    /// at the interrupted node itself. Returns the interrupted runs so the
    /// caller can count them in the `reconciled` vector.
    pub(super) fn reconcile_interrupted_shutdown_runs(
        tx: &rusqlite::Transaction<'_>,
        lp: &Graph,
    ) -> Result<Vec<GraphNodeRun>> {
        let mut spec_ids: Vec<String> = {
            let mut stmt = tx
                .prepare("SELECT id FROM graph_specs WHERE graph_id = ?1 AND status = 'running'")?;
            let rows = stmt.query_map(params![lp.id], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        if let Some(queue_id) = lp.active_run_queue_id.as_deref() {
            let mut stmt = tx.prepare(
                "SELECT ls.id FROM queue_members pm
                 JOIN graph_specs ls ON ls.id = pm.spec_id
                 WHERE pm.queue_id = ?1 AND ls.status = 'running'",
            )?;
            let rows = stmt.query_map(params![queue_id], |row| row.get(0))?;
            for id in rows.collect::<rusqlite::Result<Vec<String>>>()? {
                if !spec_ids.contains(&id) {
                    spec_ids.push(id);
                }
            }
        }
        let mut interrupted_runs: Vec<GraphNodeRun> = Vec::new();
        for spec_id in &spec_ids {
            if Self::spec_latest_run_was_interrupted(tx, spec_id)? {
                let run = tx
                    .query_row(
                        "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model FROM graph_runs WHERE spec_id = ?1 ORDER BY started_at DESC LIMIT 1",
                        params![spec_id],
                        map_graph_run_row,
                    )
                    .optional()?;
                if let Some(run) = run {
                    interrupted_runs.push(run);
                }
                tracing::warn!(
                    "Reconciling orphaned graph '{}': spec '{}' ended interrupted by daemon shutdown; marking the spec interrupted.",
                    lp.id,
                    spec_id
                );
                Self::mark_spec_interrupted(tx, spec_id)?;
            }
        }
        Ok(interrupted_runs)
    }

    /// Whether the latest run for `spec_id` carries the CB82 `interrupted`
    /// marker (engine finalizer or shutdown sweep).
    fn spec_latest_run_was_interrupted(
        tx: &rusqlite::Transaction<'_>,
        spec_id: &str,
    ) -> Result<bool> {
        let output: Option<String> = tx
            .query_row(
                "SELECT output FROM graph_runs WHERE spec_id = ?1 ORDER BY started_at DESC LIMIT 1",
                params![spec_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(text) = output else {
            return Ok(false);
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            return Ok(false);
        };
        Ok(value
            .get("interrupted")
            .and_then(serde_json::Value::as_bool)
            == Some(true))
    }

    /// Reset queue-member specs stuck `running` with no active node run in
    /// this daemon's lifetime back to `pending`. Covers the gap between
    /// `reconcile_orphaned_graphs` (which only touches graphs that were
    /// themselves `Running` at boot) and a queue member left `running` by a
    /// path that paused the graph without resetting the spec (e.g. a
    /// BLOCKER-reported spec that was never cleaned up). Called at server
    /// startup after `reconcile_orphaned_graphs`.
    pub fn reconcile_stranded_queue_specs(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;

        let paused_graphs: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, active_run_queue_id FROM graphs
                 WHERE status = ?1 AND active_run_queue_id IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![GraphStatus::Paused.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut reset_count = 0;
        let boot_id = crate::system::boot_id();
        let current_boot_id = boot_id.as_deref();

        for (graph_id, queue_id) in &paused_graphs {
            let stranded_specs: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT pm.spec_id FROM queue_members pm
                     JOIN graph_specs ls ON ls.id = pm.spec_id
                     WHERE pm.queue_id = ?1 AND ls.status = ?2
                     AND NOT EXISTS (
                         SELECT 1 FROM graph_runs lr
                         WHERE lr.spec_id = pm.spec_id
                         AND lr.status = 'running'
                         AND lr.boot_id = ?3
                     )
                     ORDER BY pm.position ASC",
                )?;
                let rows = stmt.query_map(
                    params![queue_id, GraphSpecStatus::Running.as_str(), current_boot_id],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            for spec_id in &stranded_specs {
                let interrupted =
                    Self::reconcile_stranded_spec(&tx, graph_id, spec_id, current_boot_id)?;

                let new_status = if interrupted {
                    GraphSpecStatus::Interrupted
                } else {
                    GraphSpecStatus::Pending
                };
                tx.execute(
                    "UPDATE graph_specs
                     SET status = ?1, started_at = NULL, completed_at = NULL,
                         spec_start_head = NULL, spec_committed_head = NULL,
                         updated_at = ?3
                     WHERE id = ?2",
                    params![new_status.as_str(), spec_id, Utc::now().timestamp_millis()],
                )?;
                reset_count += 1;
            }
        }

        tx.commit()?;
        Ok(reset_count)
    }

    /// Inspect the most recent `graph_runs` row for a stranded spec and fail it
    /// if it is evidence of a genuinely interrupted run (stale boot or dead
    /// pid). Returns whether that interrupted evidence was found.
    pub(super) fn reconcile_stranded_spec(
        tx: &rusqlite::Transaction<'_>,
        graph_id: &str,
        spec_id: &str,
        current_boot_id: Option<&str>,
    ) -> Result<bool> {
        tracing::warn!(
            "Reconciling stranded queue spec '{}' in graph '{}': \
             was 'running' with no active node run in this daemon's \
             lifetime; checking for evidence it was genuinely interrupted.",
            spec_id,
            graph_id
        );

        // B36: the `NOT EXISTS` above only proves no run for this
        // spec claims the *current* boot — a `graph_runs` row can
        // still be sitting at `status = 'running'` from a previous
        // boot (e.g. the daemon died before a graceful path like
        // `graph_report_blocker` could finalize it). Identify it from
        // recorded facts only — a boot id that isn't this one, or a
        // pid that's no longer alive — and mark it distinctly as
        // interrupted (never as a plain node failure). The engine
        // never touches git: no quarantine, no `git stash` — the
        // interrupted run's worktree changes are left exactly as
        // they were.
        let stale_run: Option<(String, Option<i64>, Option<String>)> = tx
            .query_row(
                "SELECT id, pid, boot_id FROM graph_runs
                 WHERE spec_id = ?1 AND status = 'running'
                 ORDER BY started_at DESC LIMIT 1",
                params![spec_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;

        // Only evidence of a genuinely interrupted run (a stale-boot
        // or dead-pid `graph_runs` row) earns `Interrupted` instead of
        // a plain reset to `Pending` — a spec stuck `running` with no
        // run row behind it at all has no such evidence, so it's
        // reset exactly as before.
        let mut interrupted = false;
        if let Some((run_id, pid, run_boot_id)) = stale_run {
            let boot_mismatch = match (run_boot_id.as_deref(), current_boot_id) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            };
            let pid_dead = pid
                .and_then(|p| u32::try_from(p).ok())
                .map(|p| !crate::daemon::process::is_process_running(p))
                .unwrap_or(false);

            if boot_mismatch || pid_dead {
                interrupted = true;
                let output = serde_json::json!({
                    "interrupted": true,
                    "reason": "daemon restarted or its process died while this node was running"
                });

                tx.execute(
                    "UPDATE graph_runs
                     SET status = ?1, output = ?2, completed_at = ?3, pid = NULL
                     WHERE id = ?4",
                    params![
                        GraphRunStatus::Fail.as_str(),
                        serde_json::to_string(&output)?,
                        Utc::now().timestamp(),
                        run_id,
                    ],
                )?;
            }
        }

        Ok(interrupted)
    }

    pub fn get_graph_details(&self, graph_id: &str) -> Result<Option<GraphDetails>> {
        let Some(lp) = self.get_graph(graph_id)? else {
            return Ok(None);
        };
        let graph_nodes = self.list_graph_nodes_for_graph(graph_id)?;
        let graph_edges = self.list_graph_edges_for_graph(graph_id)?;
        let specs = self
            .list_graph_specs(graph_id)?
            .into_iter()
            .map(|spec| {
                let nodes = self.list_graph_nodes(&spec.id)?;
                let edges = self.list_graph_edges(&spec.id)?;
                Ok(GraphSpecDetails { spec, nodes, edges })
            })
            .collect::<Result<Vec<_>>>()?;
        let completion_hook_runs = self.list_graph_completion_hook_runs(graph_id)?;

        Ok(Some(GraphDetails {
            lp,
            graph_nodes,
            graph_edges,
            specs,
            completion_hook_runs,
        }))
    }

    pub(super) fn resolve_id_prefix(
        table: &str,
        prefix: &str,
        conn: &std::sync::MutexGuard<'_, rusqlite::Connection>,
    ) -> Result<Option<String>> {
        if prefix.is_empty() {
            return Ok(None);
        }
        let sql = format!("SELECT id FROM {table} WHERE id = ?1");
        let exists: bool = conn
            .query_row(&sql, params![prefix], |_| Ok(true))
            .optional()
            .map_err(|e| anyhow!("{}", e))?
            .unwrap_or(false);
        if exists {
            return Ok(Some(prefix.to_string()));
        }
        let escaped_prefix = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like_sql = format!("SELECT id FROM {table} WHERE id LIKE ?1 || '%' ESCAPE '\\'");
        let mut stmt = conn.prepare(&like_sql)?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![escaped_prefix], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        match ids.len() {
            0 => Ok(None),
            1 => Ok(Some(ids.into_iter().next().unwrap())),
            _ => Err(anyhow!(
                "Ambiguous {} id prefix '{}' matches {} ids: {}",
                table,
                prefix,
                ids.len(),
                ids.join(", ")
            )),
        }
    }

    pub fn resolve_spec_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_specs", prefix, &conn)
    }

    pub fn resolve_graph_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graphs", prefix, &conn)
    }

    pub fn resolve_graph_node_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_nodes", prefix, &conn)
    }

    pub fn resolve_run_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_runs", prefix, &conn)
    }

    pub fn resolve_edge_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_edges", prefix, &conn)
    }

    pub fn resolve_ensemble_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("ensembles", prefix, &conn)
    }
}
