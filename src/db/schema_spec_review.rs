use anyhow::Result;
use rusqlite::Connection;

use super::Database;

impl Database {
    /// Spec review: add `review_state`/`review_tier`/`review_findings`/
    /// `created_by_session`, and the per-session review-drain watermark table.
    /// Pre-existing rows default to `approved` (created while review was off).
    pub(in crate::db) fn migrate_spec_review_columns(conn: &Connection) -> Result<()> {
        for (column, ddl) in [
            (
                "review_state",
                "ALTER TABLE graph_specs ADD COLUMN review_state TEXT NOT NULL DEFAULT 'approved'",
            ),
            (
                "review_tier",
                "ALTER TABLE graph_specs ADD COLUMN review_tier TEXT",
            ),
            (
                "review_findings",
                "ALTER TABLE graph_specs ADD COLUMN review_findings TEXT",
            ),
            (
                "created_by_session",
                "ALTER TABLE graph_specs ADD COLUMN created_by_session TEXT",
            ),
        ] {
            let has: bool = conn
                .query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('graph_specs') WHERE name = ?1",
                    [column],
                    |row| Ok(row.get::<_, i32>(0)? > 0),
                )
                .unwrap_or(false);
            if !has {
                conn.execute(ddl, [])
                    .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
            }
        }
        conn.execute(
            "CREATE TABLE IF NOT EXISTS spec_review_drains (
                session_id TEXT PRIMARY KEY,
                drained_at_ms INTEGER NOT NULL
            )",
            [],
        )
        .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }
}
