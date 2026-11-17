//! Cron-tick prompt schema migration: the `graphs.trigger_prompt` column.
//! Split out of `schema.rs` to keep every production file within the
//! file-size gate.
use anyhow::Result;
use rusqlite::Connection;

use super::super::Database;

impl Database {
    /// Add graph `trigger_prompt`: the prompt a cron tick hands a spec-less
    /// graph as its idea (the text `{{spec_content}}` renders). Part of the
    /// trigger, never part of an exported graph document. `NULL` on every
    /// pre-existing row (no graph had one), so every existing cron graph keeps
    /// its current "all specs completed; skipping" tick behaviour until an
    /// operator sets one.
    pub(in crate::db) fn migrate_graph_trigger_prompt_column(conn: &Connection) -> Result<()> {
        let has_trigger_prompt: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('graphs') WHERE name = 'trigger_prompt'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_trigger_prompt {
            conn.execute("ALTER TABLE graphs ADD COLUMN trigger_prompt TEXT", [])
                .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}
