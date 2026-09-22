use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Bumped whenever a migration changes table/column names in a way that an
/// older binary's `CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS`
/// guards can't detect (they'd silently recreate the old names as empty
/// rather than erroring) — see `check_schema_version`. Version 4 adds
/// the `activity_log` table (CM20 bitácora).
/// Version 5 renames the loop→graph schema (tables, `loop_id` columns, and
/// `break`→`error` edge conditions) — see `migrate_loop_to_graph_schema` (CC3).
/// Version 6 changes the `ensembles` entry/exit FKs from `ON DELETE CASCADE`
/// / `ON DELETE SET NULL` to `ON DELETE RESTRICT` — see
/// `migrate_cb52_ensemble_fk` (CB52).
const SCHEMA_VERSION: i64 = 6;

/// Thread-safe `SQLite` database wrapper.
///
/// Uses an `Arc<Mutex<Connection>>` so the handle can be cheaply cloned and
/// shared across threads (e.g. for background file-scanning tasks) while still
/// serialising all SQLite writes through a single connection.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn new(db_path: &PathBuf) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let db = Database {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init()?;
        if let Err(e) = db.backfill_project_nodes() {
            tracing::warn!("Could not backfill project intelligence nodes: {e}");
        }
        // CM9 typed project graph: retype backlog notes, derive containment,
        // seed known cross-project dependencies. All best-effort: open must
        // never fail because a seed path isn't registered.
        if let Err(e) = db.retype_backlog_project_nodes() {
            tracing::warn!("Could not retype backlog project nodes: {e}");
        }
        match db.rebuild_containment_edges() {
            Ok(count) => tracing::debug!("Rebuilt {count} derived containment edge(s)"),
            Err(e) => tracing::warn!("Could not rebuild containment edges: {e}"),
        }
        if let Err(e) = db.seed_cm9_project_dependencies() {
            tracing::warn!("Could not seed CM9 project dependencies: {e}");
        }
        if let Err(e) = db.seed_builtin_blueprints() {
            tracing::warn!("Could not seed builtin blueprints: {e}");
        }
        if let Err(e) = db.seed_builtin_ensemble_blueprints() {
            tracing::warn!("Could not seed builtin ensemble blueprints: {e}");
        }
        Ok(db)
    }

    /// Open the database, running migrations only if no foreign daemon is live.
    /// If a daemon is running, opens the file without running migrations or
    /// seeding (reads and ordinary writes against the existing schema still
    /// work) so a newer binary's migrations don't rename columns under a live
    /// older daemon. Every non-daemon path that opens the real `~/.canopy`
    /// database must use this instead of `new` — `run_http_server` is the one
    /// exception, because it holds the daemon lock and therefore *is* the
    /// daemon.
    pub fn new_safe(db_path: &PathBuf, data_dir: &std::path::Path) -> Result<Self> {
        if crate::daemon::process::other_instance_may_be_running(data_dir) {
            let conn = Connection::open(db_path)?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            return Ok(Database {
                conn: Arc::new(Mutex::new(conn)),
            });
        }
        Database::new(db_path)
    }

    /// Refuses to open a database stamped with a schema version newer than
    /// this binary knows about. Without this, an older binary meeting a
    /// renamed/restructured schema would pass every `IF NOT EXISTS` guard
    /// (the old names it looks for are simply gone) and start up believing
    /// every table is empty — silent data loss from its perspective. A
    /// missing `schema_version` row (pre-dates this check, or a brand new
    /// database) is not a mismatch; every migration below is still
    /// self-guarding for that case.
    fn check_schema_version(conn: &Connection) -> Result<()> {
        let daemon_state_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'daemon_state'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !daemon_state_exists {
            return Ok(());
        }

        let stored: Option<i64> = conn
            .query_row(
                "SELECT value FROM daemon_state WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|v| v.parse::<i64>().ok());

        if let Some(stored) = stored {
            if stored > SCHEMA_VERSION {
                anyhow::bail!(
                    "Database schema version {stored} is newer than this build of canopy (v{}) supports (schema version {SCHEMA_VERSION}). Upgrade canopy before opening this database.",
                    env!("CARGO_PKG_VERSION")
                );
            }
        }
        Ok(())
    }

    fn set_schema_version(conn: &Connection) -> Result<()> {
        conn.execute(
            "INSERT INTO daemon_state (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Renames the previous-generation queue tables and columns (retired
    /// names recorded only as literal SQL below, since renaming requires
    /// naming what's being renamed) to their current equivalents. Guarded on
    /// the old primary table's existence, so it's a no-op on both a fresh
    /// database (never had it) and an already-migrated one (already renamed
    /// away) — safe to run on every startup. Must run before the `CREATE
    /// TABLE IF NOT EXISTS` batch below, which would otherwise create empty
    /// `queues`/`queue_members` tables first and make the rename fail with
    /// "table already exists".
    // RETIRED-SCHEMA-NAME-BEGIN (see `no_retired_schema_name_identifiers_remain_outside_its_migration`)
    fn migrate_legacy_queue_schema(conn: &Connection) -> Result<()> {
        let legacy_schema_present: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'pools'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !legacy_schema_present {
            return Ok(());
        }

        let legacy_active_run_alias_present: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'active_run_pool_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        let legacy_template_column_present: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'spec_pool'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);

        let mut migration_sql = String::from(
            "PRAGMA legacy_alter_table = OFF;
             BEGIN TRANSACTION;
             ALTER TABLE pools RENAME TO queues;
             ALTER TABLE pool_members RENAME TO queue_members;
             ALTER TABLE queue_members RENAME COLUMN pool_id TO queue_id;
             DROP INDEX IF EXISTS idx_pool_members_position;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_members_position
                 ON queue_members(queue_id, position);
            ",
        );
        if legacy_active_run_alias_present {
            migration_sql.push_str(
                "ALTER TABLE graphs RENAME COLUMN active_run_pool_id TO active_run_queue_id;\n",
            );
        }
        if legacy_template_column_present {
            migration_sql.push_str("ALTER TABLE graphs RENAME COLUMN spec_pool TO spec_queue;\n");
        }
        migration_sql.push_str("COMMIT;");

        conn.execute_batch(&migration_sql)
            .map_err(|e| anyhow::anyhow!("legacy queue schema migration failed: {e}"))?;

        let fk_violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|e| anyhow::anyhow!("legacy queue schema migration failed: {e}"))?;
        if fk_violations > 0 {
            anyhow::bail!(
                "legacy queue schema migration failed: {fk_violations} foreign key violation(s) detected after migration"
            );
        }

        Ok(())
    }
    // RETIRED-SCHEMA-NAME-END

    // RETIRED-SCHEMA-NAME-BEGIN (CC3 loop → graph migration)
    fn migrate_loop_to_graph_schema(conn: &Connection) -> Result<()> {
        let has_loops: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'loops'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_loops {
            return Ok(());
        }
        let has_graphs: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'graphs'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if has_graphs {
            return Ok(());
        }

        conn.execute_batch(
            "PRAGMA legacy_alter_table = OFF;
             BEGIN TRANSACTION;
             ALTER TABLE loops RENAME TO graphs;
             ALTER TABLE loop_specs RENAME TO graph_specs;
             ALTER TABLE loop_nodes RENAME TO graph_nodes;
             ALTER TABLE loop_edges RENAME TO graph_edges;
             ALTER TABLE loop_runs RENAME TO graph_runs;
             ALTER TABLE loop_completion_hook_runs RENAME TO graph_completion_hook_runs;
             ",
        )
        .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;

        // Column renames — each guarded by `pragma_table_info` (older
        // databases may lack `loop_id` on tables that gained top-level
        // targeting later, and a re-run must never fail on an already
        // renamed column). The `graphs.active_run_pool_id` / `spec_pool`
        // guards cover the ordering edge: `migrate_legacy_queue_schema`
        // runs first but looks for `graphs`, so a database that still has
        // both `pools` and `graphs` reaches this function with its
        // queue-era columns untouched — renamed here instead of orphaned.
        for (table, old, new_) in [
            ("graph_specs", "loop_id", "graph_id"),
            ("graph_nodes", "loop_id", "graph_id"),
            ("graph_edges", "loop_id", "graph_id"),
            ("graph_runs", "loop_id", "graph_id"),
            ("graph_completion_hook_runs", "loop_id", "graph_id"),
            ("ensembles", "loop_id", "graph_id"),
            ("graphs", "active_run_pool_id", "active_run_queue_id"),
            ("graphs", "spec_pool", "spec_queue"),
        ] {
            let has: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
                    [table, old],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if has {
                conn.execute(
                    &format!("ALTER TABLE {table} RENAME COLUMN {old} TO {new_}"),
                    [],
                )
                .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;
            }
        }

        conn.execute(
            "UPDATE graph_edges SET condition = 'error' WHERE condition = 'break'",
            [],
        )
        .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;
        // `ensembles.entry_condition` only exists on databases new enough
        // for ensembles — guard so very old databases don't fail here.
        let has_entry_condition: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'entry_condition'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if has_entry_condition {
            conn.execute(
                "UPDATE ensembles SET entry_condition = 'error' WHERE entry_condition = 'break'",
                [],
            )
            .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;
        }
        conn.execute(
            "UPDATE graph_nodes SET config = REPLACE(config, '{{loop_name}}', '{{graph_name}}')
                WHERE config LIKE '%{{loop_name}}%'",
            [],
        )
        .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;
        conn.execute_batch(
            "DROP INDEX IF EXISTS idx_loop_specs_position;
             DROP INDEX IF EXISTS idx_loop_nodes_position;
             DROP INDEX IF EXISTS idx_loop_edges_spec_from;
             DROP INDEX IF EXISTS idx_loop_runs_spec_started;
             DROP INDEX IF EXISTS idx_loop_runs_node_iteration;
             DROP INDEX IF EXISTS idx_loop_runs_loop_started;
             DROP INDEX IF EXISTS idx_loop_completion_hook_runs_loop_started;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_specs_position
                 ON graph_specs(graph_id, position);
             CREATE INDEX IF NOT EXISTS idx_graph_edges_spec_from
                 ON graph_edges(spec_id, from_node);
             CREATE INDEX IF NOT EXISTS idx_graph_runs_spec_started
                 ON graph_runs(spec_id, started_at ASC);
             CREATE INDEX IF NOT EXISTS idx_graph_runs_node_iteration
                 ON graph_runs(node_id, iteration DESC);
             CREATE INDEX IF NOT EXISTS idx_graph_runs_graph_started
                 ON graph_runs(graph_id, started_at DESC);
             CREATE INDEX IF NOT EXISTS idx_graph_completion_hook_runs_graph_started
                 ON graph_completion_hook_runs(graph_id, started_at ASC);
             COMMIT;",
        )
        .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;

        let fk_violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;
        if fk_violations > 0 {
            anyhow::bail!("graph to graph schema migration failed: {fk_violations} foreign key violation(s) detected after migration");
        }
        Ok(())
    }
    // RETIRED-SCHEMA-NAME-END

    /// CB52: the `ensembles` entry/exit FKs (`entry_from_node`, `on_pass_to`,
    /// `on_fail_to`) used to be `ON DELETE CASCADE` / `ON DELETE SET NULL`,
    /// so deleting a plain node silently deleted every ensemble referencing
    /// it and orphaned its members/join. Fresh databases get `ON DELETE
    /// RESTRICT` from the `CREATE TABLE` batch below; this rebuilds the table
    /// on existing databases. SQLite cannot `ALTER` FK actions, so the table
    /// is copied and renamed — the documented procedure (foreign keys off
    /// during the rebuild, `foreign_key_check` after). The replacement table
    /// is derived from the stored DDL by swapping exactly the three FK
    /// actions, so later columns (`kind`, `round_robin_index`,
    /// `quorum_grace_minutes`, `commit_rights`, ...) survive untouched.
    /// Never auto-repairs orphans (C1): pre-existing orphan joins are
    /// reported by `canopy doctor` instead.
    fn migrate_cb52_ensemble_fk(conn: &Connection) -> Result<()> {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'ensembles'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(sql) = sql else {
            return Ok(());
        };
        const ENTRY_CASCADE: &str =
            "entry_from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE";
        const PASS_CASCADE: &str =
            "on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE";
        const FAIL_SET_NULL: &str = "on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE SET NULL";
        if !sql.contains(ENTRY_CASCADE)
            && !sql.contains(PASS_CASCADE)
            && !sql.contains(FAIL_SET_NULL)
        {
            return Ok(());
        }
        // The first `ensembles` in the stored DDL is the table name itself.
        let new_sql = sql
            .replacen("ensembles", "ensembles_new", 1)
            .replace(
                ENTRY_CASCADE,
                "entry_from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT",
            )
            .replace(
                PASS_CASCADE,
                "on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT",
            )
            .replace(
                FAIL_SET_NULL,
                "on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE RESTRICT",
            );
        if !new_sql.contains("ensembles_new")
            || !new_sql.contains("ON DELETE RESTRICT")
            || new_sql.contains(ENTRY_CASCADE)
            || new_sql.contains(PASS_CASCADE)
            || new_sql.contains(FAIL_SET_NULL)
        {
            anyhow::bail!("CB52 ensemble FK migration failed: unexpected stored ensembles DDL");
        }
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('ensembles')")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if cols.is_empty() {
            anyhow::bail!("CB52 ensemble FK migration failed: ensembles table has no columns");
        }
        let collist = cols.join(", ");

        conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let rebuild = (|| -> Result<()> {
            conn.execute_batch("BEGIN TRANSACTION;")?;
            let steps = [
                new_sql.clone(),
                format!("INSERT INTO ensembles_new ({collist}) SELECT {collist} FROM ensembles;"),
                "DROP TABLE ensembles;".to_string(),
                "ALTER TABLE ensembles_new RENAME TO ensembles;".to_string(),
                "CREATE INDEX IF NOT EXISTS idx_ensembles_join_node ON ensembles(join_node_id);"
                    .to_string(),
            ];
            for step in &steps {
                if let Err(e) = conn.execute_batch(step) {
                    let _ = conn.execute_batch("ROLLBACK;");
                    anyhow::bail!("CB52 ensemble FK migration failed: {e}");
                }
            }
            if let Err(e) = conn.execute_batch("COMMIT;") {
                let _ = conn.execute_batch("ROLLBACK;");
                anyhow::bail!("CB52 ensemble FK migration failed: {e}");
            }
            Ok(())
        })();
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        rebuild?;

        let fk_violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|e| anyhow::anyhow!("CB52 ensemble FK migration failed: {e}"))?;
        if fk_violations > 0 {
            anyhow::bail!(
                "CB52 ensemble FK migration failed: {fk_violations} foreign key violation(s) detected after migration"
            );
        }
        Ok(())
    }

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
    fn describe_sql_failure(step: &str, e: &rusqlite::Error) -> anyhow::Error {
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

    fn init(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        Self::check_schema_version(&conn)?;
        Self::migrate_legacy_queue_schema(&conn)?;
        Self::migrate_loop_to_graph_schema(&conn)?;
        Self::migrate_cb52_ensemble_fk(&conn)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                cli TEXT NOT NULL,
                model TEXT,
                effort TEXT,
                working_dir TEXT,
                enabled BOOLEAN NOT NULL DEFAULT 1,
                enable_at TEXT,
                created_at TEXT NOT NULL,
                log_path TEXT NOT NULL,
                timeout_minutes INTEGER NOT NULL DEFAULT 15,
                expires_at TEXT,
                last_run_at TEXT,
                last_run_ok BOOLEAN,
                last_triggered_at TEXT,
                trigger_count INTEGER NOT NULL DEFAULT 0,
                notify_on_success BOOLEAN NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                background_agent_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                trigger_type TEXT NOT NULL,
                summary TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                exit_code INTEGER,
                timeout_at TEXT,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE TABLE IF NOT EXISTS daemon_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS interactive_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cli TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                args TEXT,
                started_at TEXT NOT NULL,
                exited_at TEXT,
                exit_code INTEGER,
                status TEXT NOT NULL DEFAULT 'active',
                session_type TEXT NOT NULL DEFAULT 'interactive',
                pid INTEGER,
                boot_id TEXT
            );

            CREATE TABLE IF NOT EXISTS terminal_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                shell TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_active TEXT,
                status TEXT NOT NULL DEFAULT 'idle'
            );

            CREATE INDEX IF NOT EXISTS idx_interactive_sessions_workdir
                ON interactive_sessions(working_dir, started_at DESC);

            CREATE INDEX IF NOT EXISTS idx_terminal_sessions_workdir
                ON terminal_sessions(working_dir, created_at DESC);

            CREATE TABLE IF NOT EXISTS groups (
                id TEXT PRIMARY KEY,
                orientation TEXT NOT NULL DEFAULT 'horizontal',
                session_a TEXT NOT NULL,
                session_b TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workdir TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS activity_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workdir TEXT NOT NULL,
                source TEXT NOT NULL,
                source_id TEXT,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_activity_log_workdir_created
                ON activity_log(workdir, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_activity_log_kind
                ON activity_log(kind);

            CREATE TABLE IF NOT EXISTS sync_locks (
                id TEXT PRIMARY KEY,
                workdir TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                lock_type TEXT NOT NULL,
                resource TEXT NOT NULL,
                acquired_at INTEGER NOT NULL,
                expires_at INTEGER,
                released_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS projects (
                hash TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                tags TEXT,
                indexed_at INTEGER,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rag_queue (
                source_path TEXT NOT NULL PRIMARY KEY,
                status TEXT NOT NULL,
                queued_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rag_file_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL,
                event_type TEXT NOT NULL,
                detail TEXT,
                occurred_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rag_file_events_path
                ON rag_file_events(file_path);

            CREATE TABLE IF NOT EXISTS intelligence_nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'noted',
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                content_touched_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_kind_updated
                ON intelligence_nodes(kind, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_project_hash
                ON intelligence_nodes(project_hash);
            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_session_id
                ON intelligence_nodes(session_id);

            CREATE TABLE IF NOT EXISTS intelligence_edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_node_id TEXT NOT NULL,
                to_node_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                weight REAL NOT NULL DEFAULT 1.0,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(from_node_id) REFERENCES intelligence_nodes(id) ON DELETE CASCADE,
                FOREIGN KEY(to_node_id) REFERENCES intelligence_nodes(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_intelligence_edges_from
                ON intelligence_edges(from_node_id);
            CREATE INDEX IF NOT EXISTS idx_intelligence_edges_to
                ON intelligence_edges(to_node_id);

            CREATE TABLE IF NOT EXISTS operational_sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_operational_sessions_project_hash
                ON operational_sessions(project_hash);
            CREATE INDEX IF NOT EXISTS idx_operational_sessions_updated
                ON operational_sessions(updated_at DESC);

            CREATE TABLE IF NOT EXISTS graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER,
                spec_queue TEXT,
                active_run_queue_id TEXT,
                on_completed TEXT,
                auto_continue_at INTEGER,
                auto_continue_action TEXT,
                hooks TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_graphs_workdir_created
                ON graphs(workdir, created_at DESC);

            CREATE TABLE IF NOT EXISTS graph_specs (
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
                spec_start_dirty INTEGER,
                workdir TEXT,
                completed_via TEXT,
                completed_via_reason TEXT,
                completed_via_at INTEGER,
                spec_committed_head TEXT,
                spec_end_dirty INTEGER,
                spec_end_dirty_paths TEXT,
                cross_run_attempts INTEGER NOT NULL DEFAULT 0,
                -- CT17: monotonic change signal for the panel's backlog event.
                -- Milliseconds since epoch; every write to this row refreshes
                -- it (see `insert_graph_spec` and the `UPDATE graph_specs`
                -- writers in `db/graphs.rs`). Never rendered, so the unit is
                -- free — millis make same-tick edits distinguishable where
                -- seconds would tie. Legacy rows backfilled below hold
                -- seconds-scale values and are always smaller.
                updated_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_specs_position
                ON graph_specs(graph_id, position);

            CREATE TABLE IF NOT EXISTS graph_nodes (
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

            CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_nodes_position
                ON graph_nodes(spec_id, position);

            CREATE TABLE IF NOT EXISTS graph_edges (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                to_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                condition TEXT NOT NULL,
                route TEXT,
                CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
            );

            CREATE INDEX IF NOT EXISTS idx_graph_edges_spec_from
                ON graph_edges(spec_id, from_node);

            CREATE TABLE IF NOT EXISTS graph_runs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                input TEXT,
                output TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                iteration INTEGER NOT NULL DEFAULT 1,
                pid INTEGER,
                boot_id TEXT,
                session_id TEXT,
                paused_through INTEGER NOT NULL DEFAULT 0,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_graph_runs_spec_started
                ON graph_runs(spec_id, started_at ASC);

            CREATE INDEX IF NOT EXISTS idx_graph_runs_node_iteration
                ON graph_runs(node_id, iteration DESC);

            -- Speeds the sidebar's last-activity-per-graph aggregate
            -- (MAX(started_at) GROUP BY graph_id) into a loose index scan
            -- instead of a full table scan.
            CREATE INDEX IF NOT EXISTS idx_graph_runs_graph_started
                ON graph_runs(graph_id, started_at DESC);

            -- N2: firings of a graph's `on_completed` hook. Deliberately not
            -- `graph_runs` — that table's spec_id/node_id are NOT NULL FKs into
            -- a spec's graph, which a completion hook (no spec, no graph node)
            -- can never satisfy.
            CREATE TABLE IF NOT EXISTS graph_completion_hook_runs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                output TEXT,
                summary TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                pid INTEGER,
                boot_id TEXT,
                event TEXT NOT NULL DEFAULT 'on_completed',
                hook_index INTEGER NOT NULL DEFAULT 0,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_graph_completion_hook_runs_graph_started
                ON graph_completion_hook_runs(graph_id, started_at ASC);

            -- F1: an ensemble unit -- the members and join are ordinary
            -- graph_nodes/graph_edges rows (the engine's graph-walking code is
            -- reused as-is); this row is what lets graph_get/graph_update_ensemble
            -- address the whole ensemble as one thing instead of N+1 nodes.
            CREATE TABLE IF NOT EXISTS ensembles (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                prompt_template TEXT NOT NULL,
                join_node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                entry_from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT,
                entry_condition TEXT NOT NULL,
                min_pass INTEGER NOT NULL,
                straggler_timeout_minutes INTEGER,
                quorum_grace_minutes INTEGER,
                timeout_minutes INTEGER NOT NULL,
                on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT,
                on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE RESTRICT,
                commit_rights INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
            );

            CREATE TABLE IF NOT EXISTS ensemble_members (
                ensemble_id TEXT NOT NULL REFERENCES ensembles(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                platform TEXT NOT NULL,
                model TEXT,
                prompt_override TEXT,
                timeout_minutes INTEGER,
                PRIMARY KEY (ensemble_id, node_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_ensemble_members_position
                ON ensemble_members(ensemble_id, position);

            CREATE INDEX IF NOT EXISTS idx_ensemble_members_node
                ON ensemble_members(node_id);

            CREATE INDEX IF NOT EXISTS idx_ensembles_join_node
                ON ensembles(join_node_id);

            -- F1: ensemble blueprints -- a whole ensemble's shared prompt +
            -- member list (unlike `blueprints`, which templates a single
            -- node), seeded with the builtin ensemble-proposers pattern.
            CREATE TABLE IF NOT EXISTS ensemble_blueprints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                prompt_template TEXT NOT NULL,
                members TEXT NOT NULL,
                min_pass INTEGER,
                quorum_grace_minutes INTEGER,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queues (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queue_members (
                queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                group_name TEXT,
                PRIMARY KEY (queue_id, spec_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_members_position
                ON queue_members(queue_id, position);

            CREATE TABLE IF NOT EXISTS seed_sessions (
                session_id TEXT PRIMARY KEY,
                seed_id TEXT NOT NULL,
                bound_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES interactive_sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_seed_sessions_seed
                ON seed_sessions(seed_id);

            CREATE TABLE IF NOT EXISTS blueprints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scheduled_sends (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                target_session_id TEXT NOT NULL,
                fire_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                provenance TEXT
            );

            CREATE TABLE IF NOT EXISTS failed_scheduled_sends (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                target_session_id TEXT NOT NULL,
                workdir TEXT,
                failed_at INTEGER NOT NULL,
                provenance TEXT
            );

            -- U8: the prompt builder's last-sent prompt per project, recalled
            -- with Ctrl+L. Insert-only with a timestamp (rather than one row
            -- per workdir) so this can grow into a browsable history later —
            -- today's reads take the most recent row per workdir (LIMIT 1).
            CREATE TABLE IF NOT EXISTS last_prompts (
                id TEXT PRIMARY KEY,
                workdir TEXT NOT NULL,
                prompt_text TEXT NOT NULL,
                builder_state TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_last_prompts_workdir_created
                ON last_prompts(workdir, created_at DESC);

            -- CM5: ephemeral subagent runs. Not `agents` (no trigger, no
            -- schedule, no permanent state) and not `graph_runs` (no spec/graph).
            -- A row lives only until it is collected or its TTL (`expires_at`)
            -- passes; the health routine deletes both on its periodic tick.
            -- CB43: `platform`/`model` below mean RESOLVED at dispatch (the
            -- model actually handed to the CLI argv; NULL when the platform's
            -- `model_flag` cannot select one) — never the requested value when
            -- it was not applied.
            CREATE TABLE IF NOT EXISTS subagent_runs (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                model TEXT,
                prompt TEXT NOT NULL,
                workdir TEXT NOT NULL,
                mcp_surface TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                exit_code INTEGER,
                stdout TEXT,
                stderr TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                collected_at TEXT,
                expires_at TEXT NOT NULL,
                pid INTEGER,
                boot_id TEXT
            );

            -- CM18: tombstones for blocking `subagent_spawn` deliveries. A
            -- blocking spawn returns its result inline and deletes the
            -- `subagent_runs` row immediately, so a later `subagent_collect`
            -- would otherwise be indistinguishable from never-existed.
            -- A tombstone records the delivered id until the original
            -- `expires_at`, letting `collect` answer already-delivered.
            -- Async rows never touch this table (no change to async
            -- storage or TTL).
            CREATE TABLE IF NOT EXISTS subagent_delivered_tombstones (
                id TEXT PRIMARY KEY,
                delivered_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sandbox_runs (
                id TEXT PRIMARY KEY,
                project_hash TEXT NOT NULL,
                base_branch TEXT NOT NULL,
                sandbox_branch TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                cli_name TEXT NOT NULL,
                original_workdir TEXT NOT NULL,
                owner_type TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                cleanup_error TEXT
            );",
        )
        .map_err(|e| Self::describe_sql_failure("base schema batch", &e))?;

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

        // `active_run_queue_id` persists which queue (if any) a graph's current/
        // last run drew from, so an interrupted run (quota failure, daemon
        // crash) can be resumed against the same queue by every resume path
        // (scheduled autorun, `graph_reset`) instead of falling back to the
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

        Self::set_schema_version(&conn)?;

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn insert_subagent_run(
        &self,
        id: &str,
        platform: &str,
        model: Option<&str>,
        prompt: &str,
        workdir: &str,
        started_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO subagent_runs (id, platform, model, prompt, workdir, status, started_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'running', ?6, ?7)",
            rusqlite::params![id, platform, model, prompt, workdir, started_at, expires_at],
        )?;
        Ok(())
    }

    pub fn set_subagent_run_pid(&self, id: &str, pid: i64, boot_id: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE subagent_runs SET pid = ?1, boot_id = ?2 WHERE id = ?3",
            rusqlite::params![pid, boot_id, id],
        )?;
        Ok(())
    }

    pub fn complete_subagent_run(
        &self,
        id: &str,
        exit_code: i32,
        stdout: &str,
        stderr: &str,
        mcp_surface: Option<&str>,
        finished_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE subagent_runs SET status = 'finished', exit_code = ?1, stdout = ?2, stderr = ?3, mcp_surface = ?4, finished_at = ?5 WHERE id = ?6",
            rusqlite::params![exit_code, stdout, stderr, mcp_surface, finished_at, id],
        )?;
        Ok(())
    }

    pub fn fail_subagent_run(
        &self,
        id: &str,
        stderr: &str,
        mcp_surface: Option<&str>,
        finished_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE subagent_runs SET status = 'failed', stderr = ?1, mcp_surface = ?2, finished_at = ?3 WHERE id = ?4",
            rusqlite::params![stderr, mcp_surface, finished_at, id],
        )?;
        Ok(())
    }

    #[allow(dead_code)]
    pub fn get_subagent_run(&self, id: &str) -> Result<Option<SubagentRunRecord>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, platform, model, prompt, workdir, mcp_surface, status, exit_code, stdout, stderr, started_at, finished_at, collected_at, expires_at
             FROM subagent_runs WHERE id = ?1",
            [id],
            |row| {
                Ok(SubagentRunRecord {
                    id: row.get(0)?,
                    platform: row.get(1)?,
                    model: row.get(2)?,
                    prompt: row.get(3)?,
                    workdir: row.get(4)?,
                    mcp_surface: row.get(5)?,
                    status: row.get(6)?,
                    exit_code: row.get(7)?,
                    stdout: row.get(8)?,
                    stderr: row.get(9)?,
                    started_at: row.get(10)?,
                    finished_at: row.get(11)?,
                    collected_at: row.get(12)?,
                    expires_at: row.get(13)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn collect_subagent_run(&self, id: &str) -> Result<Option<SubagentRunRecord>> {
        let conn = self.conn.lock().unwrap();
        let record = conn
            .query_row(
                "SELECT id, platform, model, prompt, workdir, mcp_surface, status, exit_code, stdout, stderr, started_at, finished_at, collected_at, expires_at
                 FROM subagent_runs WHERE id = ?1",
                [id],
                |row| {
                    Ok(SubagentRunRecord {
                        id: row.get(0)?,
                        platform: row.get(1)?,
                        model: row.get(2)?,
                        prompt: row.get(3)?,
                        workdir: row.get(4)?,
                        mcp_surface: row.get(5)?,
                        status: row.get(6)?,
                        exit_code: row.get(7)?,
                        stdout: row.get(8)?,
                        stderr: row.get(9)?,
                        started_at: row.get(10)?,
                        finished_at: row.get(11)?,
                        collected_at: row.get(12)?,
                        expires_at: row.get(13)?,
                    })
                },
            )
            .optional()?;
        // Discard the row only once it holds a real result. A collect that
        // races the still-`running` subagent returns the "running" status but
        // leaves the row so a later collect can retrieve the actual output;
        // an abandoned running row is caught by `expire_subagent_runs`.
        if record
            .as_ref()
            .is_some_and(|r| r.status == "finished" || r.status == "failed")
        {
            conn.execute("DELETE FROM subagent_runs WHERE id = ?1", [id])?;
        }
        Ok(record)
    }

    pub fn expire_subagent_runs(&self) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let count = conn.execute(
            "DELETE FROM subagent_runs WHERE collected_at IS NOT NULL OR expires_at < ?1",
            [&now],
        )?;
        // CM18: tombstones expire on the same schedule as the rows they
        // stand in for, using the original `expires_at` copied at delivery.
        let tombstones = conn.execute(
            "DELETE FROM subagent_delivered_tombstones WHERE expires_at < ?1",
            [&now],
        )?;
        Ok((count + tombstones) as u64)
    }

    /// CM18: record that a blocking spawn delivered `id` inline. The
    /// `subagent_runs` row is already deleted; this tombstone lets a later
    /// `subagent_collect` answer "already delivered" instead of "not found".
    /// `expires_at` must be the original run's expiry so natural expiry
    /// cleans the tombstone on the same schedule.
    pub fn insert_delivered_tombstone(
        &self,
        id: &str,
        delivered_at: &str,
        expires_at: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO subagent_delivered_tombstones (id, delivered_at, expires_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![id, delivered_at, expires_at],
        )?;
        Ok(())
    }

    /// CM18: true if `id` was delivered by a blocking spawn (and its
    /// tombstone has not yet expired).
    pub fn is_delivered_tombstone(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM subagent_delivered_tombstones WHERE id = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    #[allow(dead_code)]
    pub fn delete_delivered_tombstone(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM subagent_delivered_tombstones WHERE id = ?1",
            [id],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SubagentRunRecord {
    pub id: String,
    pub platform: String,
    pub model: Option<String>,
    pub prompt: String,
    pub workdir: String,
    pub mcp_surface: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub collected_at: Option<String>,
    pub expires_at: String,
}

pub mod achievements;
pub mod activity_log;
pub mod agent;
pub mod blueprints;
pub mod clean;
pub mod ensembles;
pub mod gamification;
pub mod graph_transfer;
pub mod graphs;
pub mod group;
pub mod health;
pub mod intelligence;
pub mod last_prompts;
pub mod project;
pub mod queues;
pub mod run;
pub mod sandbox;
pub mod scheduled_sends;
pub mod seeds;
pub mod session;
pub mod state;
pub mod sync;

#[cfg(test)]
pub use crate::application::ports::{AgentRepository, RunRepository, StateRepository};

#[cfg(test)]
mod tests;
