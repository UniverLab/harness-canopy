use super::*;

impl Database {
    /// Reset a graph spec back to `Pending`, clearing `started_at` and
    /// `completed_at` unconditionally (unlike [`Self::update_graph_spec_status`],
    /// which only overwrites when a new value is given). Used by `graph_control`.
    ///
    /// `clear_cross_run_attempts` (C19) additionally zeroes the spec's
    /// persisted cross-run attempt counter — but only when the caller passed
    /// this exact spec explicitly. [`Self::reset_graph`] wires that in: an
    /// operator naming a spec by id is the deliberate "I fixed this" signal
    /// decision 6 asks for; a blanket reset of every non-completed spec is
    /// not, so the counter must survive it — otherwise the very relaunch
    /// this budget exists to guard against would silently get a fresh one
    /// every time.
    pub fn reset_graph_spec_status(
        &self,
        spec_id: &str,
        clear_cross_run_attempts: bool,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let updated_at = Utc::now().timestamp_millis();
        let rows = conn.execute(
            "UPDATE graph_specs SET status = ?1, started_at = NULL, completed_at = NULL, spec_start_head = NULL, spec_committed_head = NULL,
                 completed_via = CASE WHEN ?3 THEN NULL ELSE completed_via END,
                 completed_via_reason = CASE WHEN ?3 THEN NULL ELSE completed_via_reason END,
                 completed_via_at = CASE WHEN ?3 THEN NULL ELSE completed_via_at END,
                 cross_run_attempts = CASE WHEN ?3 THEN 0 ELSE cross_run_attempts END,
                 updated_at = ?4
             WHERE id = ?2",
            params![
                GraphSpecStatus::Pending.as_str(),
                spec_id,
                clear_cross_run_attempts,
                updated_at,
            ],
        )?;
        Ok(rows > 0)
    }

