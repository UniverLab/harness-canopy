use anyhow::Result;
use rusqlite::Connection;

use super::Database;

impl Database {
    /// Add the `ensembles.read_only` column.
    pub(in crate::db) fn migrate_ensemble_read_only_column(conn: &Connection) -> Result<()> {
        let has: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('ensembles') WHERE name = 'read_only'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has {
            conn.execute(
                "ALTER TABLE ensembles ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}
