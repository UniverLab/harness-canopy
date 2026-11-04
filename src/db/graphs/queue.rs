use super::*;

impl Database {
    /// Schedule a one-shot resume for a graph at `at`. The scheduler fires it
    /// once `at` is reached (if the graph is fireable) and clears the field —
    /// see [`crate::domain::graphs::Graph::is_autorun_due`].
    pub fn schedule_graph_autorun(&self, graph_id: &str, at: DateTime<Utc>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET autorun_at = ?1 WHERE id = ?2",
            params![at.timestamp(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear a graph's pending one-shot autorun schedule, without touching status.
    pub fn clear_graph_autorun(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET autorun_at = NULL WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Graphs with a pending one-shot autorun schedule (regardless of trigger).
    pub fn list_pending_autorun_graphs(&self) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_type, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start, require_spec_type
              FROM graphs WHERE autorun_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], map_graph_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Schedule a one-shot deferred resume for a *paused* graph at `at`: the
    /// scheduler fires the `graph_control` action `action` (defaults to
    /// `retry_current_node` when `None`) once `at` is reached, but only if
    /// the graph is still `Paused` — see
    /// [`crate::domain::graphs::Graph::is_auto_continue_due`]. Distinct from
    /// [`Self::schedule_graph_autorun`], which resets-and-relaunches instead
    /// of resuming in place.
    pub fn schedule_graph_auto_continue(
        &self,
        graph_id: &str,
        at: DateTime<Utc>,
        action: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET auto_continue_at = ?1, auto_continue_action = ?2 WHERE id = ?3",
            params![at.timestamp(), action, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear a graph's pending one-shot auto-continue schedule, without
    /// touching status.
    pub fn clear_graph_auto_continue(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET auto_continue_at = NULL, auto_continue_action = NULL WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Graphs with a pending one-shot auto-continue schedule (regardless of
    /// trigger or current status — callers gate firing on
    /// [`crate::domain::graphs::Graph::is_auto_continue_due`]).
    pub fn list_pending_auto_continue_graphs(&self) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_type, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start, require_spec_type
              FROM graphs WHERE auto_continue_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], map_graph_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_graph_status(
        &self,
        graph_id: &str,
        status: GraphStatus,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        // C1: every status transition through this generic path is explicit
        // (an operator's pause, a run finishing, a launch claiming the graph)
        // — never `reconcile_orphaned_graphs`'s own automatic pause, which
        // writes `paused_by_reconciliation` through its own dedicated
        // statement. Clearing it here unconditionally keeps that flag scoped
        // to exactly the graphs reconciliation itself paused.
        let rows = conn.execute(
            "UPDATE graphs
             SET status = ?1,
                 started_at = COALESCE(?2, started_at),
                 completed_at = COALESCE(?3, completed_at),
                 paused_by_reconciliation = 0,
                 blocker = CASE WHEN ?1 = 'paused' THEN blocker ELSE NULL END
             WHERE id = ?4",
            params![
                status.as_str(),
                started_at.map(|value| value.timestamp()),
                completed_at.map(|value| value.timestamp()),
                graph_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Mark a running graph as pausing (pause requested). Returns true if the
    /// graph was running and is now pausing, false otherwise. Does NOT terminate
    /// running nodes — the engine checks this state between node executions.
    pub fn request_pause_pending(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1 WHERE id = ?2 AND status = 'running'",
            params![GraphStatus::Pausing.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Transition a graph from pausing to paused. Called by the engine after
    /// the running node completes naturally.
    pub fn complete_pause(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1 WHERE id = ?2 AND status = 'pausing'",
            params![GraphStatus::Paused.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Cancel a pending pause, flipping the graph back to `Running` — the
    /// resume path's counterpart to [`Self::complete_pause`]. A single
    /// compare-and-set, so a concurrent node-boundary completion wins or
    /// loses atomically rather than resurrecting a finished pause.
    pub fn cancel_pause_pending(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1 WHERE id = ?2 AND status = 'pausing'",
            params![GraphStatus::Running.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Display names of the node runs a pending pause is waiting for: every
    /// still-`running` (or mutex-`waiting`) run under the graph, resolved to
    /// its node name, falling back to the raw node id when the node row is
    /// gone. Empty when nothing is in flight.
    pub fn pause_waiting_node_names(&self, graph_id: &str) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for run in self.list_running_graph_runs(graph_id)? {
            let name = self
                .get_graph_node(&run.node_id)?
                .map(|node| node.name)
                .unwrap_or(run.node_id);
            names.push(name);
        }
        Ok(names)
    }

    /// Atomically claim `graph_id` for a run by flipping it to `Running`, but
    /// only if it is not *already* `Running` (B42). Returns `true` when this
    /// call won the claim (the graph was fireable and is now `Running`), `false`
    /// when the graph was already `Running` — i.e. another dispatch is live and
    /// this launch must be treated as a no-op rather than starting a duplicate,
    /// superseding run.
    ///
    /// This is a single-statement compare-and-set, so two dispatches racing to
    /// launch the same graph (the classic autorun-vs-resume check-then-act race:
    /// one reads the graph as `failed`, the other hasn't written `running` yet)
    /// serialize on the connection lock and exactly one wins — the guard is on
    /// the status *transition* itself, not on a separate earlier read.
    pub fn claim_graph_for_run(&self, graph_id: &str, started_at: DateTime<Utc>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs
             SET status = ?1,
                 started_at = COALESCE(?2, started_at),
                 blocker = NULL
             WHERE id = ?3 AND status != ?1",
            params![
                GraphStatus::Running.as_str(),
                started_at.timestamp(),
                graph_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Persist (or, with `None`, clear) the queue a run against `graph_id` is
    /// currently drawing from. Called once when a run starts — including a
    /// resumed run, so a failed queue run that gets auto-reset-and-relaunched
    /// re-persists the same queue rather than losing it — and cleared again
    /// only when a run finishes genuinely. See
    /// [`crate::domain::graphs::Graph::active_run_queue_id`].
    pub fn set_graph_active_run_queue(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET active_run_queue_id = ?1 WHERE id = ?2",
            params![queue_id, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// The idea an idea-driven run of `graph_id` last ran with (`None` for a
    /// graph that never ran with an idea, or is spec-driven). Read straight
    /// from the row — there is deliberately no `Graph` field for it (cf.
    /// `is_graph_hook_launched`), so only the resume paths that need it pay
    /// for the read.
    pub fn graph_active_run_idea(&self, graph_id: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare("SELECT active_run_idea FROM graphs WHERE id = ?1")?;
        let mut rows = stmt.query(params![graph_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, Option<String>>(0)?),
            None => Ok(None),
        }
    }

    /// Persist the idea a run against `graph_id` is executing (called once
    /// when the run starts — a resumed run re-persists the same value).
    /// Returns whether the graph row existed.
    pub fn set_graph_active_run_idea(&self, graph_id: &str, idea: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET active_run_idea = ?1 WHERE id = ?2",
            params![idea, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// The workdir a run against `graph_id` was launched with: the override
    /// it ran with, or the stored workdir when none was given (`None` for a
    /// graph that never ran since the column shipped). Read straight from
    /// the row — there is deliberately no `Graph` field for it (cf.
    /// [`Self::graph_active_run_idea`]), so only the resume/hook/display
    /// paths that need it pay for the read.
    pub fn graph_active_run_workdir(&self, graph_id: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare("SELECT active_run_workdir FROM graphs WHERE id = ?1")?;
        let mut rows = stmt.query(params![graph_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, Option<String>>(0)?),
            None => Ok(None),
        }
    }

    /// Persist the workdir a run against `graph_id` executes with (called
    /// once when the run starts — a resumed run re-persists the same value,
    /// a fresh run overwrites it). Returns whether the graph row existed.
    pub fn set_graph_active_run_workdir(
        &self,
        graph_id: &str,
        workdir: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET active_run_workdir = ?1 WHERE id = ?2",
            params![workdir, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// The workdir `graph_id`'s run executes with: the persisted run workdir,
    /// falling back to `stored` (the graph's own workdir) for legacy rows
    /// that never persisted one.
    pub fn graph_run_workdir_or(&self, graph_id: &str, stored: &str) -> String {
        self.graph_active_run_workdir(graph_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| stored.to_string())
    }

    /// The blocker text a guard pause stored for `graph_id` (`None` when the
    /// graph is not paused behind a blocker). Read straight from the row —
    /// there is deliberately no `Graph` field for it (cf.
    /// [`Self::graph_active_run_idea`]), so only the readers that need it
    /// pay for the read.
    pub fn graph_blocker(&self, graph_id: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare("SELECT blocker FROM graphs WHERE id = ?1")?;
        let mut rows = stmt.query(params![graph_id])?;
        match rows.next()? {
            Some(row) => Ok(row.get::<_, Option<String>>(0)?),
            None => Ok(None),
        }
    }

    /// Persist (or, with `None`, clear) the blocker text for `graph_id`.
    /// Returns whether the graph row existed.
    pub fn set_graph_blocker(&self, graph_id: &str, blocker: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET blocker = ?1 WHERE id = ?2",
            params![blocker, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Reset a graph back to `Draft` (the status `graph_control` accepts) and clear
    /// `completed_at`, so a `failed` or `completed` graph can be relaunched via
    /// `graph_control` + `graph_control` instead of being stuck forever.
    pub fn reset_graph_status(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1, completed_at = NULL, blocker = NULL WHERE id = ?2",
            params![GraphStatus::Draft.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }
}