    /// FR1: zero one spec's cross-run attempt count and nothing else — the
    /// status, heads and worktree stay as they are. An explicit operator
    /// directive (`graph_control` / `graph_control` with a start node) calls this
    /// before its run starts, so the human ruling clears the limit the same way
    /// a named `graph_control` does.
    pub fn clear_graph_spec_cross_run_attempts(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET cross_run_attempts = 0, updated_at = ?2 WHERE id = ?1",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    /// C19: the spec's persisted cross-run attempt count — how many separate
    /// graph executions it has failed with a genuine (non-infrastructure)
    /// verdict. `0` for a spec that has never failed this way (including
    /// every pre-migration row). See [`Self::increment_graph_spec_cross_run_attempts`].
    pub fn get_graph_spec_cross_run_attempts(&self, spec_id: &str) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT cross_run_attempts FROM graph_specs WHERE id = ?1",
            params![spec_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Increment `spec_id`'s persisted cross-run attempt count and return the
    /// new value, atomically under the single connection lock so a
    /// concurrent read never observes a torn increment. Called by
    /// `GraphEngine::record_spec_attempt` exactly once per spec-execution
    /// that ends in a genuine (non-infrastructure) `Failed` — never for an
    /// infra failure, and never more than once per attempt.
    pub fn increment_graph_spec_cross_run_attempts(&self, spec_id: &str) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE graph_specs SET cross_run_attempts = cross_run_attempts + 1, updated_at = ?2 WHERE id = ?1",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        conn.query_row(
            "SELECT cross_run_attempts FROM graph_specs WHERE id = ?1",
            params![spec_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Administratively transition a standalone spec's status. The transition
    /// is recorded with provenance (completed_via = 'admin') and the given reason.
    pub fn set_spec_admin_status(
        &self,
        spec_id: &str,
        status: GraphSpecStatus,
        reason: &str,
    ) -> Result<SpecAdminStatusOutcome> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        // 1. Check if spec exists
        let mut stmt = conn.prepare("SELECT id, graph_id FROM graph_specs WHERE id = ?1")?;
        let (_, graph_id): (String, Option<String>) = match stmt
            .query_row(params![spec_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?
        {
            Some(row) => row,
            None => return Ok(SpecAdminStatusOutcome::NotFound),
        };

        // 2. Reject if spec is bound to a graph (not standalone)
        if let Some(graph_id) = graph_id {
            return Ok(SpecAdminStatusOutcome::NotStandalone(graph_id));
        }

        // 3. Check for active graph run
        if let Some(run) = active_graph_run_for_spec_locked(&conn, spec_id)? {
            return Ok(SpecAdminStatusOutcome::ActiveRun {
                graph_id: run.graph_id,
                run_id: run.id,
            });
        }

        // 4. Update the spec with admin status
        let now = Utc::now().timestamp();
        let updated_at = Utc::now().timestamp_millis();
        let (completed_at, completed_via_at) = match status {
            GraphSpecStatus::Pending => (None, None),
            _ => (Some(now), Some(now)),
        };

        conn.execute(
            "UPDATE graph_specs
             SET status = ?1, completed_at = ?2, completed_via = 'admin',
                 completed_via_reason = ?3, completed_via_at = ?4, updated_at = ?6
             WHERE id = ?5",
            params![
                status.as_str(),
                completed_at,
                reason,
                completed_via_at,
                spec_id,
                updated_at,
            ],
        )?;

        Ok(SpecAdminStatusOutcome::Success)
    }

    /// The single state-transition path behind `graph_control` — resets a graph
    /// (and, without `specs`, every non-completed spec except an
    /// administratively skipped spec) back to `pending` so it can be
    /// relaunched. Shared by the `graph_control` MCP tool and the scheduler's
    /// auto-reset-and-resume of a `failed` graph on autorun, so there is exactly
    /// one place that knows how to unstick a graph. The returned
    /// `skipped_count` reports administratively skipped specs preserved by a
    /// blanket reset.
    ///
    /// When the graph's last run was against a queue (`active_run_queue_id` is
    /// set), the queue's *members* are what actually need resetting — the
    /// graph's own bound specs are typically empty for a queue run — so they're
    /// folded into the same eligible set as the graph's bound specs, both for
    /// validating an explicit `specs` list and for the "every non-completed"
    /// default. This is the one reset implementation both `graph_control` and
    /// the scheduler's autorun share; it must not be forked.
    pub fn reset_graph(
        &self,
        graph_id: &str,
        specs: Option<&[String]>,
    ) -> Result<GraphResetOutcome> {
        let Some(lp) = self.get_graph(graph_id)? else {
            return Ok(GraphResetOutcome::NotFound);
        };

        // Ground truth for "is this graph actually busy right now" is the
        // `graph_runs` table, not `lp.status` — a sibling node's
        // `graph_report_blocker` can flip status to `paused` while a
        // different node under the same graph keeps executing (status and a
        // run's lifetime are independent). Resetting underneath that live
        // run is exactly what corrupted the 2026-08-05 incident: the reset
        // killed-and-reset the run's spec while its `execute_node` future
        // was still in flight, so its late completion routed an edge and
        // failed the graph out from under the fresh dispatch this reset then
        // launched. Refuse outright instead — the caller's next move is an
        // informed wait or a deliberate kill, not a race.
        if let Some(run) = self.list_running_graph_runs(graph_id)?.into_iter().next() {
            return Ok(GraphResetOutcome::InFlight {
                run_id: run.id,
                node_id: run.node_id,
                started_at: run.started_at,
            });
        }

        let bound_specs = self.list_graph_specs(graph_id)?;
        let queue_specs: Vec<GraphSpec> = match &lp.active_run_queue_id {
            Some(queue_id) => self
                .list_queue_member_spec_ids(queue_id)?
                .into_iter()
                .filter_map(|spec_id| self.get_graph_spec(&spec_id).transpose())
                .collect::<Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let eligible_specs: Vec<&GraphSpec> =
            bound_specs.iter().chain(queue_specs.iter()).collect();

        let valid_ids: std::collections::HashSet<&str> =
            eligible_specs.iter().map(|spec| spec.id.as_str()).collect();

        let target_ids: Vec<String> = match specs {
            Some(ids) => {
                for id in ids {
                    if !valid_ids.contains(id.as_str()) {
                        return Ok(GraphResetOutcome::InvalidSpec(id.clone()));
                    }
                }
                ids.to_vec()
            }
            None => eligible_specs
                .iter()
                .filter(|spec| {
                    spec.status != GraphSpecStatus::Completed
                        && !(spec.status == GraphSpecStatus::Skipped
                            && spec.completed_via.as_deref() == Some("admin"))
                })
                .map(|spec| spec.id.clone())
                .collect(),
        };

        // No spec being reset can have a live `running` node-run row left:
        // the guard above already confirmed zero `running` rows exist
        // anywhere under this graph, and every eligible spec's runs are
        // recorded under this same `graph_id` (bound or drawn live from a
        // queue — see `list_graph_runs_for_graph`), so there is nothing left
        // to terminate here.
        //
        // C19: only an explicitly-named `specs` list clears the target
        // spec(s)' cross-run attempt count — see `reset_graph_spec_status`'s
        // doc. A blanket reset (`specs: None`) resets every non-completed
        // spec's status the same as always, but leaves each one's count
        // exactly where it was.
        let clear_cross_run_attempts = specs.is_some();
        for spec_id in &target_ids {
            self.reset_graph_spec_status(spec_id, clear_cross_run_attempts)?;
        }
        self.reset_graph_status(graph_id)?;

        let skipped_count = if specs.is_none() {
            eligible_specs
                .iter()
                .filter(|spec| {
                    spec.status == GraphSpecStatus::Skipped
                        && spec.completed_via.as_deref() == Some("admin")
                })
                .count()
        } else {
            0
        };

        Ok(GraphResetOutcome::Reset {
            spec_count: target_ids.len(),
            skipped_count,
        })
    }

    pub fn insert_graph_spec(&self, spec: &GraphSpec) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, completed_via, completed_via_reason, completed_via_at, updated_at, spec_type, spec_scope, spec_code)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
            params![
                &spec.id,
                &spec.graph_id,
                &spec.name,
                &spec.description,
                spec.position,
                spec.parallelizable,
                spec.status.as_str(),
                spec.started_at.map(|value| value.timestamp()),
                spec.completed_at.map(|value| value.timestamp()),
                &spec.spec_start_head,
                spec.spec_start_dirty,
                &spec.workdir,
                &spec.spec_committed_head,
                spec.spec_end_dirty,
                spec.spec_end_dirty_paths.as_ref().map(|paths| paths.join("\n")),
                &spec.completed_via,
                &spec.completed_via_reason,
                spec.completed_via_at.map(|value| value.timestamp()),
                Utc::now().timestamp_millis(),
                spec.spec_type.map(|t| t.as_str()),
                &spec.spec_scope,
                &spec.spec_code,
            ],
        )?;
        Ok(())
    }

    pub fn list_graph_specs(&self, graph_id: &str) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, spec_type, spec_scope, spec_code
             FROM graph_specs WHERE graph_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_spec_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_graph_spec(&self, spec_id: &str) -> Result<Option<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, spec_type, spec_scope, spec_code
             FROM graph_specs WHERE id = ?1",
        )?;

        stmt.query_row(params![spec_id], map_graph_spec_row)
            .optional()
            .map_err(Into::into)
    }

