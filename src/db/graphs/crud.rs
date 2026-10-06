use super::*;

impl Database {
    pub fn insert_graph(&self, lp: &Graph) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let (trigger_type, trigger_config) = encode_graph_trigger(lp.trigger.as_ref())?;
        let on_completed = encode_graph_completion_hook(
            lp.hooks
                .get(&GraphHookEvent::OnCompleted)
                .and_then(|v| v.first()),
        )?;
        let hooks = encode_graph_hooks(&lp.hooks)?;
        conn.execute(
            "INSERT INTO graphs (id, name, description, workdir, status, trigger_type, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                &lp.id,
                &lp.name,
                &lp.description,
                &lp.workdir,
                lp.status.as_str(),
                trigger_type,
                trigger_config,
                lp.created_at.timestamp(),
                lp.started_at.map(|value| value.timestamp()),
                lp.completed_at.map(|value| value.timestamp()),
                lp.autorun_at.map(|value| value.timestamp()),
                &lp.active_run_queue_id,
                on_completed,
                lp.auto_continue_at.map(|value| value.timestamp()),
                &lp.auto_continue_action,
                lp.archived,
                lp.paused_by_reconciliation,
                &lp.infra_node_id,
                hooks,
                lp.allow_dirty_start,
            ],
        )?;
        Ok(())
    }

    pub fn update_graph_details(
        &self,
        graph_id: &str,
        name: Option<&str>,
        description: Option<Option<&str>>,
        workdir: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs
             SET name = COALESCE(?1, name),
                 description = CASE
                     WHEN ?2 IS NULL THEN description
                     ELSE ?3
                 END,
                 workdir = COALESCE(?4, workdir)
             WHERE id = ?5",
            params![
                name,
                description.map(|_| 1),
                description.flatten(),
                workdir,
                graph_id
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn get_graph(&self, graph_id: &str) -> Result<Option<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
             FROM graphs WHERE id = ?1",
        )?;

        stmt.query_row(params![graph_id], map_graph_row)
            .optional()
            .map_err(Into::into)
    }

    /// List graphs, optionally narrowed to one `workdir`. `include_archived`
    /// controls whether archived graphs are included: `false` is the
    /// "browsing" view (sidebar, `canopy graph list`, MCP `graph_list`) — an
    /// archived graph is excluded by the query itself (not filtered in
    /// memory), never by loading every row and discarding some. Pass `true`
    /// for a lookup that must still resolve an archived graph (e.g. `canopy
    /// graph info` by id/name) or to list the archived set for the archive
    /// view.
    pub fn list_graphs(&self, workdir: Option<&str>, include_archived: bool) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let archived_clause = if include_archived {
            ""
        } else {
            " AND archived = 0"
        };
        let sql = if workdir.is_some() {
            format!(
                "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
                 FROM graphs WHERE workdir = ?1{archived_clause} ORDER BY created_at DESC"
            )
        } else {
            format!(
                "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
                 FROM graphs WHERE 1=1{archived_clause} ORDER BY created_at DESC"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(workdir) = workdir {
            stmt.query_map(params![workdir], map_graph_row)?
        } else {
            stmt.query_map([], map_graph_row)?
        };

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn delete_graph(&self, graph_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute("DELETE FROM graphs WHERE id = ?1", params![graph_id])?;
        Ok(())
    }

    /// Archive a graph: it leaves every browsing listing (`list_graphs` with
    /// `include_archived: false`) but its row, specs, and run history are
    /// untouched — a single-row, atomic flag flip, never a delete/recreate
    /// or a move to another table (the graph keeps its id and every foreign
    /// key into it). Refuses a `running` graph (archiving is for work that's
    /// finished with; pause it first) and a graph that's already archived.
    pub fn archive_graph(&self, graph_id: &str) -> Result<ArchiveGraphOutcome> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT status, archived FROM graphs WHERE id = ?1",
                params![graph_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, archived)) = row else {
            return Ok(ArchiveGraphOutcome::NotFound);
        };
        if GraphStatus::from_str(&status) == GraphStatus::Running
            || GraphStatus::from_str(&status) == GraphStatus::Pausing
        {
            return Ok(ArchiveGraphOutcome::Running);
        }
        if archived != 0 {
            return Ok(ArchiveGraphOutcome::AlreadyArchived);
        }
        conn.execute(
            "UPDATE graphs SET archived = 1 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(ArchiveGraphOutcome::Archived)
    }

    /// Restore an archived graph back to the main browsing list. A single-row,
    /// atomic flag flip — everything the graph carries (specs, run history)
    /// was never touched by archiving in the first place. Returns `true` when
    /// a row was actually flipped (i.e. it existed and was archived).
    pub fn restore_graph(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET archived = 0 WHERE id = ?1 AND archived = 1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Count of archived graphs — the always-visible number that makes the
    /// archive non-invisible (see the F4-archive spec). A dedicated `COUNT(*)`
    /// query, not `list_graphs(...).len()`, so the main view never pays for
    /// loading every archived row just to show a number.
    pub fn count_archived_graphs(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT COUNT(*) FROM graphs WHERE archived = 1",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn update_graph_infra_node_id(
        &self,
        graph_id: &str,
        infra_node_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET infra_node_id = ?1 WHERE id = ?2",
            params![infra_node_id, graph_id],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_allow_dirty_start(&self, graph_id: &str, value: bool) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE graphs SET allow_dirty_start = ?1 WHERE id = ?2",
            params![value, graph_id],
        )?;
        Ok(())
    }

    /// Whether any graph is currently running.  The update restart guard
    /// uses the same exact status predicate as the cleanup busy check.
    pub fn has_running_graphs(&self) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graphs WHERE status = 'running'",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Replace a graph's trigger (cron/watch/manual). Passing `None` clears any
    /// existing trigger, making the graph manual-only.
    pub fn update_graph_trigger(&self, graph_id: &str, trigger: Option<&Trigger>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let (trigger_type, trigger_config) = encode_graph_trigger(trigger)?;
        let rows = conn.execute(
            "UPDATE graphs SET trigger_type = ?1, trigger_config = ?2 WHERE id = ?3",
            params![trigger_type, trigger_config, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Replace a graph's `on_completed` hook config (N2). Passing `None`
    /// clears it, making the graph's completion behave exactly as it did
    /// before N2 (no hook). Writes both the legacy `on_completed` column
    /// and the new `hooks` map for rollback compatibility. Preserves
    /// hooks registered for other events.
    pub fn update_graph_completion_hook(
        &self,
        graph_id: &str,
        hook: Option<&GraphCompletionHook>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let on_completed = encode_graph_completion_hook(hook)?;
        // Read the current hooks map, update on_completed, write it back
        let current_hooks_json: Option<String> = conn
            .query_row(
                "SELECT hooks FROM graphs WHERE id = ?1",
                params![graph_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let mut hooks_map: std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>> =
            current_hooks_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_default();
        if let Some(h) = hook {
            hooks_map.insert(GraphHookEvent::OnCompleted, vec![h.clone()]);
        } else {
            hooks_map.remove(&GraphHookEvent::OnCompleted);
        }
        let hooks_json = encode_graph_hooks(&hooks_map)?;
        let rows = conn.execute(
            "UPDATE graphs SET on_completed = ?1, hooks = ?2 WHERE id = ?3",
            params![on_completed, hooks_json, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Replace the full hooks map for a graph. Used by MCP graph_update with
    /// the new event-keyed `hooks` parameter.
    pub fn update_graph_hooks(
        &self,
        graph_id: &str,
        hooks: &std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let hooks_json = encode_graph_hooks(hooks)?;
        // Also sync the legacy `on_completed` column for rollback compat
        let on_completed_legacy = hooks
            .get(&GraphHookEvent::OnCompleted)
            .and_then(|v| v.first())
            .map(serde_json::to_string)
            .transpose()?;
        let rows = conn.execute(
            "UPDATE graphs SET hooks = ?1, on_completed = ?2 WHERE id = ?3",
            params![hooks_json, on_completed_legacy, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Graphs that fire on a cron schedule (their trigger is `Cron`).
    pub fn list_cron_graphs(&self) -> Result<Vec<Graph>> {
        self.list_graphs_where_trigger("cron")
    }

    /// Graphs that fire on a file-system watch (their trigger is `Watch`).
    pub fn list_watch_graphs(&self) -> Result<Vec<Graph>> {
        self.list_graphs_where_trigger("watch")
    }

    fn list_graphs_where_trigger(&self, trigger_type: &str) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
             FROM graphs WHERE trigger_type = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![trigger_type], map_graph_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
