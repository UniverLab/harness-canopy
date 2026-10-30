use anyhow::Result;
use rusqlite::Connection;

use crate::db::Database;

// `late` is the child module of `migration.rs` that holds the middle run
// of the post-base-schema migrations so every file stays under the
// 800-line gate; `init` call order in `db/mod.rs` is the migration order
// and is unchanged.

impl Database {
    /// CB42: add `sandbox_runs.cleanup_error` if missing.
    pub(in crate::db) fn migrate_sandbox_cleanup_error_column(conn: &Connection) -> Result<()> {
        // CB42: record cleanup failures on the sandbox run (req 6). A fresh DB
        // gets the column above; an existing one is migrated idempotently here
        // using the same pragma-guard pattern as every other ALTER in this file.
        let has_cleanup_error: bool = conn.query_row(
            "SELECT COUNT(*) FROM pragma_table_info('sandbox_runs') WHERE name = 'cleanup_error'",
            [], |row| Ok(row.get::<_, i32>(0)? > 0)).unwrap_or(false);
        if !has_cleanup_error {
            conn.execute("ALTER TABLE sandbox_runs ADD COLUMN cleanup_error TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// CM8: move `run:`/`sync:`/`launchpad:` rows out of `intelligence_nodes`.
    pub(in crate::db) fn migrate_operational_sessions_from_intelligence(
        conn: &Connection,
    ) -> Result<()> {
        // CM8: separate operational telemetry from knowledge. The three
        // writers (run, sync, launchpad) used to write `kind='session'`
        // rows into `intelligence_nodes` with ids `run:`, `sync:`,
        // `launchpad:` — they now write to `operational_sessions` instead.
        // Migrate any remaining rows so the TUI panels keep showing the same
        // thing. Idempotent: second run finds nothing in `intelligence_nodes`
        // to copy.
        // Reversible as long as `operational_sessions` isn't dropped:
        //   INSERT OR REPLACE INTO intelligence_nodes
        //     (id, kind, status, title, body, metadata, project_hash, session_id, created_at, updated_at)
        //   SELECT id, 'session', 'noted', title, body, metadata, project_hash, session_id, created_at, updated_at
        //   FROM operational_sessions;
        conn.execute_batch(
            "INSERT OR REPLACE INTO operational_sessions (id, title, body, metadata, project_hash, session_id, created_at, updated_at)
                SELECT id, title, body, metadata, project_hash, session_id, created_at, updated_at
                FROM intelligence_nodes
                WHERE id LIKE 'run:%' OR id LIKE 'sync:%' OR id LIKE 'launchpad:%';
             DELETE FROM intelligence_nodes
                WHERE id LIKE 'run:%' OR id LIKE 'sync:%' OR id LIKE 'launchpad:%';",
        )
        .map_err(|e| anyhow::anyhow!("operational_sessions migration failed: {e}"))?;
        Ok(())
    }

    /// CM10: add `intelligence_nodes.status` if missing.
    pub(in crate::db) fn migrate_intelligence_nodes_status_column(conn: &Connection) -> Result<()> {
        // CM10: knowledge-node status. All pre-existing nodes backfill to
        // 'noted' via the column default. Idempotent pragma guard, same
        // pattern as the scheduled_sends.workdir migration below.
        let has_intelligence_status: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('intelligence_nodes') WHERE name = 'status'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_intelligence_status {
            conn.execute(
                "ALTER TABLE intelligence_nodes ADD COLUMN status TEXT NOT NULL DEFAULT 'noted'",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add `scheduled_sends.workdir`/`builder_state` and `provenance` on both send tables.
    pub(in crate::db) fn migrate_scheduled_send_columns(conn: &Connection) -> Result<()> {
        // `workdir` records which project a scheduled send targeted so a
        // dead-target failure can be preserved per-project for U8's recall.
        let has_scheduled_send_workdir: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('scheduled_sends') WHERE name = 'workdir'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_scheduled_send_workdir {
            conn.execute("ALTER TABLE scheduled_sends ADD COLUMN workdir TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        let has_scheduled_builder_state: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('scheduled_sends') WHERE name = 'builder_state'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_scheduled_builder_state {
            conn.execute(
                "ALTER TABLE scheduled_sends ADD COLUMN builder_state TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // `provenance` carries hook origin (`kind`/`graph_id`/`event`) as
        // structured JSON so the TUI can name the sender without that
        // metadata being buried in the prompt text. Nullable so legacy
        // prompt-builder sends keep reading as `None`.
        for table in ["scheduled_sends", "failed_scheduled_sends"] {
            let has_column: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = 'provenance'"
                    ),
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                conn.execute(
                    &format!("ALTER TABLE {table} ADD COLUMN provenance TEXT"),
                    [],
                )
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// Add interactive-session `session_type`/`pid`/`boot_id`, then reclassify bridge rows.
    pub(in crate::db) fn migrate_interactive_session_columns(conn: &Connection) -> Result<()> {
        let has_session_type: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('interactive_sessions') WHERE name = 'session_type'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_session_type {
            conn.execute(
                "ALTER TABLE interactive_sessions ADD COLUMN session_type TEXT NOT NULL DEFAULT 'interactive'",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        let has_pid: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('interactive_sessions') WHERE name = 'pid'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_pid {
            conn.execute(
                "ALTER TABLE interactive_sessions ADD COLUMN pid INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // Boot-id aware auto-resume (see should_resume_session in
        // tui::app::mod): a stored pid alone can't tell a resumed session
        // from an unrelated process that got the same pid after a reboot
        // recycled the pid space. NULL for rows written before this column
        // existed — treated as "unknown boot" (always safe to resume).
        let has_boot_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('interactive_sessions') WHERE name = 'boot_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_boot_id {
            conn.execute(
                "ALTER TABLE interactive_sessions ADD COLUMN boot_id TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // Older builds registered `canopy bridge` sidecars with
        // session_type = 'interactive' (a bug — see daemon::bridge), which made
        // auto_resume_sessions try to relaunch them as chat CLIs on every TUI
        // startup. Reclassify any such rows so they're excluded from
        // get_active_sessions() going forward. Idempotent: once reclassified,
        // the WHERE clause no longer matches them.
        conn.execute(
            "UPDATE interactive_sessions SET session_type = 'bridge'
             WHERE cli = 'bridge' AND session_type != 'bridge'",
            [],
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }

    /// Add agent `enable_at`/`notify_on_success`/`effort` columns.
    pub(in crate::db) fn migrate_agent_optin_columns(conn: &Connection) -> Result<()> {
        let has_enable_at: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'enable_at'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_enable_at {
            conn.execute("ALTER TABLE agents ADD COLUMN enable_at TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // Per-agent opt-in to success notifications (B27). Failures always
        // notify; scheduled/watch successes stay silent unless this is set,
        // so a frequent cron agent can't spam the Action Center. Older
        // databases predate the column; default 0 keeps every existing agent
        // on the quiet-on-success policy.
        let has_notify_on_success: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'notify_on_success'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_notify_on_success {
            conn.execute(
                "ALTER TABLE agents ADD COLUMN notify_on_success BOOLEAN NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        let has_effort: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'effort'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_effort {
            conn.execute("ALTER TABLE agents ADD COLUMN effort TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add graph `trigger_type`/`trigger_config` columns.
    pub(in crate::db) fn migrate_graph_trigger_columns(conn: &Connection) -> Result<()> {
        // Graphs gained optional cron/watch triggers; older databases predate the
        // columns. Add them if missing (both nullable, so existing rows stay
        // manual-only).
        for column in ["trigger_type", "trigger_config"] {
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                conn.execute(&format!("ALTER TABLE graphs ADD COLUMN {column} TEXT"), [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// Add graph `autorun_at`/`auto_continue_*`/`spec_queue` columns.
    pub(in crate::db) fn migrate_graph_schedule_columns(conn: &Connection) -> Result<()> {
        // One-shot resume schedule for graphs (mirrors agents' `enable_at`);
        // older databases predate the column.
        let has_autorun_at: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'autorun_at'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_autorun_at {
            conn.execute("ALTER TABLE graphs ADD COLUMN autorun_at INTEGER", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // One-shot deferred-resume-while-paused schedule (distinct from
        // `autorun_at`'s reset-and-relaunch — see
        // [`crate::domain::graphs::Graph::auto_continue_at`]); older databases
        // predate the columns.
        for (column, sql_type) in [
            ("auto_continue_at", "INTEGER"),
            ("auto_continue_action", "TEXT"),
        ] {
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                conn.execute(
                    &format!("ALTER TABLE graphs ADD COLUMN {column} {sql_type}"),
                    [],
                )
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }

        // `spec_queue` (7f2efdf) was an unused template model, retired in favor
        // of the `queues`/`queue_members` tables below. Kept only so pre-R4
        // databases that already have the column don't need a destructive
        // migration; current code never reads or writes it.
        let has_spec_queue: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'spec_queue'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_queue {
            conn.execute("ALTER TABLE graphs ADD COLUMN spec_queue TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add graph-spec `spec_start_head`/`spec_start_dirty` columns.
    pub(in crate::db) fn migrate_spec_start_columns(conn: &Connection) -> Result<()> {
        // `spec_start_head` records the workdir's git HEAD when a spec starts
        // running, so `check` nodes can verify a spec actually committed
        // without relying on a file marker outside the spec row. Older
        // databases predate the column.
        let has_spec_start_head: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'spec_start_head'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_start_head {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN spec_start_head TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        let has_spec_start_dirty: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'spec_start_dirty'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_start_dirty {
            conn.execute(
                "ALTER TABLE graph_specs ADD COLUMN spec_start_dirty INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// R3: rebuild `graph_specs` so `graph_id` may be NULL.
    pub(in crate::db) fn migrate_graph_specs_nullable_graph_id(conn: &Connection) -> Result<()> {
        // Standalone specs (R3): a spec no longer must belong to a graph — it
        // can exist as a backlog item (optionally tagged to a workdir)
        // before being assigned. Older databases have `graph_id NOT NULL`,
        // which `ALTER TABLE ... ADD COLUMN` cannot relax, so rebuild the
        // table via SQLite's documented copy-and-rename procedure. Every
        // existing row keeps its `graph_id`; only new rows may leave it NULL.
        let graph_specs_graph_id_nullable: bool = conn
            .query_row(
                "SELECT \"notnull\" FROM pragma_table_info('graph_specs') WHERE name = 'graph_id'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|notnull| notnull == 0)
            .unwrap_or(false);
        if !graph_specs_graph_id_nullable {
            conn.execute_batch(
                "PRAGMA foreign_keys=OFF;
                 BEGIN TRANSACTION;

                 CREATE TABLE graph_specs_new (
                     id TEXT PRIMARY KEY,
                     graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                     name TEXT NOT NULL,
                     description TEXT,
                     position INTEGER NOT NULL,
                     parallelizable INTEGER NOT NULL DEFAULT 0,
                     status TEXT NOT NULL,
                     started_at INTEGER,
                     completed_at INTEGER,
                     spec_start_head TEXT,
                     spec_start_dirty INTEGER
                 );
                 INSERT INTO graph_specs_new (id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty)
                     SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty FROM graph_specs;
                 DROP TABLE graph_specs;
                 ALTER TABLE graph_specs_new RENAME TO graph_specs;

                 CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_specs_position
                     ON graph_specs(graph_id, position);

                 COMMIT;
                 PRAGMA foreign_keys=ON;",
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// R3: add the optional graph-spec `workdir` tag.
    pub(in crate::db) fn migrate_spec_workdir_column(conn: &Connection) -> Result<()> {
        // Optional workdir tag on specs (R3), for backlog filtering only —
        // it never drives execution. Nullable, so existing (graph-bound)
        // specs are unaffected; older databases predate the column.
        let has_spec_workdir: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'workdir'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_spec_workdir {
            conn.execute("ALTER TABLE graph_specs ADD COLUMN workdir TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// R1: rebuild `graph_nodes`/`graph_edges` to allow graph-level targeting.
    pub(in crate::db) fn migrate_graph_targeting_rebuild(conn: &Connection) -> Result<()> {
        // Graph-level graphs (R1): a node/edge may now target a graph directly
        // (`graph_id`) instead of a spec, so it can be defined once per graph
        // instead of being repeated across every spec. Older databases have
        // `spec_id NOT NULL` on both tables, which `ALTER TABLE ... ADD
        // COLUMN` cannot relax, so rebuild the tables via SQLite's documented
        // copy-and-rename procedure ("Making Other Kinds Of Table Schema
        // Changes"). Every existing row keeps its `spec_id`; `graph_id` starts
        // NULL for all of them, so nothing already saved changes meaning.
        let has_graph_nodes_graph_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_nodes') WHERE name = 'graph_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_graph_nodes_graph_id {
            conn.execute_batch(
                "PRAGMA foreign_keys=OFF;
                 BEGIN TRANSACTION;

                 CREATE TABLE graph_nodes_new (
                     id TEXT PRIMARY KEY,
                     spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                     graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                     name TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     config TEXT NOT NULL,
                     position INTEGER NOT NULL,
                     created_at INTEGER NOT NULL,
                     CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
                 );
                 INSERT INTO graph_nodes_new (id, spec_id, graph_id, name, kind, config, position, created_at)
                     SELECT id, spec_id, NULL, name, kind, config, position, created_at FROM graph_nodes;
                 DROP TABLE graph_nodes;
                 ALTER TABLE graph_nodes_new RENAME TO graph_nodes;

                 CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_nodes_position
                     ON graph_nodes(spec_id, position);
                 CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_nodes_graph_position
                     ON graph_nodes(graph_id, position);

                 CREATE TABLE graph_edges_new (
                     id TEXT PRIMARY KEY,
                     spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                     graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                     from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                     to_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                     condition TEXT NOT NULL,
                     CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
                 );
                 INSERT INTO graph_edges_new (id, spec_id, graph_id, from_node, to_node, condition)
                     SELECT id, spec_id, NULL, from_node, to_node, condition FROM graph_edges;
                 DROP TABLE graph_edges;
                 ALTER TABLE graph_edges_new RENAME TO graph_edges;

                 CREATE INDEX IF NOT EXISTS idx_graph_edges_spec_from
                     ON graph_edges(spec_id, from_node);
                 CREATE INDEX IF NOT EXISTS idx_graph_edges_graph_from
                     ON graph_edges(graph_id, from_node);

                 COMMIT;
                 PRAGMA foreign_keys=ON;",
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// R1: create the graph-targeting indexes once the column exists.
    pub(in crate::db) fn migrate_graph_targeting_indexes(conn: &Connection) -> Result<()> {
        // These reference `graph_id`, so they can only be created once the
        // column is guaranteed to exist — either from the fresh CREATE TABLE
        // above (new databases) or the rebuild just above (migrated ones).
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_nodes_graph_position
                 ON graph_nodes(graph_id, position);
             CREATE INDEX IF NOT EXISTS idx_graph_edges_graph_from
                 ON graph_edges(graph_id, from_node);",
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }

    /// Add graph `active_run_queue_id`/`on_completed` columns.
    pub(in crate::db) fn migrate_graphs_registry_columns(conn: &Connection) -> Result<()> {
        // `active_run_queue_id` persists which queue (if any) a graph's current/
        // last run drew from, so an interrupted run (quota failure, daemon
        // crash) can be resumed against the same queue by every resume path
        // (scheduled autorun, `graph_control`) instead of falling back to the
        // graph's own bound specs. Older databases predate the column.
        let has_active_run_queue_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'active_run_queue_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_active_run_queue_id {
            conn.execute("ALTER TABLE graphs ADD COLUMN active_run_queue_id TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // `on_completed` (N2): the graph's optional post-completion hook
        // config (agent-node-style JSON: platform/model/prompt/
        // timeout_minutes), fired once when a run reaches `Completed`. `NULL`
        // on older databases and on any graph that never configured one —
        // exactly today's (pre-N2) behavior.
        let has_on_completed: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'on_completed'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_on_completed {
            conn.execute("ALTER TABLE graphs ADD COLUMN on_completed TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add graph-run `pid`/`boot_id`/`session_id` columns.
    pub(in crate::db) fn migrate_graph_runs_identity_columns(conn: &Connection) -> Result<()> {
        // `pid`/`boot_id` (B12): the OS process-group leader spawned for a
        // node run, and the boot it was spawned under. Lets the engine
        // `killpg` an active run's process on every abnormal end (timeout,
        // pause, reset, budget exhaustion, run failure, daemon shutdown),
        // and lets startup reconciliation attempt a best-effort kill of
        // survivors from the same boot. Older databases predate both
        // columns; NULL on existing rows (nothing was tracked for them, so
        // there's nothing to kill).
        for column in ["pid", "boot_id"] {
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graph_runs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                let sql = if column == "pid" {
                    "ALTER TABLE graph_runs ADD COLUMN pid INTEGER".to_string()
                } else {
                    "ALTER TABLE graph_runs ADD COLUMN boot_id TEXT".to_string()
                };
                conn.execute(&sql, [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }

        // `session_id` (RS1): the harness session id that served a node run,
        // captured per-platform (set-at-spawn for platforms that accept a
        // caller-chosen id, list-after-run for those that can enumerate their
        // sessions). Older databases predate the column; NULL on existing rows
        // (no session identity was ever captured for them). Additive — the
        // foundation for resume mode (RS2).
        let has_session_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_runs') WHERE name = 'session_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_session_id {
            conn.execute("ALTER TABLE graph_runs ADD COLUMN session_id TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// B25: add graph-spec `completed_via*` columns.
    pub(in crate::db) fn migrate_spec_completed_via_columns(conn: &Connection) -> Result<()> {
        // `completed_via`/`completed_via_reason`/`completed_via_at` (B25):
        // administrative spec status transitions, recorded with provenance
        // and reason. Older databases predate these columns; NULL on existing
        // rows (all existing completions are engine-driven).
        for column in ["completed_via", "completed_via_reason", "completed_via_at"] {
            let has_column: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_column {
                let sql = match column {
                    "completed_via" => "ALTER TABLE graph_specs ADD COLUMN completed_via TEXT",
                    "completed_via_reason" => {
                        "ALTER TABLE graph_specs ADD COLUMN completed_via_reason TEXT"
                    }
                    "completed_via_at" => {
                        "ALTER TABLE graph_specs ADD COLUMN completed_via_at INTEGER"
                    }
                    _ => "",
                };
                if !sql.is_empty() {
                    conn.execute(sql, [])
                        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
                }
            }
        }
        Ok(())
    }

    /// RS3: add `queue_members.group_name`.
    pub(in crate::db) fn migrate_queue_member_group_name_column(conn: &Connection) -> Result<()> {
        // `group_name` (RS3): the optional context group a queue member belongs
        // to. Grouped members share a warm harness session — the first agent
        // node of a grouped spec resumes the session captured by the previous
        // successfully-completed grouped sibling instead of cold-starting.
        // Older databases predate the column; NULL on existing rows (every
        // legacy member is ungrouped and never cross-resumes). Additive.
        let has_group_name: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('queue_members') WHERE name = 'group_name'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_group_name {
            conn.execute("ALTER TABLE queue_members ADD COLUMN group_name TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add `graph_edges.route` for router nodes.
    pub(in crate::db) fn migrate_edge_route_column(conn: &Connection) -> Result<()> {
        // `route` (router nodes): the label a `route`-conditioned edge names,
        // alongside `condition = 'route'` — see
        // `domain::graphs::GraphEdgeCondition::{as_str, route_label, from_parts}`.
        // NULL for every pre-existing edge (none of them can be a router's
        // route edge, since `GraphNodeKind::Router` didn't exist yet either).
        let has_edge_route: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_edges') WHERE name = 'route'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_edge_route {
            conn.execute("ALTER TABLE graph_edges ADD COLUMN route TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Add ensemble-member `prompt_override`/`timeout_minutes`.
    pub(in crate::db) fn migrate_ensemble_member_columns(conn: &Connection) -> Result<()> {
        // Per-member prompt override: a member can now render its own prompt
        // instead of the ensemble's shared `prompt_template` — see
        // `EnsembleMember::prompt_override`. NULL for every pre-existing
        // member, which is exactly "use the shared template", so no
        // behavioural migration is needed alongside the column add.
        let has_member_prompt_override: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensemble_members') WHERE name = 'prompt_override'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_member_prompt_override {
            conn.execute(
                "ALTER TABLE ensemble_members ADD COLUMN prompt_override TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        // Per-member timeout override (CM23): a member can now carry its own
        // agent timeout, overriding the ensemble's shared `timeout_minutes` for
        // that member only — see `EnsembleMember::timeout_minutes`. NULL for
        // every pre-existing member, which is exactly "use the ensemble's
        // timeout_minutes", so no behavioural migration is needed alongside the
        // column add.
        let has_member_timeout_minutes: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensemble_members') WHERE name = 'timeout_minutes'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_member_timeout_minutes {
            conn.execute(
                "ALTER TABLE ensemble_members ADD COLUMN timeout_minutes INTEGER",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// CM53: add `action`/`target_session_id`/`target_session_name` columns to
    /// `agents` for `send_to_session` delivery. `action` defaults to
    /// `'headless'` so pre-existing rows keep their existing (CLI-spawn)
    /// behaviour; the target columns default to NULL.
    pub(in crate::db) fn migrate_agent_action_columns(conn: &Connection) -> Result<()> {
        let has_action: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'action'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_action {
            conn.execute(
                "ALTER TABLE agents ADD COLUMN action TEXT NOT NULL DEFAULT 'headless'",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }

        for col in ["target_session_id", "target_session_name"] {
            let has_col: bool = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = '{col}'"
                    ),
                    [],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has_col {
                conn.execute(&format!("ALTER TABLE agents ADD COLUMN {col} TEXT"), [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// Add graph `active_run_workdir`: the workdir a run was launched with
    /// (the override, or the stored workdir when none was given), persisted
    /// the moment that run starts so every continuation of the run
    /// (`graph_control` in every mode, scheduled resumes, quota auto-resume)
    /// re-dispatches in it instead of silently expiring back to the stored
    /// workdir. `NULL` on pre-existing rows (legacy runs fall back to the
    /// stored workdir) and on graphs that never ran. Lives here rather than
    /// in `schema.rs` so that file stays under the 800-line gate.
    pub(in crate::db) fn migrate_graph_active_run_workdir(conn: &Connection) -> Result<()> {
        let has_active_run_workdir: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'active_run_workdir'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_active_run_workdir {
            conn.execute("ALTER TABLE graphs ADD COLUMN active_run_workdir TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}
