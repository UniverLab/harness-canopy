//! Named-mutex schema migrations: the `ensembles.mutex` column and the
//! `graph_events.kind` vocabulary widening. Split out of `schema.rs` to keep
//! every production file within the file-size gate.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

use super::super::Database;

impl Database {
    /// Named mutexes: add the `ensembles.mutex` column (JSON list of resource
    /// names, NULL for none) if missing. Fresh databases get it from the base
    /// schema batch; the `mutex_leases`/`mutex_waiters` tables need no
    /// migration of their own (brand-new `CREATE TABLE IF NOT EXISTS` in the
    /// base batch, which `create_base_schema` runs on every open).
    pub(in crate::db) fn migrate_ensemble_mutex_column(conn: &Connection) -> Result<()> {
        let has_column: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'mutex'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_column {
            conn.execute("ALTER TABLE ensembles ADD COLUMN mutex TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }

    /// Named mutexes: widen the `graph_events.kind` CHECK to admit
    /// `mutex_waiting`/`mutex_acquired`. SQLite cannot `ALTER` a CHECK, so —
    /// like `migrate_cb52_ensemble_fk` — the table is copied and renamed, with
    /// the replacement DDL derived from the stored DDL by swapping exactly
    /// the vocabulary tail, so later columns survive untouched. A no-op on a
    /// fresh database (created with the new vocabulary by
    /// `migrate_graph_events_table`) and once rebuilt.
    pub(in crate::db) fn migrate_graph_events_mutex_kinds(conn: &Connection) -> Result<()> {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'graph_events'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(sql) = sql else {
            return Ok(());
        };
        if sql.contains("mutex_waiting") {
            return Ok(());
        }
        let Some(tail) = sql.find("'note'") else {
            anyhow::bail!("graph_events mutex migration failed: unexpected stored DDL");
        };
        let mut new_sql = sql;
        new_sql.insert_str(tail + "'note'".len(), ",'mutex_waiting','mutex_acquired'");
        let Some(first) = new_sql.find("graph_events") else {
            anyhow::bail!("graph_events mutex migration failed: unexpected stored DDL");
        };
        new_sql.replace_range(first..first + "graph_events".len(), "graph_events_new");
        let mut stmt = conn.prepare("SELECT name FROM pragma_table_info('graph_events')")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if cols.is_empty() {
            anyhow::bail!("graph_events mutex migration failed: table has no columns");
        }
        let collist = cols.join(", ");

        conn.execute_batch("PRAGMA foreign_keys=OFF;")?;
        let rebuild = (|| -> Result<()> {
            conn.execute_batch("BEGIN TRANSACTION;")?;
            for step in [
                new_sql.clone(),
                format!(
                    "INSERT INTO graph_events_new ({collist}) SELECT {collist} FROM graph_events;"
                ),
                "DROP TABLE graph_events;".to_string(),
                "ALTER TABLE graph_events_new RENAME TO graph_events;".to_string(),
                "CREATE INDEX IF NOT EXISTS idx_graph_events_graph_id_id ON graph_events(graph_id, id);"
                    .to_string(),
            ] {
                if let Err(e) = conn.execute_batch(&step) {
                    let _ = conn.execute_batch("ROLLBACK;");
                    anyhow::bail!("graph_events mutex migration failed: {e}");
                }
            }
            if let Err(e) = conn.execute_batch("COMMIT;") {
                let _ = conn.execute_batch("ROLLBACK;");
                anyhow::bail!("graph_events mutex migration failed: {e}");
            }
            Ok(())
        })();
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        rebuild?;
        Ok(())
    }
}