    /// CM46: resolve a spec's id from its name within one graph. Used to fill
    /// `graph_events.spec_id` for an event whose only handle was the name.
    pub fn resolve_spec_id_by_name(
        &self,
        graph_id: &str,
        spec_name: &str,
    ) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT id FROM graph_specs WHERE graph_id = ?1 AND name = ?2 LIMIT 1",
            params![graph_id, spec_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// A single spec's own graph (nodes/edges), resolved by spec id alone —
    /// independent of whether the spec is bound to a graph (`graph_specs.graph_id`)
    /// or a standalone queue member. Lets the graph engine drive a queue spec
    /// through the same lookup path as a bound spec (see `graph_engine::run`).
    pub fn get_graph_spec_details(&self, spec_id: &str) -> Result<Option<GraphSpecDetails>> {
        let Some(spec) = self.get_graph_spec(spec_id)? else {
            return Ok(None);
        };
        let nodes = self.list_graph_nodes(spec_id)?;
        let edges = self.list_graph_edges(spec_id)?;
        Ok(Some(GraphSpecDetails { spec, nodes, edges }))
    }

    /// Standalone specs, i.e. the backlog: specs not (yet) assigned to any
    /// graph, optionally filtered by their `workdir` tag and/or status.
    /// `unassigned_only` additionally filters to `graph_id IS NULL` — set it
    /// to `false` to see every spec regardless of graph assignment.
    pub fn list_specs(
        &self,
        workdir: Option<&str>,
        status: Option<GraphSpecStatus>,
        unassigned_only: bool,
        spec_type: Option<SpecType>,
    ) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, spec_type, spec_scope, spec_code
             FROM graph_specs
             WHERE (?1 IS NULL OR workdir = ?1)
               AND (?2 IS NULL OR status = ?2)
               AND (?3 = 0 OR graph_id IS NULL)
               AND (?4 IS NULL OR spec_type = ?4)
             ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(
            params![
                workdir,
                status.map(GraphSpecStatus::as_str),
                unassigned_only,
                spec_type.map(|t| t.as_str())
            ],
            map_graph_spec_row,
        )?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Return the newest write for backlog (unassigned) specs in the given
    /// workdir, independent of the capped list used to render the Knowledge
    /// face. Mirrors [`Database::max_project_knowledge_updated_at`].
    pub fn max_backlog_updated_at(&self, workdir: Option<&str>) -> Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT MAX(updated_at) FROM graph_specs
             WHERE (?1 IS NULL OR workdir = ?1) AND graph_id IS NULL",
            rusqlite::params![workdir],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Update a standalone/backlog spec's name, description, and/or workdir
    /// tag. Unlike [`Self::update_graph_spec_details`] (position/parallelizable,
    /// used by `graph_edit`), this is for `spec_edit update` and never
    /// touches graph assignment or ordering.
    pub fn update_spec_tag_details(
        &self,
        spec_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        workdir: Option<Option<&str>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let updated_at = Utc::now().timestamp_millis();
        let rows = conn.execute(
            "UPDATE graph_specs
             SET name = COALESCE(?1, name),
                 description = COALESCE(?2, description),
                 workdir = CASE WHEN ?3 IS NULL THEN workdir ELSE ?4 END,
                 updated_at = ?6
             WHERE id = ?5",
            params![
                name,
                description,
                workdir.map(|_| 1),
                workdir.flatten(),
                spec_id,
                updated_at,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn list_all_specs_for_conversion(&self) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, spec_type, spec_scope, spec_code
             FROM graph_specs
             ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map([], map_graph_spec_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Delete a spec outright. Callers must enforce the graph-binding guard
    /// (see `spec_edit delete`'s handler) before calling this — this function
    /// performs no such check itself.
    pub fn delete_graph_spec(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM graph_specs WHERE id = ?1", params![spec_id])?;
        Ok(rows > 0)
    }

    /// Unbind a spec from its graph (set graph_id = NULL), making it standalone.
    /// Does NOT delete the spec row — execution history is preserved.
    /// Returns true if a row was updated, false if spec_id not found.
    pub fn unbind_graph_spec(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET graph_id = NULL, updated_at = ?2 WHERE id = ?1 AND graph_id IS NOT NULL",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    /// Record the workdir's git HEAD at the moment a spec starts running.
    /// Called once per spec (not per node) — see [`crate::graph_engine`]'s
    /// `{{spec_start_head}}` placeholder. `head = None` means the workdir
    /// isn't a git repo; the column is cleared rather than left stale.
    #[allow(dead_code)]
    pub fn set_graph_spec_start_head(&self, spec_id: &str, head: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_start_head = ?1, updated_at = ?3 WHERE id = ?2",
            params![head, spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    pub fn set_graph_spec_start_state(
        &self,
        spec_id: &str,
        head: Option<&str>,
        dirty: Option<i64>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_start_head = ?1, spec_start_dirty = ?2, updated_at = ?3 WHERE id = ?4",
            params![head, dirty, Utc::now().timestamp_millis(), spec_id],
        )?;
        Ok(rows > 0)
    }

    /// CM29: the workdir's git-dirty state captured once, at the moment
    /// this spec's attempt actually ends (Completed/Failed/Interrupted) —
    /// same lifecycle as `set_graph_spec_start_state`, the other boundary.
    /// `paths` is truncated to 20 entries by the caller before this is
    /// called; stored newline-joined.
    pub fn set_graph_spec_end_state(
        &self,
        spec_id: &str,
        dirty: Option<i64>,
        paths: &[String],
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let joined = (!paths.is_empty()).then(|| paths.join("\n"));
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_end_dirty = ?1, spec_end_dirty_paths = ?2, updated_at = ?3 WHERE id = ?4",
            params![dirty, joined, Utc::now().timestamp_millis(), spec_id],
        )?;
        Ok(rows > 0)
    }

    /// CM29: the specs that ended without completing, for `workdir` — either
    /// graph-bound specs whose graph's `workdir` matches, or standalone specs
    /// whose own `workdir` tag matches — most recently touched first.
    /// Excludes `exclude_spec_id` (the spec about to be dispatched next) so
    /// a spec that is itself `Interrupted` and about to be legitimately
    /// resumed is never mistaken for "the previous spec" that left the mess
    /// (CM29 guideline). `pending`/`running` are never candidates — those
    /// are either not-yet-run or still in flight, never "previous".
    /// `skipped` is not a candidate either: a spec nobody ran never produced
    /// dirt of its own, so its stale record must not block a later spec
    /// (FR1). `completed` was already excluded, and `failed` stays a
    /// candidate on purpose: a spec that failed leaving uncommitted work must
    /// not have the next spec build (and commit) on top of it.
    pub fn find_dirty_predecessor_specs(
        &self,
        workdir: &str,
        exclude_spec_id: Option<&str>,
    ) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, gs.spec_type, gs.spec_scope, gs.spec_code
             FROM graph_specs gs
             LEFT JOIN graphs g ON g.id = gs.graph_id
             WHERE (g.workdir = ?1 OR gs.workdir = ?1)
               AND gs.status IN ('failed', 'interrupted')
               AND (?2 IS NULL OR gs.id != ?2)
             ORDER BY gs.updated_at DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map(params![workdir, exclude_spec_id], map_graph_spec_row)?;
        let mut specs = Vec::new();
        for spec in rows {
            specs.push(spec?);
        }
        Ok(specs)
    }

    /// CB69: attribution for the dirty-start message — the spec whose most
    /// recent recorded run in THIS graph left the workdir dirty. `graph_runs`
    /// rows (including queue-drawn ones, which always carry the launched
    /// graph's `graph_id`) ordered by run start; each joined to its
    /// `graph_specs` row's `spec_end_dirty`. `spec_end_dirty > 0` implements
    /// "a run of this graph recorded the dirt": NULL means never captured and
    /// 0 means captured-clean, neither "recorded dirt". `ORDER BY ... ,
    /// gr.iteration DESC, gr.id DESC` makes the pick deterministic when two
    /// rows share `started_at` (run `started_at` is stored in seconds).
    /// Returns `None` when no run of this graph recorded dirt — the caller
    /// then says "dirty before this graph ran" and names no spec.
    pub fn find_last_dirty_run_spec(&self, graph_id: &str) -> Result<Option<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, gs.spec_type, gs.spec_scope, gs.spec_code
             FROM graph_runs gr
             JOIN graph_specs gs ON gs.id = gr.spec_id
             WHERE gr.graph_id = ?1
               AND gs.spec_end_dirty IS NOT NULL
               AND gs.spec_end_dirty > 0
             ORDER BY gr.started_at DESC, gr.iteration DESC, gr.id DESC
             LIMIT 1",
            params![graph_id],
            map_graph_spec_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// CM29: specs reconciliation just marked `Interrupted` that haven't had
    /// their end-dirty state captured yet — `spec_end_dirty IS NULL` is the
    /// idempotency guard (a spec resumed and re-ended later overwrites this
    /// again through the normal `run_spec` path, so this only ever picks up
    /// genuinely-uncaptured rows, including ones a previous boot's capture
    /// pass somehow missed). Returns each spec paired with its resolved
    /// workdir (graph's `workdir`, or its own `workdir` tag), `None` when
    /// neither is known.
    pub fn list_interrupted_specs_missing_end_dirty(
        &self,
    ) -> Result<Vec<(GraphSpec, Option<String>)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, gs.spec_type, gs.spec_scope, gs.spec_code, g.workdir
             FROM graph_specs gs
             LEFT JOIN graphs g ON g.id = gs.graph_id
             WHERE gs.status = 'interrupted' AND gs.spec_end_dirty IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let spec = map_graph_spec_row(row)?;
            let graph_workdir: Option<String> = row.get(21)?;
            Ok((spec, graph_workdir))
        })?;
        rows.map(|r| {
            r.map(|(spec, graph_workdir)| {
                let resolved = graph_workdir.or_else(|| spec.workdir.clone());
                (spec, resolved)
            })
            .map_err(Into::into)
        })
        .collect()
    }

    /// Record the workdir's git HEAD immediately after a `commit_rights:
    /// true` node's own execution actually moved it (C15) — see
    /// [`crate::graph_engine`]'s `{{spec_committed_head}}` placeholder.
    /// Unlike `spec_start_head`, which is captured once and answers "has
    /// anything been committed since this spec began", this answers "did
    /// *this run's own committer* land a commit" — the distinction a
    /// concurrent commit from outside this run (another agent, a human
    /// sharing the worktree) would otherwise slip past. Overwritten every
    /// time the committer node visits and moves HEAD again (e.g. a
    /// review/retry cycle that re-commits), so it always reflects the
    /// latest commit this run itself produced.
    pub fn set_graph_spec_committed_head(&self, spec_id: &str, head: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_committed_head = ?1, updated_at = ?3 WHERE id = ?2",
            params![head, spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_spec_details(
        &self,
        spec_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        position: Option<i64>,
        parallelizable: Option<bool>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs
             SET name = COALESCE(?1, name),
                 description = COALESCE(?2, description),
                 position = COALESCE(?3, position),
                 parallelizable = COALESCE(?4, parallelizable),
                 updated_at = ?6
             WHERE id = ?5",
            params![
                name,
                description,
                position,
                parallelizable,
                spec_id,
                Utc::now().timestamp_millis()
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_spec_status(
        &self,
        spec_id: &str,
        status: GraphSpecStatus,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs
             SET status = ?1,
                 started_at = COALESCE(?2, started_at),
                 completed_at = COALESCE(?3, completed_at),
                 updated_at = ?5
             WHERE id = ?4",
            params![
                status.as_str(),
                started_at.map(|value| value.timestamp()),
                completed_at.map(|value| value.timestamp()),
                spec_id,
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(rows > 0)
    }
}
