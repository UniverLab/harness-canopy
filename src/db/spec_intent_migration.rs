//! Spec review: add the `graph_specs.intent` column.
//!
//! `intent` is the short human-approved sentence a spec carries when the
//! adversarial spec review is on. The column is nullable: every pre-existing
//! row migrates to `NULL`, and a spec created while the review is off keeps
//! `NULL` too — the migration never invents an intent.

use anyhow::Result;
use rusqlite::Connection;

use super::Database;

impl Database {
    /// Add `graph_specs.intent` when the table predates it. A fresh database
    /// already declares the column in `BASE_SCHEMA_BATCHES`, so this is a
    /// guarded no-op there.
    pub(in crate::db) fn migrate_spec_intent_column(conn: &Connection) -> Result<()> {
        let has_intent: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = 'intent'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_intent {
            conn.execute("ALTER TABLE graph_specs ADD COLUMN intent TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}
