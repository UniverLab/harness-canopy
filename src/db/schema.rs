use anyhow::Result;
use rusqlite::Connection;

use super::{Database, BASE_SCHEMA_BATCHES};

impl Database {
    /// Shortens a `rusqlite` batch/statement failure to the SQLite error
    /// reason plus just the failing statement's first line. Without this,
    /// `rusqlite::Error::SqlInputError`'s `Display` embeds the *entire
    /// remaining* SQL text passed to the failing `prepare()` call — for a
    /// failure early in the base schema batch that's hundreds of lines
    /// (this is what `canopy doctor` dumped for CB56). Because
    /// `execute_batch` re-slices its input to start at the next
    /// unconsumed statement each iteration, the remaining SQL always
    /// *starts* with the statement that just failed, so its first line is
    /// exactly the right thing to show.
    pub(super) fn describe_sql_failure(step: &str, e: &rusqlite::Error) -> anyhow::Error {
        match e {
            rusqlite::Error::SqlInputError { msg, sql, .. } => {
                let first_line = sql
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("");
                anyhow::anyhow!("{step}: {msg} (statement: {first_line})")
            }
            other => anyhow::anyhow!("{step}: {other}"),
        }
    }

    /// Create every base table and index (fresh databases); idempotent via
    /// `IF NOT EXISTS`.
    pub(super) fn create_base_schema(conn: &Connection) -> Result<()> {
        for batch in BASE_SCHEMA_BATCHES {
            conn.execute_batch(batch)
                .map_err(|e| Self::describe_sql_failure("base schema batch", &e))?;
        }
        Ok(())
    }
}

