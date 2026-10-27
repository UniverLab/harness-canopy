//! `shelf_files` rows: what the session shelf holds. Every read goes
//! through here; the bytes themselves live under `~/.canopy/shelf/<file_id>`
//! (see `crate::sessions::shelf`), never in SQLite.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::db::Database;

/// The `shelf_files` table plus its indexes, shared by the base schema
/// (`BASE_SCHEMA_BATCHES`, the same shape as
/// `mutex_store::MUTEX_TABLES_SCHEMA`) and the migration so the two can
/// never drift. `session_id: NULL` is the host shelf; a session's files
/// outlive it (7-day retention), so there is deliberately no foreign key.
pub(super) const SHELF_FILES_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS shelf_files (
                file_id TEXT PRIMARY KEY,
                session_id TEXT,
                name TEXT NOT NULL,
                mime TEXT NOT NULL,
                size INTEGER NOT NULL,
                sha256 TEXT NOT NULL,
                caption TEXT CHECK (caption IS NULL OR length(caption) <= 500),
                from_label TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_shelf_files_session_created
                ON shelf_files(session_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_shelf_files_created
                ON shelf_files(created_at DESC);";

/// One shelf file's metadata row. `session_id: None` is the host shelf.
#[derive(Debug, Clone)]
pub struct ShelfRow {
    pub file_id: String,
    pub session_id: Option<String>,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub sha256: String,
    pub caption: Option<String>,
    pub from_label: String,
    /// RFC 3339 UTC; also the ordering key (same format on every write, so
    /// lexicographic order is chronological order).
    pub created_at: String,
}

/// Every column [`ShelfRow`] needs, in the order `shelf_from_row` reads them.
const SHELF_COLUMNS: &str =
    "file_id, session_id, name, mime, size, sha256, caption, from_label, created_at";

fn shelf_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ShelfRow> {
    Ok(ShelfRow {
        file_id: row.get(0)?,
        session_id: row.get(1)?,
        name: row.get(2)?,
        mime: row.get(3)?,
        size: row.get(4)?,
        sha256: row.get(5)?,
        caption: row.get(6)?,
        from_label: row.get(7)?,
        created_at: row.get(8)?,
    })
}

impl Database {
    /// Insert one shelf row; the caller has already written the file bytes
    /// to a temp name in the shelf directory (see `ShelfStore::insert`).
    pub fn insert_shelf_file(&self, row: &ShelfRow) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO shelf_files (file_id, session_id, name, mime, size, sha256, caption, from_label, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.file_id,
                row.session_id,
                row.name,
                row.mime,
                row.size,
                row.sha256,
                row.caption,
                row.from_label,
                row.created_at,
            ],
        )?;
        Ok(())
    }

    /// One shelf's files, newest first. `session_id: None` lists the host
    /// shelf (rows with no session).
    pub fn list_shelf_files(&self, session_id: Option<&str>) -> Result<Vec<ShelfRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let sql = format!(
            "SELECT {SHELF_COLUMNS} FROM shelf_files
             WHERE session_id IS ?1
             ORDER BY created_at DESC, file_id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params![session_id], shelf_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// One file by id, searched across every shelf (`shelf_get` addresses
    /// a `file_id` only, never a path).
    pub fn get_shelf_file(&self, file_id: &str) -> Result<Option<ShelfRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SHELF_COLUMNS} FROM shelf_files WHERE file_id = ?1"
        ))?;
        let found = stmt
            .query_row(params![file_id], shelf_from_row)
            .optional()?;
        Ok(found)
    }

    /// Every shelf row, oldest first — the order the size cap prunes in.
    pub fn all_shelf_files_oldest_first(&self) -> Result<Vec<ShelfRow>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SHELF_COLUMNS} FROM shelf_files
             ORDER BY created_at ASC, file_id ASC"
        ))?;
        let rows = stmt
            .query_map([], shelf_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete one shelf row; `false` when there was no such row.
    pub fn delete_shelf_file(&self, file_id: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let deleted = conn.execute(
            "DELETE FROM shelf_files WHERE file_id = ?1",
            params![file_id],
        )?;
        Ok(deleted > 0)
    }

    /// Session ids that are still live (active interactive rows and idle
    /// terminal rows) — the set the 7-day shelf retention checks membership
    /// against. A finished, killed or deleted session drops out, which is
    /// when its files start their 7-day clock.
    pub fn live_session_ids(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut ids = std::collections::HashSet::new();
        for sql in [
            "SELECT id FROM interactive_sessions WHERE status = 'active'",
            "SELECT id FROM terminal_sessions WHERE status = 'idle'",
        ] {
            let mut stmt = conn.prepare(sql)?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            for id in rows.flatten() {
                ids.insert(id);
            }
        }
        Ok(ids)
    }

    /// Test-only: age a shelf row's `created_at`, so retention and pruning
    /// tests do not have to wait real days (same pattern as
    /// `mark_session_orphaned`).
    #[cfg(test)]
    pub fn set_shelf_created_at(&self, file_id: &str, created_at: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE shelf_files SET created_at = ?1 WHERE file_id = ?2",
            params![created_at, file_id],
        )?;
        Ok(())
    }
}
