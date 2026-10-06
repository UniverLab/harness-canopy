use super::*;

impl Database {
    /// Record one firing of a graph's `on_completed` hook (N2), started as
    /// `Running` before the process is spawned — mirrors [`Self::insert_graph_run`]'s
    /// pattern of a row that exists before the child does, so a crash mid-spawn
    /// still leaves a `Running` row behind rather than nothing.
    pub fn insert_graph_completion_hook_run(&self, run: &GraphCompletionHookRun) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_completion_hook_runs (id, graph_id, status, output, summary, started_at, completed_at, pid, boot_id, event, hook_index, executed_platform, executed_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &run.id,
                &run.graph_id,
                run.status.as_str(),
                run.output.as_ref().map(serde_json::to_string).transpose()?,
                &run.summary,
                run.started_at.timestamp(),
                run.completed_at.map(|value| value.timestamp()),
                run.pid,
                &run.boot_id,
                run.event.as_str(),
                run.hook_index,
                &run.executed_platform,
                &run.executed_model,
            ],
        )?;
        Ok(())
    }

    /// Same B12 treatment as [`Self::set_graph_run_pid`]: record the spawned
    /// process-group leader so an abnormal end can `killpg` it.
    pub fn set_graph_completion_hook_run_pid(
        &self,
        run_id: &str,
        pid: i64,
        boot_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_completion_hook_runs SET pid = ?1, boot_id = ?2 WHERE id = ?3",
            params![pid, boot_id, run_id],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_completion_hook_run_result(
        &self,
        run_id: &str,
        status: GraphRunStatus,
        output: Option<&Value>,
        summary: Option<&str>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_completion_hook_runs
             SET status = ?1,
                 output = COALESCE(?2, output),
                 summary = COALESCE(?3, summary),
                 completed_at = COALESCE(?4, completed_at),
                 pid = NULL
             WHERE id = ?5",
            params![
                status.as_str(),
                output.map(serde_json::to_string).transpose()?,
                summary,
                completed_at.map(|value| value.timestamp()),
                run_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Every past hook firing for a graph, oldest first — surfaced
    /// via `graph_get`/`canopy graph info` alongside the graph's node runs.
    pub fn list_graph_completion_hook_runs(
        &self,
        graph_id: &str,
    ) -> Result<Vec<GraphCompletionHookRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, status, output, summary, started_at, completed_at, pid, boot_id, event, hook_index, executed_platform, executed_model
             FROM graph_completion_hook_runs WHERE graph_id = ?1 ORDER BY started_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_completion_hook_run_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Mark a graph as having been launched by a hook (CH4). This is a
    /// transient flag that lives only for the duration of the run — it is
    /// cleared when the run completes (via [`Self::clear_graph_hook_launched`]).
    pub fn mark_graph_as_hook_launched(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET hook_launched = 1 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear the hook-launched flag for a graph. Called when the run completes.
    pub fn clear_graph_hook_launched(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET hook_launched = 0 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Whether the graph was launched by a hook (depth = 1).
    pub fn is_graph_hook_launched(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare("SELECT hook_launched FROM graphs WHERE id = ?1")?;
        let result: Option<bool> = stmt
            .query_row(params![graph_id], |row| row.get(0))
            .ok()
            .map(|v: i64| v != 0);
        Ok(result.unwrap_or(false))
    }

    /// Record provenance: which graph and event launched this graph (CH4).
    /// Stored in `graph_hook_launches` for traceability.
    pub fn record_hook_launch_provenance(
        &self,
        target_graph_id: &str,
        source_graph_id: &str,
        event: &str,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_hook_launches (target_graph_id, source_graph_id, event, launched_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                target_graph_id,
                source_graph_id,
                event,
                chrono::Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }
}