impl Database {
    /// Add graph `archived`/`paused_by_reconciliation`/`infra_node_id`/`allow_dirty_start`.
    pub(in crate::db) fn migrate_graph_roster_columns(conn: &Connection) -> Result<()> {
        // Archiving (F4): a graph can leave the sidebar's browsing list
        // without losing its row, specs, or run history — the reversible
        // alternative to `delete_graph`. A constant `DEFAULT 0` means every
        // pre-existing graph reads as not-archived with no data movement.
        let has_archived: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'archived'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_archived {
            conn.execute(
                "ALTER TABLE graphs ADD COLUMN archived INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // C1: distinguishes a graph `reconcile_orphaned_graphs` paused after an
        // unclean daemon exit from one an operator paused on purpose, so a
        // pending `autorun_at` schedule can survive the former but still be
        // blocked by the latter — see
        // [`crate::domain::graphs::Graph::is_autorun_due`]. `DEFAULT 0` means
        // every pre-existing `Paused` graph reads as operator-paused, which is
        // the safe assumption for a row this migration has no way to
        // distinguish retroactively.
        let has_paused_by_reconciliation: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'paused_by_reconciliation'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_paused_by_reconciliation {
            conn.execute(
                "ALTER TABLE graphs ADD COLUMN paused_by_reconciliation INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // CM2: optional pre-wired target for infrastructure failures. When
        // set, new agent/check/gate nodes auto-create a `Error` edge to
        // this node. `NULL` on every pre-existing row (no auto-wiring).
        let has_infra_node_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'infra_node_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_infra_node_id {
            conn.execute("ALTER TABLE graphs ADD COLUMN infra_node_id TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // CM29: downgrades the dirty-start refusal (graph_run/autorun) to a
        // warning instead of refusing to launch. `DEFAULT 0` — every
        // pre-existing graph keeps today's (implicit) refusal behavior.
        let has_allow_dirty_start: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'allow_dirty_start'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_allow_dirty_start {
            conn.execute(
                "ALTER TABLE graphs ADD COLUMN allow_dirty_start INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add graph-spec `spec_committed_head`/`spec_end_dirty*` columns.
    pub(in crate::db) fn migrate_spec_commit_evidence_columns(conn: &Connection) -> Result<()> {
        // `spec_committed_head` (C15): the workdir's git HEAD immediately
        // after a `commit_rights: true` node's own execution actually moved
        // it — as opposed to `spec_start_head`, which only proves *some*
        // commit landed since the spec began and is satisfied just as well
        // by a concurrent commit from outside this run sharing the same
        // worktree. `NULL` on every pre-existing row (no historical spec's
        // committing node was ever tracked this way) and on any row where
        // the graph never named a committer. Additive.
        let has_spec_committed_head: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'spec_committed_head'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_committed_head {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN spec_committed_head TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // CM29: git state captured once more when a spec's attempt actually
        // ends (Completed/Failed/Interrupted), alongside the existing
        // `spec_start_dirty`. `spec_end_dirty_paths` is the first 20 dirty
        // paths, newline-joined (never contains a literal newline itself —
        // git status paths don't). `NULL` on every pre-existing row.
        let has_spec_end_dirty: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'spec_end_dirty'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_end_dirty {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN spec_end_dirty INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        let has_spec_end_dirty_paths: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'spec_end_dirty_paths'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_end_dirty_paths {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN spec_end_dirty_paths TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// C19: add `graph_specs.cross_run_attempts`.
    pub(in crate::db) fn migrate_spec_cross_run_attempts_column(conn: &Connection) -> Result<()> {
        // `cross_run_attempts` (C19): how many separate graph executions this
        // spec has failed with a genuine (non-infrastructure) verdict.
        // Unlike the per-node iteration budget `run_spec` tracks in memory
        // for the duration of one execution, this is persisted so it
        // survives `graph_reset`, a relaunch, and a daemon restart — the
        // whole point being that an unsatisfiable spec doesn't get a fresh
        // budget every time an operator resets and relaunches after a quota
        // failure. `DEFAULT 0` means every pre-existing spec reads as never
        // having failed under this counter, which is correct: it didn't
        // exist to count anything before now. See
        // `Database::increment_graph_spec_cross_run_attempts` and
        // `GraphEngine::record_spec_attempt`.
        let has_cross_run_attempts: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'cross_run_attempts'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_cross_run_attempts {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN cross_run_attempts INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// CT17: add graph-spec `updated_at`/`content_touched_at` and their indexes.
    pub(in crate::db) fn migrate_intelligence_touch_columns(conn: &Connection) -> Result<()> {
        // CT17: dedicated change signal for project knowledge, independent of the
        // public `updated_at` (seconds, exposed via MCP in daemon/handler.rs) so
        // this migration cannot change that column's unit or meaning.
        let has_spec_updated_at: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'updated_at'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_updated_at {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        conn.execute(
            "UPDATE graph_specs SET updated_at = COALESCE(started_at, completed_at, strftime('%s', 'now')) WHERE updated_at = 0",
            [],
        )?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_graph_specs_workdir_updated ON graph_specs(workdir, updated_at DESC)",
            [],
        )?;
        let has_content_touched_at: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('intelligence_nodes') WHERE name = 'content_touched_at'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_content_touched_at {
            conn.execute(
                "ALTER TABLE intelligence_nodes ADD COLUMN content_touched_at INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| Self::describe_sql_failure("CT17 content_touched_at migration", &e))?;
        }
        conn.execute(
            "DROP INDEX IF EXISTS idx_intelligence_nodes_project_hash_updated",
            [],
        )
        .map_err(|e| Self::describe_sql_failure("CT17 content_touched_at migration", &e))?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_project_hash_touched ON intelligence_nodes(project_hash, content_touched_at DESC)",
            [],
        )
        .map_err(|e| Self::describe_sql_failure("CT17 content_touched_at migration", &e))?;
        Ok(())
    }

    /// Add ensemble `kind`/`round_robin_index`/`quorum_grace_minutes` columns.
    pub(in crate::db) fn migrate_ensemble_execution_columns(conn: &Connection) -> Result<()> {
        let has_ensemble_kind: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'kind'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_ensemble_kind {
            conn.execute(
                "ALTER TABLE ensembles ADD COLUMN kind TEXT NOT NULL DEFAULT 'parallel'",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        let has_round_robin_index: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'round_robin_index'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_round_robin_index {
            conn.execute(
                "ALTER TABLE ensembles ADD COLUMN round_robin_index INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // CM24: quorum_grace_minutes — nullable, None = old behaviour.
        let has_quorum_grace: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'quorum_grace_minutes'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_quorum_grace {
            conn.execute(
                "ALTER TABLE ensembles ADD COLUMN quorum_grace_minutes INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        let has_bp_quorum_grace: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensemble_blueprints') WHERE name = 'quorum_grace_minutes'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_bp_quorum_grace {
            conn.execute(
                "ALTER TABLE ensemble_blueprints ADD COLUMN quorum_grace_minutes INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// CT3: add the `graph_run_output` table and run `stdout_tail`/`stderr_tail`.
    pub(in crate::db) fn migrate_graph_run_output_tails(conn: &Connection) -> Result<()> {
        // CT3: live tailing of check-node output. Chunks of stdout/stderr are
        // appended while the node runs so the TUI can poll them; the final
        // truncated tails are cached on `graph_runs` at completion for an
        // instant post-completion view. Additive and idempotent.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS graph_run_output (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL REFERENCES graph_runs(id) ON DELETE CASCADE,
                stream TEXT NOT NULL CHECK (stream IN ('stdout','stderr')),
                chunk TEXT NOT NULL,
                ts INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );
            CREATE INDEX IF NOT EXISTS idx_graph_run_output_run_ts
                ON graph_run_output(run_id, ts ASC);
            CREATE INDEX IF NOT EXISTS idx_graph_run_output_run_stream_id
                ON graph_run_output(run_id, stream, id DESC);",
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        for column in ["stdout_tail", "stderr_tail"] {
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graph_runs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                let sql = format!("ALTER TABLE graph_runs ADD COLUMN {column} TEXT");
                conn.execute(&sql, [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// CB31: add `graph_runs.paused_through`.
    pub(in crate::db) fn migrate_run_paused_through_column(conn: &Connection) -> Result<()> {
        // CB31: a node run finalized with its own verdict while a
        // wait-for-completion `graph_pause` was pending is flagged here so
        // `graph_continue` re-executing that node does not spend one of its
        // `DEFAULT_MAX_ITERATIONS_PER_NODE` — an operator pause is not a node
        // attempt. Additive and idempotent; NOT NULL DEFAULT 0 backfills
        // every existing row as "not paused through".
        let has_paused_through: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_runs') WHERE name = 'paused_through'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_paused_through {
            conn.execute(
                "ALTER TABLE graph_runs ADD COLUMN paused_through INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// CH1: add graph `hooks`, backfill it, and add hook-run `event`/`hook_index`.
    pub(in crate::db) fn migrate_graph_hook_event_columns(conn: &Connection) -> Result<()> {
        // Event-keyed hooks (CH1): the graph's hooks stored as a JSON map
        // from event name to an ordered array of agent payloads, while
        // retaining `on_completed` for rollback/old fixtures. On database
        // open, add `hooks` if missing and idempotently backfill every
        // non-NULL legacy `on_completed` object as
        // `{"on_completed":[object]}`. Reads use `hooks` with a fallback
        // to the legacy column.
        let has_hooks: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'hooks'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_hooks {
            conn.execute("ALTER TABLE graphs ADD COLUMN hooks TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        // Backfill (unconditional): covers databases where `hooks` already
        // exists but legacy `on_completed` rows were never migrated.
        conn.execute(
            "UPDATE graphs SET hooks = json_object('on_completed', json_array(json(on_completed)))
             WHERE on_completed IS NOT NULL AND (hooks IS NULL OR hooks = '')",
            [],
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;

        // `event`/`hook_index` on `graph_completion_hook_runs` (CH1):
        // every new hook run records which event it served and its
        // position within that event's hook list. Old rows default to
        // `on_completed`/`0` — the only event that existed before CH1.
        for (column, definition) in [
            ("event", "TEXT NOT NULL DEFAULT 'on_completed'"),
            ("hook_index", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graph_completion_hook_runs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                let sql = format!(
                    "ALTER TABLE graph_completion_hook_runs ADD COLUMN {column} {definition}"
                );
                conn.execute(&sql, [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// CM45: add `graph_completion_hook_runs.hook_config` — the JSON config of
    /// the hook that actually fired. Additive and idempotent; pre-migration
    /// rows keep NULL (the config was never captured for them).
    pub(in crate::db) fn migrate_graph_hook_config_column(conn: &Connection) -> Result<()> {
        let has_column: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_completion_hook_runs') WHERE name = 'hook_config'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_column {
            conn.execute(
                "ALTER TABLE graph_completion_hook_runs ADD COLUMN hook_config TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// CH4: add graph `hook_launched` and the `graph_hook_launches` table.
    pub(in crate::db) fn migrate_hook_launch_tracking(conn: &Connection) -> Result<()> {
        // CH4: hook-launched depth tracking and provenance.
        // `hook_launched` on `graphs`: whether this graph was launched by a hook
        // (depth cap enforcement). Defaults to 0 so existing graphs are unaffected.
        let has_hook_launched: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'hook_launched'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_hook_launched {
            conn.execute(
                "ALTER TABLE graphs ADD COLUMN hook_launched INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        // Provenance for hook-launched graphs: which source graph and event
        // triggered this launch. Append-only, for traceability.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS graph_hook_launches (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                target_graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                source_graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                event TEXT NOT NULL,
                launched_at INTEGER NOT NULL
            );",
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }

    /// CB43: add executed platform/model columns and their indexes.
    pub(in crate::db) fn migrate_executed_platform_columns(conn: &Connection) -> Result<()> {
        // CB43: record which platform+model actually executed a run. Two
        // nullable TEXT columns on the row already written — no new table, no
        // second write. Pre-migration rows keep NULL (omitted, never guessed).
        for (table, column) in [
            ("graph_runs", "executed_platform"),
            ("graph_runs", "executed_model"),
            ("graph_completion_hook_runs", "executed_platform"),
            ("graph_completion_hook_runs", "executed_model"),
            ("runs", "executed_platform"),
            ("runs", "executed_model"),
        ] {
            let has_column: bool = conn
                .query_row(
                    &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
                    rusqlite::params![column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                let sql = format!("ALTER TABLE {table} ADD COLUMN {column} TEXT");
                conn.execute(&sql, [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        // Keep the 30-day recent-usage scan bounded per table.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_graph_runs_executed_started
                ON graph_runs(executed_platform, started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_hook_runs_executed_started
                ON graph_completion_hook_runs(executed_platform, started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_runs_executed_started
                ON runs(executed_platform, started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_subagent_runs_platform_started
                ON subagent_runs(platform, started_at DESC);",
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }

    /// Add ensemble and ensemble-member infra-retry columns.
    pub(in crate::db) fn migrate_ensemble_infra_columns(conn: &Connection) -> Result<()> {
        let has_ensemble_commit_rights: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'commit_rights'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_ensemble_commit_rights {
            conn.execute(
                "ALTER TABLE ensembles ADD COLUMN commit_rights INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // CM30: platform-level infra-retry override lives in config.toml's
        // CliConfig; these columns are the ensemble-level default and the
        // per-member override in the same precedence chain (member ->
        // ensemble -> node config -> platform -> engine default). NULL for
        // every pre-existing row, which is exactly "defer further down the
        // chain" — no behavioural migration needed alongside the column add.
        for column in [
            "infra_retry_limit",
            "infra_crash_max_seconds",
            "infra_backoff_seconds",
        ] {
            let has_column: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = '{column}'"
                    ),
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                conn.execute(
                    &format!("ALTER TABLE ensembles ADD COLUMN {column} INTEGER"),
                    [],
                )
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }

        for column in [
            "infra_retry_limit",
            "infra_crash_max_seconds",
            "infra_backoff_seconds",
        ] {
            let has_column: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('ensemble_members') WHERE name = '{column}'"
                    ),
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                conn.execute(
                    &format!("ALTER TABLE ensemble_members ADD COLUMN {column} INTEGER"),
                    [],
                )
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// CM48: create the `roster_entries` availability roster. Guarded
    /// `CREATE TABLE IF NOT EXISTS` — a no-op on a fresh database (the table
    /// is already in `BASE_SCHEMA_BATCHES`) and on an already-migrated one,
    /// and the thing that adds the table to a database created before it
    /// existed.
    pub(in crate::db) fn migrate_roster_entries_table(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS roster_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                platform TEXT NOT NULL,
                model_pattern TEXT,
                state TEXT NOT NULL CHECK(state IN ('available','unavailable')),
                reason TEXT,
                until TEXT,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                UNIQUE(platform, model_pattern)
            );
            CREATE INDEX IF NOT EXISTS idx_roster_entries_platform
                ON roster_entries(platform);",
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }

    /// CM49: create the `crews`/`crew_members` tables and the two binding
    /// columns on `ensembles`. Guarded `CREATE TABLE IF NOT EXISTS` + a
    /// pragma-guarded `ALTER TABLE` — a no-op on a fresh database (already in
    /// `BASE_SCHEMA_BATCHES`) and on an already-migrated one, and the thing
    /// that adds them to a database created before they existed.
    pub(in crate::db) fn migrate_crews_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS crews (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );
            CREATE TABLE IF NOT EXISTS crew_members (
                crew_id TEXT NOT NULL REFERENCES crews(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                platform TEXT NOT NULL,
                model TEXT,
                timeout_minutes INTEGER,
                infra_retry_limit INTEGER,
                infra_crash_max_seconds INTEGER,
                infra_backoff_seconds INTEGER,
                PRIMARY KEY (crew_id, position)
            );",
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        for (table, column, ddl_type) in [
            ("ensembles", "crew_id", "TEXT REFERENCES crews(id)"),
            ("ensembles", "crew_name", "TEXT"),
            ("ensembles", "crew_version_applied", "INTEGER"),
        ] {
            let has_column: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('{table}') \
                         WHERE name = '{column}'"
                    ),
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                conn.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"),
                    [],
                )
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }
}
