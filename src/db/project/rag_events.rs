use anyhow::Result;

use crate::db::Database;

use super::{RagFileEvent, RagPerFileStatus};

fn row_to_rag_file_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<RagFileEvent> {
    Ok(RagFileEvent {
        id: row.get(0)?,
        file_path: row.get(1)?,
        event_type: row.get(2)?,
        detail: row.get(3)?,
        occurred_at: row.get(4)?,
    })
}

impl Database {
    pub fn clear_rag_file_events(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute("DELETE FROM rag_file_events", [])?;
        Ok(())
    }

    /// Append a lifecycle event for a RAG file.
    pub fn log_rag_event(
        &self,
        file_path: &str,
        event_type: &str,
        detail: Option<&str>,
        occurred_at: i64,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO rag_file_events (file_path, event_type, detail, occurred_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![file_path, event_type, detail, occurred_at],
        )?;
        Ok(())
    }

    /// Return all events for a specific file, newest-first.
    pub fn rag_events_for_file(&self, file_path: &str) -> Result<Vec<RagFileEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, event_type, detail, occurred_at
               FROM rag_file_events
              WHERE file_path = ?1
              ORDER BY occurred_at DESC, id DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![file_path], row_to_rag_file_event)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Return all events, newest-first, optionally limited.
    pub fn list_rag_events(&self, limit: usize) -> Result<Vec<RagFileEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, event_type, detail, occurred_at
               FROM rag_file_events
              ORDER BY occurred_at DESC, id DESC
              LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], row_to_rag_file_event)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Return the most-recent error event per file path.
    pub fn rag_last_error_per_file(&self) -> Result<Vec<RagFileEvent>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, file_path, event_type, detail, occurred_at
               FROM rag_file_events
              WHERE event_type = 'error'
                AND occurred_at = (
                    SELECT MAX(occurred_at) FROM rag_file_events e2
                     WHERE e2.file_path = rag_file_events.file_path
                       AND e2.event_type = 'error'
                )
              ORDER BY file_path",
        )?;
        let rows = stmt.query_map([], row_to_rag_file_event)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Return a map of file_path → last_indexed_at (unix seconds) for all files
    /// that have at least one successful `"indexed"` event and were not later
    /// deleted. Error events do not invalidate the last successful timestamp.
    /// Used at startup to skip re-indexing files that haven't changed since
    /// they were last indexed successfully.
    pub fn indexed_files_timestamps(&self) -> Result<std::collections::HashMap<String, i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        // Keep the last successful index timestamp per file unless a later delete
        // exists. This avoids full startup re-index graphs after transient errors.
        let mut stmt = conn.prepare(
            "SELECT s.file_path, s.last_indexed_at
               FROM (
                   SELECT
                       file_path,
                       MAX(CASE WHEN event_type = 'indexed' THEN occurred_at END) AS last_indexed_at,
                       MAX(CASE WHEN event_type = 'deleted' THEN occurred_at END) AS last_deleted_at
                   FROM rag_file_events
                   GROUP BY file_path
               ) s
              WHERE s.last_indexed_at IS NOT NULL
                AND (s.last_deleted_at IS NULL OR s.last_indexed_at > s.last_deleted_at)",
        )?;
        let mut map = std::collections::HashMap::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let ts: i64 = row.get(1)?;
            map.insert(path, ts);
        }
        Ok(map)
    }

    /// Count of `"error"` events recorded for a given file path.
    pub fn rag_error_count(&self, file_path: &str) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rag_file_events WHERE file_path = ?1 AND event_type = 'error'",
            rusqlite::params![file_path],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Return the set of file paths that have been permanently given up on:
    /// at least one `"failed"` event with no later `"indexed"` event. A
    /// successful re-index (e.g. after a manual re-add) clears the file from
    /// this set, mirroring how `indexed_files_timestamps` treats `"deleted"`.
    pub fn permanently_failed_rag_files(&self) -> Result<std::collections::HashSet<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT file_path
               FROM (
                   SELECT
                       file_path,
                       MAX(CASE WHEN event_type = 'failed' THEN occurred_at END) AS last_failed_at,
                       MAX(CASE WHEN event_type = 'indexed' THEN occurred_at END) AS last_indexed_at
                   FROM rag_file_events
                   GROUP BY file_path
               ) s
              WHERE s.last_failed_at IS NOT NULL
                AND (s.last_indexed_at IS NULL OR s.last_failed_at > s.last_indexed_at)",
        )?;
        let mut set = std::collections::HashSet::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            set.insert(path);
        }
        Ok(set)
    }
}

impl Database {
    /// Return one summary row per file: last event, last detail, index count.
    /// Results are ordered by last activity time, newest first.
    pub fn rag_per_file_status(&self) -> Result<Vec<RagPerFileStatus>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT
                 e.file_path,
                 e.event_type,
                 e.detail,
                 COALESCE(ic.cnt, 0),
                 e.occurred_at
             FROM rag_file_events e
             INNER JOIN (
                 SELECT file_path, MAX(occurred_at) AS max_at
                   FROM rag_file_events
                  GROUP BY file_path
             ) latest ON e.file_path = latest.file_path
                     AND e.occurred_at = latest.max_at
             LEFT JOIN (
                 SELECT file_path, COUNT(*) AS cnt
                   FROM rag_file_events
                  WHERE event_type = 'indexed'
                  GROUP BY file_path
             ) ic ON e.file_path = ic.file_path
             ORDER BY e.occurred_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(RagPerFileStatus {
                file_path: row.get(0)?,
                last_event_type: row.get(1)?,
                last_detail: row.get(2)?,
                times_indexed: row.get(3)?,
                last_at: row.get(4)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}
