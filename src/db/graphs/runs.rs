use super::*;

impl Database {
    pub fn insert_graph_run(&self, run: &GraphNodeRun) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                &run.id,
                &run.graph_id,
                &run.spec_id,
                &run.node_id,
                run.status.as_str(),
                run.input
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.output
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.started_at.timestamp(),
                run.completed_at.map(|value| value.timestamp()),
                run.iteration,
                run.pid,
                &run.boot_id,
                &run.session_id,
                &run.executed_platform,
                &run.executed_model,
            ],
        )?;
        Ok(())
    }

    /// Record the OS process-group leader spawned for `run_id`'s node
    /// execution, and the boot it was spawned under. Called right after a
    /// successful `spawn()` — before that, the run row (inserted by the
    /// caller before execution starts) has `pid = NULL`, meaning "no live
    /// process to kill" (e.g. a gate node, or an agent/check node that
    /// hasn't finished spawning yet).
    pub fn set_graph_run_pid(&self, run_id: &str, pid: i64, boot_id: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs SET pid = ?1, boot_id = ?2 WHERE id = ?3",
            params![pid, boot_id, run_id],
        )?;
        Ok(rows > 0)
    }

    /// Record the harness session id captured for a node run (RS1), so the
    /// session can be resumed later (RS2/RS3).
    pub fn set_graph_run_session_id(&self, run_id: &str, session_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs SET session_id = ?1 WHERE id = ?2",
            params![session_id, run_id],
        )?;
        Ok(rows > 0)
    }

    /// RS2/CM26: the earlier run this run's session was continued from, if any.
    /// A resumed run's own row carries the SAME session_id it continued
    /// (`run_agent_process` writes it back verbatim, never a fresh uuid — see
    /// its RS2 comment), while a cold-started run always mints a brand-new
    /// session id (or none) that can never collide with an earlier row's. So a
    /// match here can only mean this run resumed that earlier one. Ordered by
    /// `rowid` (insertion order) rather than `started_at` (only second-
    /// granularity) to disambiguate deterministically when two runs on the same
    /// node start within the same second. Display-only — never consulted by the
    /// engine's own resume logic, which uses `resumable_sessions` in-memory.
    pub fn find_resumed_from_run_id(
        &self,
        run_id: &str,
        node_id: &str,
        session_id: &str,
    ) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let found: Option<String> = conn
            .query_row(
                "SELECT id FROM graph_runs
                 WHERE session_id = ?1 AND node_id = ?2 AND id != ?3
                   AND rowid < (SELECT rowid FROM graph_runs WHERE id = ?3)
                 ORDER BY rowid DESC LIMIT 1",
                params![session_id, node_id, run_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }

    /// The active (`running`) node run for `spec_id`, if any. Mirrors
    /// [`Self::get_active_graph_run_for_node`] but scoped to a whole spec —
    /// used by `graph_reset`, which resets a spec wholesale rather than one
    /// node at a time.
    pub fn get_active_graph_run_for_spec(&self, spec_id: &str) -> Result<Option<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        active_graph_run_for_spec_locked(&conn, spec_id).map_err(Into::into)
    }

    /// Every node run still `running` across every graph — used at daemon
    /// shutdown to terminate every process this boot owns before exiting.
    pub fn list_all_running_graph_runs(&self) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE status = 'running'",
        )?;
        let rows = stmt.query_map(params![], map_graph_run_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn list_graph_runs_for_spec(&self, spec_id: &str) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE spec_id = ?1 ORDER BY started_at ASC, iteration ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_graph_run_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// All node runs recorded against `graph_id`, regardless of whether the
    /// spec they belong to is bound (`graph_specs.graph_id`) or was picked up
    /// live from a queue (queue members always keep `graph_id: None` on their
    /// own row — see `GraphEngine::run_graph`). `graph_runs.graph_id` is set on
    /// every insert either way, so this is the only reliable way to find a
    /// queue-driven graph's current/recent activity without a queue id in hand.
    pub fn list_graph_runs_for_graph(&self, graph_id: &str) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE graph_id = ?1 ORDER BY started_at ASC, iteration ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_run_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Page through a graph's node runs, most-recent-first, optionally
    /// narrowed to one spec and/or one node — the query a failure
    /// investigation needs (`graph_node_runs_list`): find what ran, in what
    /// order, without knowing a node id ahead of time. Unlike
    /// [`Self::list_graph_runs_for_graph`] (oldest-first, unbounded — built for
    /// the engine replaying a whole run), this is bounded by `limit`/`offset`
    /// so a graph with hundreds of runs stays a usable response.
    ///
    /// `graph_id` alone rides `idx_graph_runs_graph_started(graph_id,
    /// started_at DESC)` directly, matching this query's default order.
    /// Adding `spec_id`/`node_id` applies as a residual filter on top of that
    /// same index scan — both columns already carry their own index
    /// (`idx_graph_runs_spec_started`, `idx_graph_runs_node_iteration`) for
    /// other call sites, but the scan here is bounded by `graph_id` first
    /// either way, so no additional composite index is needed.
    pub fn list_graph_node_runs(
        &self,
        graph_id: &str,
        spec_id: Option<&str>,
        node_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let mut sql = String::from(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE graph_id = ?",
        );
        let mut query_params: Vec<&dyn rusqlite::ToSql> = vec![&graph_id];
        if let Some(spec_id) = spec_id.as_ref() {
            sql.push_str(" AND spec_id = ?");
            query_params.push(spec_id);
        }
        if let Some(node_id) = node_id.as_ref() {
            sql.push_str(" AND node_id = ?");
            query_params.push(node_id);
        }
        sql.push_str(" ORDER BY started_at DESC, iteration DESC LIMIT ? OFFSET ?");
        query_params.push(&limit);
        query_params.push(&offset);

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(query_params.as_slice(), map_graph_run_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn count_graph_node_runs_filtered(
        &self,
        graph_id: &str,
        spec_id: Option<&str>,
        node_id: Option<&str>,
    ) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let mut sql = String::from("SELECT COUNT(*) FROM graph_runs WHERE graph_id = ?");
        let mut query_params: Vec<&dyn rusqlite::ToSql> = vec![&graph_id];
        if let Some(spec_id) = spec_id.as_ref() {
            sql.push_str(" AND spec_id = ?");
            query_params.push(spec_id);
        }
        if let Some(node_id) = node_id.as_ref() {
            sql.push_str(" AND node_id = ?");
            query_params.push(node_id);
        }

        conn.query_row(&sql, query_params.as_slice(), |row| row.get(0))
            .map_err(Into::into)
    }

    pub fn get_graph_run(&self, run_id: &str) -> Result<Option<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE id = ?1",
        )?;

        stmt.query_row(params![run_id], map_graph_run_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_active_graph_run_for_node(&self, node_id: &str) -> Result<Option<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs
             WHERE node_id = ?1 AND status = 'running'
             ORDER BY started_at DESC
             LIMIT 1",
        )?;

        stmt.query_row(params![node_id], map_graph_run_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_graph_run_result(
        &self,
        run_id: &str,
        status: GraphRunStatus,
        output: Option<&Value>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs
             SET status = ?1,
                 output = COALESCE(?2, output),
                 completed_at = COALESCE(?3, completed_at),
                 pid = NULL
             WHERE id = ?4",
            params![
                status.as_str(),
                output.map(serde_json::to_string).transpose()?,
                completed_at.map(|value| value.timestamp()),
                run_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Mark a running node run as interrupted by an operator. Does NOT kill
    /// the process — the caller is responsible for termination.
    pub fn interrupt_graph_run(&self, run_id: &str, reason: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs
             SET status = ?1,
                 output = ?2,
                 completed_at = ?3,
                 pid = NULL
             WHERE id = ?4 AND status = 'running'",
            params![
                GraphRunStatus::Interrupted.as_str(),
                serde_json::to_string(&serde_json::json!({
                    "interrupted": true,
                    "reason": reason
                }))?,
                chrono::Utc::now().timestamp(),
                run_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// CB31: flag every run of `node_ids` under `spec_id` at `iteration` as
    /// `paused_through` — the node(s) finalized with their own verdict while a
    /// wait-for-completion `graph_pause` was pending. A single node has one such
    /// row; an ensemble step has one per member plus its join. The flag tells
    /// [`Self::last_spec_node_run_was_operator_paused`] that `graph_continue`
    /// re-executing this cursor must reuse the same iteration number rather
    /// than spend a fresh one. Returns the number of rows flagged.
    pub fn mark_spec_node_runs_paused_through(
        &self,
        spec_id: &str,
        node_ids: &[String],
        iteration: i64,
    ) -> Result<usize> {
        if node_ids.is_empty() {
            return Ok(0);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let placeholders = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "UPDATE graph_runs SET paused_through = 1
             WHERE spec_id = ?1 AND iteration = ?2 AND node_id IN ({placeholders})"
        );
        let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(spec_id.to_string()), Box::new(iteration)];
        for id in node_ids {
            sql_params.push(Box::new(id.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| b.as_ref()).collect();
        let rows = conn.execute(&sql, refs.as_slice())?;
        Ok(rows)
    }

    /// CB31: whether the most recent run among `node_ids` under `spec_id` was
    /// ended by an operator rather than by the node's own work — recorded
    /// `Interrupted` (explicit `graph_pause(interrupt: true)`), or finalized
    /// with its own verdict while a wait-for-completion pause was pending
    /// (`paused_through`). The engine consults this before incrementing the
    /// per-node iteration counter on a resumed dispatch: an operator pause or
    /// interrupt is not a node attempt and must not consume one of the node's
    /// `DEFAULT_MAX_ITERATIONS_PER_NODE`.
    pub fn last_spec_node_run_was_operator_paused(
        &self,
        spec_id: &str,
        node_ids: &[String],
    ) -> Result<bool> {
        if node_ids.is_empty() {
            return Ok(false);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let placeholders = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "SELECT status, paused_through FROM graph_runs
             WHERE spec_id = ?1 AND node_id IN ({placeholders})
             ORDER BY started_at DESC, rowid DESC LIMIT 1"
        );
        let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(spec_id.to_string())];
        for id in node_ids {
            sql_params.push(Box::new(id.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| b.as_ref()).collect();
        let row = conn
            .query_row(&sql, refs.as_slice(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .optional()?;
        Ok(match row {
            Some((status, paused_through)) => {
                GraphRunStatus::from_str(&status) == GraphRunStatus::Interrupted
                    || paused_through != 0
            }
            None => false,
        })
    }

    /// CT3: append one live-output chunk for a running check node. Called
    /// from the engine's stdout/stderr reader tasks; `stream` must be
    /// `stdout` or `stderr` (enforced by the table CHECK constraint).
    pub fn append_graph_run_output(&self, run_id: &str, stream: &str, chunk: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_run_output (run_id, stream, chunk) VALUES (?1, ?2, ?3)",
            params![run_id, stream, chunk],
        )?;
        Ok(())
    }

    /// CT3: live tail for the tail dialog. Reads each stream's chunks
    /// newest-first and stops once it has [`TAIL_MAX_BYTES`] of that stream,
    /// so a node that has emitted gigabytes still costs only a few dozen row
    /// reads per poll (NFR: following a very chatty node must not block the
    /// TUI or consume unbounded memory). Keeps the last `max_lines` lines of
    /// each stream; when neither stream has any chunks (after completion when
    /// only the snapshot remains, or a zero-output hang) falls back to the
    /// `graph_runs.stdout_tail`/`stderr_tail` snapshot columns.
    pub fn get_graph_run_tail(&self, run_id: &str, max_lines: usize) -> Result<(String, String)> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        // Pull one stream's trailing chunks in reverse insertion order,
        // stopping as soon as we hold enough bytes to satisfy the byte cap
        // that `take_last_bytes` would apply anyway. Bounds the scan to
        // O(TAIL_MAX_BYTES) regardless of how much total output exists.
        let read_stream_tail = |stream: &str| -> Result<(String, bool)> {
            let mut stmt = conn.prepare(
                "SELECT chunk FROM graph_run_output
                 WHERE run_id = ?1 AND stream = ?2
                 ORDER BY id DESC",
            )?;
            let mut rows = stmt.query(params![run_id, stream])?;
            let mut parts: Vec<String> = Vec::new();
            let mut bytes = 0usize;
            let mut any = false;
            while let Some(row) = rows.next()? {
                any = true;
                let chunk: String = row.get(0)?;
                bytes += chunk.len();
                parts.push(chunk);
                if bytes >= TAIL_MAX_BYTES {
                    break;
                }
            }
            parts.reverse();
            Ok((parts.concat(), any))
        };

        let (stdout_buf, any_stdout) = read_stream_tail("stdout")?;
        let (stderr_buf, any_stderr) = read_stream_tail("stderr")?;

        if !any_stdout && !any_stderr {
            let (snap_out, snap_err): (Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT stdout_tail, stderr_tail FROM graph_runs WHERE id = ?1",
                    params![run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or((None, None));
            return Ok((
                take_last_lines(snap_out.as_deref().unwrap_or(""), max_lines),
                take_last_lines(snap_err.as_deref().unwrap_or(""), max_lines),
            ));
        }
        Ok((
            take_last_lines(take_last_bytes(&stdout_buf), max_lines),
            take_last_lines(take_last_bytes(&stderr_buf), max_lines),
        ))
    }

    /// CT3: cache the final truncated per-stream tails on the run row at
    /// completion, so the dialog can show post-completion output without
    /// scanning the chunk table.
    pub fn set_graph_run_tail_snapshot(
        &self,
        run_id: &str,
        stdout_tail: Option<&str>,
        stderr_tail: Option<&str>,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE graph_runs SET stdout_tail = ?1, stderr_tail = ?2 WHERE id = ?3",
            params![stdout_tail, stderr_tail, run_id],
        )?;
        Ok(())
    }
}
