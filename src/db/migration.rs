use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use super::{Database, SCHEMA_VERSION};

mod late;
mod mutex_schema;

impl Database {
    /// Refuses to open a database stamped with a schema version newer than
    /// this binary knows about. Without this, an older binary meeting a
    /// renamed/restructured schema would pass every `IF NOT EXISTS` guard
    /// (the old names it looks for are simply gone) and start up believing
    /// every table is empty — silent data loss from its perspective. A
    /// missing `schema_version` row (pre-dates this check, or a brand new
    /// database) is not a mismatch; every migration below is still
    /// self-guarding for that case.
    pub(super) fn check_schema_version(conn: &Connection) -> Result<()> {
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

    pub(super) fn set_schema_version(conn: &Connection) -> Result<()> {
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
    pub(super) fn migrate_legacy_queue_schema(conn: &Connection) -> Result<()> {
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
    pub(super) fn migrate_loop_to_graph_schema(conn: &Connection) -> Result<()> {
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

        Self::rename_loop_tables(conn)?;
        Self::rename_loop_columns(conn)?;
        Self::migrate_loop_edge_conditions(conn)?;
        Self::replace_loop_template_markers(conn)?;
        Self::rebuild_loop_indexes(conn)?;
        Self::assert_no_foreign_key_violations(conn, "graph to graph schema migration failed")?;
        Ok(())
    }

    // RETIRED-VOCAB-BEGIN (CB63: this migration rewrites stored rows and
    // must name the retired prefix to do it; exempt from
    // `no_retired_vocabulary_in_user_visible_surfaces`)
    /// CB63: the engine's activity attribution was `loop:<name>` until the
    /// loop→graph rename reached it (CC3 moved everything else). Rows written
    /// by pre-CB63 binaries still carry the old prefix in
    /// `activity_log.source`; rewrite the anchored prefix to `graph:`.
    /// Idempotent by construction — the `LIKE 'loop:%'` predicate matches no
    /// row once rewritten — so safe on every startup, the same
    /// guarded-and-re-runnable shape as `migrate_loop_to_graph_schema` (CC3).
    pub(super) fn migrate_activity_attribution_prefix(conn: &Connection) -> Result<()> {
        let has_activity_log: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'activity_log'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_activity_log {
            return Ok(());
        }
        // Anchored prefix rewrite, deliberately not
        // `REPLACE(source, 'loop:', 'graph:')`: REPLACE would also rewrite a
        // graph *name* that contains "loop:" (e.g. a graph named
        // "loop:keeper"), corrupting the attribution. `substr(source, 6)`
        // drops the 5-byte `loop:` prefix (SQLite substr is 1-based).
        conn.execute(
            "UPDATE activity_log
                SET source = 'graph:' || substr(source, 6)
              WHERE source LIKE 'loop:%'",
            [],
        )
        .map_err(|e| anyhow::anyhow!("activity attribution migration failed: {e}"))?;
        Ok(())
    }
    // RETIRED-VOCAB-END

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
    pub(super) fn migrate_cb52_ensemble_fk(conn: &Connection) -> Result<()> {
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

    /// Rename the six retired `loop*` tables to their `graph*` names.
    fn rename_loop_tables(conn: &Connection) -> Result<()> {
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
        Ok(())
    }

    /// Rename the retired `loop_id` / queue-era columns, each guarded by
    /// `pragma_table_info` (older databases may lack `loop_id` on tables that
    /// gained top-level targeting later, and a re-run must never fail on an
    /// already renamed column). The `graphs.active_run_pool_id` / `spec_pool`
    /// guards cover the ordering edge: `migrate_legacy_queue_schema`
    /// runs first but looks for `graphs`, so a database that still has
    /// both `pools` and `graphs` reaches this function with its
    /// queue-era columns untouched — renamed here instead of orphaned.
    fn rename_loop_columns(conn: &Connection) -> Result<()> {
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
        Ok(())
    }

    /// Rewrite the retired `break` edge condition to `error`, on both edges
    /// and (when the column exists) ensembles' entry condition.
    fn migrate_loop_edge_conditions(conn: &Connection) -> Result<()> {
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
        Ok(())
    }

    /// Replace the retired `{{loop_name}}` template marker in stored node
    /// config with `{{graph_name}}`.
    fn replace_loop_template_markers(conn: &Connection) -> Result<()> {
        conn.execute(
            "UPDATE graph_nodes SET config = REPLACE(config, '{{loop_name}}', '{{graph_name}}')
                WHERE config LIKE '%{{loop_name}}%'",
            [],
        )
        .map_err(|e| anyhow::anyhow!("graph to graph schema migration failed: {e}"))?;
        Ok(())
    }

    /// Drop the retired `idx_loop_*` indexes and create their `idx_graph_*`
    /// replacements.
    fn rebuild_loop_indexes(conn: &Connection) -> Result<()> {
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
        Ok(())
    }

    /// CM46: durable per-graph event log. A brand-new table, so a guarded
    /// `CREATE TABLE IF NOT EXISTS` batch matches `migrate_graph_run_output_tails`
    /// and `migrate_hook_launch_tracking` rather than an `ALTER`. The CHECK
    /// constraint pins `kind` to the published vocabulary at the DB level.
    pub(super) fn migrate_graph_events_table(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS graph_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                graph_id TEXT NOT NULL,
                spec_id TEXT,
                node_id TEXT,
                run_id TEXT,
                kind TEXT NOT NULL
                    CHECK (kind IN (
                        'graph_started','graph_paused','graph_resumed','graph_completed','graph_failed','graph_blocked',
                        'spec_started','spec_completed','spec_failed','spec_skipped',
                        'node_started','node_finished',
                        'hook_fired',
                        'config_changed',
                        'note',
                        'mutex_waiting','mutex_acquired'
                    )),
                message TEXT NOT NULL,
                payload TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_graph_events_graph_id_id ON graph_events(graph_id, id);
            ",
        )
        .map_err(|e| anyhow::anyhow!("graph_events schema migration failed: {e}"))?;
        Ok(())
    }

    /// CM47: one working-memory note per spec owner. The `body` CHECK is a
    /// hard backstop; the application layer refuses long bodies with the
    /// named message the spec requires. Deleting the spec deletes its notes
    /// via `ON DELETE CASCADE` (`PRAGMA foreign_keys=ON` is set per
    /// connection), so `delete_graph_spec` needs no change.
    pub(super) fn migrate_spec_notes_table(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS spec_notes (
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                owner_kind TEXT NOT NULL
                    CHECK (owner_kind IN ('node','ensemble','operator')),
                owner_id TEXT NOT NULL,
                owner_name TEXT NOT NULL,
                body TEXT NOT NULL CHECK (length(body) <= 2500),
                author_platform TEXT,
                author_model TEXT,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (spec_id, owner_kind, owner_id)
            );
            CREATE INDEX IF NOT EXISTS idx_spec_notes_spec
                ON spec_notes(spec_id);",
        )
        .map_err(|e| anyhow::anyhow!("spec_notes schema migration failed: {e}"))?;
        Ok(())
    }

    /// Adds `interactive_sessions.harness_conversation_id`: the harness's own
    /// conversation identifier (Claude Code session/transcript id, opencode
    /// session id, muse resume id). A resume of the same harness conversation
    /// is matched on this column, so the session keeps its canopy id and name
    /// instead of appearing as a second, newly-named row. Guarded on
    /// `pragma_table_info` and `CREATE INDEX IF NOT EXISTS`, so it is safe on
    /// both a fresh database (the `CREATE TABLE` batch already has the column)
    /// and an already-migrated one.
    pub(super) fn migrate_interactive_harness_column(conn: &Connection) -> Result<()> {
        let has_harness_id: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('interactive_sessions') WHERE name = 'harness_conversation_id'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_harness_id {
            conn.execute(
                "ALTER TABLE interactive_sessions ADD COLUMN harness_conversation_id TEXT",
                [],
            )
            .map_err(|e| anyhow::anyhow!("interactive harness column migration failed: {e}"))?;
        }
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_interactive_sessions_harness
                ON interactive_sessions(harness_conversation_id)",
            [],
        )
        .map_err(|e| anyhow::anyhow!("interactive harness column migration failed: {e}"))?;
        Ok(())
    }

    /// Fail the migration when any foreign-key violation remains, naming the
    /// step in the message (observable via `canopy doctor`).
    fn assert_no_foreign_key_violations(conn: &Connection, migration_label: &str) -> Result<()> {
        let fk_violations: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .map_err(|e| anyhow::anyhow!("{migration_label}: {e}"))?;
        if fk_violations > 0 {
            anyhow::bail!("{migration_label}: {fk_violations} foreign key violation(s) detected after migration");
        }
        Ok(())
    }
    // RETIRED-SCHEMA-NAME-END
}
