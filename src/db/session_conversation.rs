//! Harness conversation id persistence on `interactive_sessions`.
//!
//! Split from `session.rs`, which sits at the file-size cap.

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use crate::db::Database;

impl Database {
    /// Record the harness's own conversation identifier on a session row.
    /// Best-effort by design (the caller never fails on it) and never
    /// overwrites an identifier already recorded for that row: the first
    /// identifier a session reports is the conversation it belongs to.
    pub fn set_session_harness_conversation_id(
        &self,
        session_id: &str,
        harness_id: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET harness_conversation_id = ?1
              WHERE id = ?2
                AND (harness_conversation_id IS NULL OR harness_conversation_id = '')",
            params![harness_id, session_id],
        )?;
        Ok(())
    }

    /// Forget the recorded harness conversation id: a fresh fallback after a
    /// failed resume is a NEW conversation, and the no-overwrite recorder can
    /// only store its id once the stale one is gone.
    pub fn clear_session_harness_conversation_id(&self, session_id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET harness_conversation_id = NULL WHERE id = ?1",
            params![session_id],
        )?;
        Ok(())
    }

    /// Read the recorded harness conversation id for `session_id`, or `None`
    /// when unset, empty, or the row is missing.
    pub fn get_session_harness_conversation_id(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt =
            conn.prepare("SELECT harness_conversation_id FROM interactive_sessions WHERE id = ?1")?;
        let result: Option<String> = stmt
            .query_row(params![session_id], |row| row.get(0))
            .ok()
            .flatten();
        Ok(result.filter(|id| !id.trim().is_empty()))
    }

    /// All non-empty `harness_conversation_id` values currently claimed by
    /// non-bridge `interactive_sessions` rows, so fallback discovery never
    /// assigns the same conversation id to two sessions.
    pub fn list_claimed_harness_conversation_ids(&self) -> Result<HashSet<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT harness_conversation_id FROM interactive_sessions
              WHERE harness_conversation_id IS NOT NULL
                AND harness_conversation_id != ''
                AND session_type != 'bridge'",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut claimed = HashSet::new();
        for id in rows.flatten() {
            if !id.trim().is_empty() {
                claimed.insert(id);
            }
        }
        Ok(claimed)
    }
}
