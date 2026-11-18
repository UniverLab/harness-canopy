use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use super::Database;
use crate::domain::graphs::GraphSpec;

impl Database {
    /// Specs of `session_id` finished (approved/rejected/review_error) since
    /// the session's last review-drain watermark, oldest-updated first.
    fn finished_specs_since_locked(
        conn: &rusqlite::Connection,
        session_id: &str,
        drained_at_ms: i64,
    ) -> Result<Vec<GraphSpec>> {
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, spec_type, spec_scope, spec_code, intent, review_state, review_tier, review_findings, created_by_session
             FROM graph_specs
             WHERE created_by_session = ?1
               AND review_state IN ('approved', 'rejected', 'review_error')
               AND updated_at > ?2
             ORDER BY updated_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(
            params![session_id, drained_at_ms],
            super::graphs::map_graph_spec_row,
        )?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn has_pending_spec_reviews_locked(
        conn: &rusqlite::Connection,
        session_id: &str,
    ) -> Result<bool> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_specs
             WHERE created_by_session = ?1 AND review_state IN ('unreviewed', 'in_review')",
            params![session_id],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    fn drain_watermark_locked(conn: &rusqlite::Connection, session_id: &str) -> Result<i64> {
        let wm: Option<i64> = conn
            .query_row(
                "SELECT drained_at_ms FROM spec_review_drains WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(wm.unwrap_or(0))
    }

    fn upsert_drain_watermark_locked(
        conn: &rusqlite::Connection,
        session_id: &str,
        drained_at_ms: i64,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO spec_review_drains (session_id, drained_at_ms) VALUES (?1, ?2)
             ON CONFLICT(session_id) DO UPDATE SET drained_at_ms = excluded.drained_at_ms",
            params![session_id, drained_at_ms],
        )?;
        Ok(())
    }

    /// Atomically collect and claim a drain for `session_id`: when no spec of
    /// the session is unreviewed/in_review and at least one finished spec is
    /// newer than the watermark, returns those specs and advances the watermark
    /// to the max `updated_at` of the set. Returns `None` when nothing to
    /// drain or reviews are still pending. The watermark advance is the claim,
    /// so a concurrent caller observing the new watermark sees an empty set.
    pub fn claim_spec_review_drain(&self, session_id: &str) -> Result<Option<Vec<GraphSpec>>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        if Self::has_pending_spec_reviews_locked(&conn, session_id)? {
            return Ok(None);
        }
        let watermark = Self::drain_watermark_locked(&conn, session_id)?;
        let specs = Self::finished_specs_since_locked(&conn, session_id, watermark)?;
        if specs.is_empty() {
            return Ok(None);
        }
        let new_wm = specs
            .iter()
            .filter_map(|s| self.spec_updated_at_locked(&conn, &s.id).ok().flatten())
            .max()
            .unwrap_or(watermark.saturating_add(1));
        Self::upsert_drain_watermark_locked(&conn, session_id, new_wm)?;
        Ok(Some(specs))
    }

    fn spec_updated_at_locked(
        &self,
        conn: &rusqlite::Connection,
        spec_id: &str,
    ) -> Result<Option<i64>> {
        let v: Option<i64> = conn
            .query_row(
                "SELECT updated_at FROM graph_specs WHERE id = ?1",
                params![spec_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(v)
    }
}
