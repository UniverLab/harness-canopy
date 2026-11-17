//! Session finish writes that carry the failure detail atomically.
//!
//! Split from `session.rs`, which sits at the file-size cap.

use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use crate::db::Database;

impl Database {
    /// [`finish_interactive_session`](Database::finish_interactive_session)
    /// with the row's failure detail in the SAME update: the status flip and
    /// the `exit_detail` write commit together, so a reader that observes
    /// `status = 'error'` can never catch the detail still in flight (the
    /// two-call sequence left a window where the row read as a failed
    /// session with no explanation). `None` clears any earlier detail,
    /// exactly like the clean-exit path's separate clear did.
    pub fn finish_interactive_session_with_detail(
        &self,
        id: &str,
        exit_code: i32,
        detail: Option<&str>,
    ) -> Result<()> {
        let status = if exit_code == 0 { "completed" } else { "error" };
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET exited_at = ?1, exit_code = ?2, status = ?3, exit_detail = ?4 WHERE id = ?5",
            params![Utc::now().to_rfc3339(), exit_code, status, detail, id],
        )?;
        Ok(())
    }
}
