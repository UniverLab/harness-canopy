use super::*;

impl Database {
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
    /// Scoped to `graph_id`: only specs bound to that graph or members of
    /// its active queue run count, so a stale record from another graph (or
    /// a standalone backlog row) never blocks a spec resuming its own work.
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
        graph_id: &str,
    ) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, gs.spec_type, gs.spec_scope, gs.spec_code, gs.intent, gs.review_state, gs.review_tier, gs.review_findings
             FROM graph_specs gs
             LEFT JOIN graphs g ON g.id = gs.graph_id
             WHERE (g.workdir = ?1 OR gs.workdir = ?1)
               AND gs.status IN ('failed', 'interrupted')
               AND (?2 IS NULL OR gs.id != ?2)
               AND (gs.graph_id = ?3 OR gs.id IN (SELECT spec_id FROM queue_members WHERE queue_id = (SELECT active_run_queue_id FROM graphs WHERE id = ?3)))
             ORDER BY gs.updated_at DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map(
            params![workdir, exclude_spec_id, graph_id],
            map_graph_spec_row,
        )?;
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
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, gs.spec_type, gs.spec_scope, gs.spec_code, gs.intent, gs.review_state, gs.review_tier, gs.review_findings
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
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, gs.spec_type, gs.spec_scope, gs.spec_code, gs.intent, gs.review_state, gs.review_tier, gs.review_findings, g.workdir
             FROM graph_specs gs
             LEFT JOIN graphs g ON g.id = gs.graph_id
             WHERE gs.status = 'interrupted' AND gs.spec_end_dirty IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let spec = map_graph_spec_row(row)?;
            let graph_workdir: Option<String> = row.get(25)?;
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

    /// Set (or clear with `None`) a spec's `intent`. The handler decides
    /// whether an empty string becomes `None`; this writes exactly what it is
    /// given, so the review gate and the column can never disagree.
    pub fn set_graph_spec_intent(&self, spec_id: &str, intent: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET intent = ?1, updated_at = ?3 WHERE id = ?2",
            params![intent, spec_id, Utc::now().timestamp_millis()],
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
