use anyhow::Result;

use crate::db::Database;

use super::RagQueueItem;

fn row_to_rag_queue_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<RagQueueItem> {
    Ok(RagQueueItem {
        source_path: row.get(0)?,
        status: row.get(1)?,
        queued_at: row.get(2)?,
    })
}

impl Database {
    // ── RAG queue (SQLite) ──────────────────────────────────────────────

    pub fn clear_rag_queue(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute("DELETE FROM rag_queue", [])?;
        Ok(())
    }

    pub fn enqueue_rag_item(&self, source_path: &str, queued_at: i64) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO rag_queue (source_path, status, queued_at, updated_at)
             VALUES (?1, 'queued', ?2, ?2)
             ON CONFLICT(source_path) DO UPDATE SET
               status='queued',
               queued_at=excluded.queued_at,
               updated_at=excluded.updated_at",
            rusqlite::params![source_path, queued_at],
        )?;
        Ok(())
    }

    pub fn mark_rag_item_processing(&self, source_path: &str, updated_at: i64) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let n = conn.execute(
            "UPDATE rag_queue
             SET status='processing', updated_at=?2
             WHERE source_path=?1",
            rusqlite::params![source_path, updated_at],
        )?;
        Ok(n > 0)
    }

    /// Recover queue items that were left in `processing` after an unexpected
    /// daemon exit. Moves them back to `queued` so indexing can resume.
    pub fn requeue_processing_rag_items(&self, now: i64) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let n = conn.execute(
            "UPDATE rag_queue
             SET status='queued', queued_at=?1, updated_at=?1
             WHERE status='processing'",
            rusqlite::params![now],
        )?;
        Ok(n)
    }

    pub fn remove_rag_item(&self, source_path: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let n = conn.execute(
            "DELETE FROM rag_queue WHERE source_path=?1",
            rusqlite::params![source_path],
        )?;
        Ok(n > 0)
    }

    pub fn list_rag_queue(&self, limit: usize) -> Result<Vec<RagQueueItem>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT source_path, status, queued_at
             FROM rag_queue
             ORDER BY CASE status WHEN 'processing' THEN 0 ELSE 1 END, queued_at ASC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], row_to_rag_queue_item)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    pub fn rag_queue_counts(&self) -> Result<(i64, i64)> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let queued: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rag_queue WHERE status='queued'",
            [],
            |row| row.get(0),
        )?;
        let processing: i64 = conn.query_row(
            "SELECT COUNT(*) FROM rag_queue WHERE status='processing'",
            [],
            |row| row.get(0),
        )?;
        Ok((queued, processing))
    }
}
