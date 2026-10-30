use anyhow::Result;
use chrono::Utc;
use rusqlite::params;

use crate::db::Database;

/// Record of an interactive agent session (persisted in SQLite).
#[allow(dead_code)]
#[derive(Clone)]
pub struct InteractiveSession {
    pub id: String,
    pub name: String,
    pub cli: String,
    pub working_dir: String,
    pub args: Option<String>,
    pub started_at: String,
    pub status: String,
    pub session_type: String,
    pub pid: Option<i64>,
    /// Machine boot id recorded when the session started (see
    /// `system::boot_id`). NULL for rows written before this column existed.
    pub boot_id: Option<String>,
    /// The harness's own conversation identifier (Claude Code session /
    /// transcript id, opencode session id, muse resume id). A later
    /// registration carrying the same value is the same conversation resumed,
    /// so it reuses this row instead of minting a new id and name. NULL for
    /// rows written before this column existed and for registrations that
    /// carry no harness identifier.
    pub harness_conversation_id: Option<String>,
}

/// Every column [`InteractiveSession`] needs, in the order
/// `session_from_row` reads them.
const SESSION_COLUMNS: &str = "id, name, cli, working_dir, args, started_at, status, \
     session_type, pid, boot_id, harness_conversation_id";

/// Ids of `active`/`resumed` rows whose process is already gone, per
/// [`crate::system::session_process_is_live`]. Split out so the reconcile
/// update borrows no live statement while it writes.
fn stale_interactive_session_ids(
    conn: &rusqlite::Connection,
    current_boot_id: Option<&str>,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT id, pid, boot_id FROM interactive_sessions
         WHERE status IN ('active', 'resumed')",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, pid, boot_id)| {
            !crate::system::session_process_is_live(*pid, boot_id.as_deref(), current_boot_id)
        })
        .map(|(id, _, _)| id)
        .collect())
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InteractiveSession> {
    Ok(InteractiveSession {
        id: row.get(0)?,
        name: row.get(1)?,
        cli: row.get(2)?,
        working_dir: row.get(3)?,
        args: row.get(4)?,
        started_at: row.get(5)?,
        status: row.get(6)?,
        session_type: row.get(7)?,
        pid: row.get(8)?,
        boot_id: row.get(9)?,
        harness_conversation_id: row.get(10)?,
    })
}

/// The values needed to register (or re-register) an interactive session.
/// Bundled in a struct so the registration call stays under the argument
/// limit without a lint exception.
pub struct SessionRegistration<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub cli: &'a str,
    pub working_dir: &'a str,
    pub args: Option<&'a str>,
    pub pid: Option<i64>,
    pub session_type: &'a str,
    pub boot_id: Option<&'a str>,
    /// Harness conversation identifier, when the caller knows it.
    pub harness_conversation_id: Option<&'a str>,
}

/// Outcome of [`Database::register_interactive_session`]: either an earlier
/// row for the same harness conversation was reactivated, or a new row was
/// inserted under the supplied identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOutcome {
    Reactivated { id: String, name: String },
    Inserted,
}

#[allow(dead_code)]
pub struct TerminalSession {
    pub id: String,
    pub name: String,
    pub shell: String,
    pub working_dir: String,
    pub created_at: String,
}

impl Database {
    /// Insert a new interactive session as active.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_interactive_session(
        &self,
        id: &str,
        name: &str,
        cli: &str,
        working_dir: &str,
        args: Option<&str>,
        pid: Option<i64>,
        session_type: &str,
        boot_id: Option<&str>,
    ) -> Result<()> {
        // A sandboxed workdir registers under the original project path — a
        // sandbox never registers itself as a project (mapping locks the
        // connection, so it must happen before this function takes it).
        let working_dir = self.map_workdir_out_of_sandbox(working_dir)?;
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO interactive_sessions (id, name, cli, working_dir, args, started_at, status, session_type, pid, boot_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9)",
            params![id, name, cli, working_dir, args, Utc::now().to_rfc3339(), session_type, pid, boot_id],
        )?;
        Ok(())
    }

    /// Find the earlier session row for a harness conversation, if canopy has
    /// ever recorded it. Matched strictly on the harness's own conversation
    /// identifier — never on workdir, model or timing — so two sessions in the
    /// same directory stay distinct. Bridge sidecars are excluded (proxies,
    /// never resumable conversations) and any status matches: a conversation
    /// being resumed is usually `completed`, `error` or `resumed` by then.
    /// Most recent row wins if history ever holds more than one.
    pub fn find_session_by_harness_id(
        &self,
        harness_id: &str,
    ) -> Result<Option<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM interactive_sessions
             WHERE harness_conversation_id = ?1 AND session_type != 'bridge'
             ORDER BY started_at DESC LIMIT 1",
        ))?;
        let found = match stmt.query_row(params![harness_id], session_from_row) {
            Ok(session) => Some(session),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(found)
    }

    /// Fetch one interactive session row by its canopy id (`None` when no such
    /// row). Any status matches: a resume relaunches a finished session, and
    /// the caller — not this lookup — decides what the row's state means.
    pub fn get_interactive_session(&self, id: &str) -> Result<Option<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM interactive_sessions WHERE id = ?1",
        ))?;
        let found = match stmt.query_row(params![id], session_from_row) {
            Ok(session) => Some(session),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(found)
    }

    /// Rename one interactive session row. Only a resume that had to take a
    /// fresh name (its source name was held by another live session) renames:
    /// every other path keeps the name so hooks and scheduled sends addressed
    /// to it keep resolving.
    pub fn rename_interactive_session(&self, id: &str, new_name: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET name = ?1 WHERE id = ?2",
            params![new_name, id],
        )?;
        Ok(())
    }

    /// Reactivate an existing session row in place, for a harness conversation
    /// that came back: the row keeps its id, name and `session_type`, and only
    /// the liveness fields move (pid, workdir, args, boot id, and `started_at`,
    /// which doubles as last-seen). Never writes the name, so a hook or
    /// scheduled send addressed to it keeps resolving.
    pub fn reactivate_interactive_session(
        &self,
        id: &str,
        pid: Option<i64>,
        working_dir: &str,
        args: Option<&str>,
        boot_id: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions
                SET status = 'active', pid = ?1, working_dir = ?2, args = ?3, boot_id = ?4,
                    started_at = ?5
              WHERE id = ?6",
            params![pid, working_dir, args, boot_id, Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

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

    /// Register a freshly launched interactive session, reusing the earlier row
    /// when the registration carries an id canopy already knows (a resume
    /// relaunched under its source session's id) or when the harness
    /// conversation identifier matches one canopy has seen: that row is
    /// reactivated and its id and name are returned, so the caller adopts
    /// them instead of a second row appearing for the same conversation. With
    /// neither, a new row is inserted under the supplied identity.
    pub fn register_interactive_session(
        &self,
        registration: &SessionRegistration<'_>,
    ) -> Result<RegistrationOutcome> {
        // A sandboxed workdir resolves to the original project path before
        // any row is written (mapping locks the connection, so it must run
        // before this function locks it below).
        let working_dir = self.map_workdir_out_of_sandbox(registration.working_dir)?;
        // A resume relaunches the source session's own id: reactivate that
        // row in place — status, pid, boot id and args move, the id never
        // does. The name only moves when the spawn had to take a fresh one
        // (the source name was held by another live session); a carried
        // harness identifier is recorded but never overwrites the recorded
        // one. No second row is ever added for the relaunched session.
        if let Some(existing) = self.get_interactive_session(registration.id)? {
            self.reactivate_interactive_session(
                &existing.id,
                registration.pid,
                &working_dir,
                registration.args,
                registration.boot_id,
            )?;
            if existing.name != registration.name {
                self.rename_interactive_session(&existing.id, registration.name)?;
            }
            if let Some(harness_id) = registration.harness_conversation_id {
                self.set_session_harness_conversation_id(&existing.id, harness_id)?;
            }
            return Ok(RegistrationOutcome::Reactivated {
                id: existing.id,
                name: registration.name.to_string(),
            });
        }
        if let Some(harness_id) = registration.harness_conversation_id {
            if let Some(existing) = self.find_session_by_harness_id(harness_id)? {
                self.reactivate_interactive_session(
                    &existing.id,
                    registration.pid,
                    &working_dir,
                    registration.args,
                    registration.boot_id,
                )?;
                return Ok(RegistrationOutcome::Reactivated {
                    id: existing.id,
                    name: existing.name,
                });
            }
        }
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO interactive_sessions (id, name, cli, working_dir, args, started_at, status, session_type, pid, boot_id, harness_conversation_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'active', ?7, ?8, ?9, ?10)",
            params![
                registration.id,
                registration.name,
                registration.cli,
                working_dir,
                registration.args,
                Utc::now().to_rfc3339(),
                registration.session_type,
                registration.pid,
                registration.boot_id,
                registration.harness_conversation_id,
            ],
        )?;
        Ok(RegistrationOutcome::Inserted)
    }

    /// Get launch args for an interactive session by id.
    pub fn get_interactive_session_args(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare("SELECT args FROM interactive_sessions WHERE id = ?1")?;
        let result = stmt.query_row(params![session_id], |row| row.get(0)).ok();
        Ok(result)
    }

    /// Get the status of an interactive session by id (`None` if no such row).
    /// Test-only inspection helper (B32) for asserting status transitions.
    #[cfg(test)]
    pub fn get_interactive_session_status(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare("SELECT status FROM interactive_sessions WHERE id = ?1")?;
        let result = stmt.query_row(params![session_id], |row| row.get(0)).ok();
        Ok(result)
    }

    /// Get the working directory for an interactive session by id.
    pub fn get_session_workdir(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt =
            conn.prepare("SELECT working_dir FROM interactive_sessions WHERE id = ?1")?;
        let result = stmt.query_row(params![session_id], |row| row.get(0)).ok();
        Ok(result)
    }

    /// Mark a session as exited with a status and optional exit code.
    pub fn finish_interactive_session(&self, id: &str, exit_code: i32) -> Result<()> {
        let status = if exit_code == 0 { "completed" } else { "error" };
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET exited_at = ?1, exit_code = ?2, status = ?3 WHERE id = ?4",
            params![Utc::now().to_rfc3339(), exit_code, status, id],
        )?;
        Ok(())
    }

    /// Get all sessions with status = 'active', excluding bridge sidecars.
    ///
    /// Bridge sessions (`canopy bridge`, see `daemon::bridge`) are proxy
    /// processes for an MCP harness, not resumable interactive CLIs — they
    /// must never be handed to `auto_resume_sessions`, which would try to
    /// relaunch `canopy bridge` as if it were a chat session.
    pub fn get_active_sessions(&self) -> Result<Vec<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}
             FROM interactive_sessions WHERE status = 'active' AND session_type != 'bridge'
             ORDER BY started_at DESC",
        ))?;
        let rows = stmt
            .query_map([], session_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// List sessions an interactive hook can target (CM16): rows with
    /// `status = 'active'`, excluding bridge sidecars. This is byte-for-byte
    /// the acceptance set `execute_interactive_hook` enforces (via
    /// `get_active_sessions`), so every id returned here is hook-acceptable.
    /// Read-only; touches no PTY and wakes no session. Most-recent-first,
    /// capped at `limit` rows.
    ///
    /// NOTE: `status = 'active'` does NOT mean a TUI is attached right now —
    /// attachment lives in TUI-process memory, not in this table, and a row
    /// stays `active` with no TUI running. The hook contract already promises
    /// an enqueued send is delivered when a TUI later starts, so such rows
    /// are legitimate targets and are listed with `hook_target: yes` by the
    /// `session_list` surface.
    pub fn list_hookable_sessions(&self, limit: i64) -> Result<Vec<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}
             FROM interactive_sessions WHERE status = 'active' AND session_type != 'bridge'
             ORDER BY started_at DESC LIMIT ?1",
        ))?;
        let rows = stmt
            .query_map(params![limit], session_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Fetch one hook-addressable session by id (CM16): any status, but never
    /// bridge sidecars (MCP harness proxies, never hook targets). Returns
    /// `None` for unknown ids AND for bridge ids, letting the caller report a
    /// clear not-found. Read-only; touches no PTY and wakes no session.
    pub fn get_hookable_session(&self, id: &str) -> Result<Option<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}
             FROM interactive_sessions WHERE id = ?1 AND session_type != 'bridge'",
        ))?;
        let result = match stmt.query_row(params![id], session_from_row) {
            Ok(session) => Some(session),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(result)
    }

    /// Get all sessions with status = 'orphaned', excluding bridge sidecars.
    ///
    /// Populates the TUI's orphaned-sessions dialog, which lets the user
    /// manually revive or dismiss a session that couldn't be (or wasn't)
    /// auto-resumed at startup.
    pub fn get_orphaned_sessions(&self) -> Result<Vec<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}
             FROM interactive_sessions WHERE status = 'orphaned' AND session_type != 'bridge'
             ORDER BY started_at DESC",
        ))?;
        let rows = stmt
            .query_map([], session_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get sessions eligible for the canopy-native resume picker (C25):
    /// finished (`completed` or `error`) interactive sessions, excluding
    /// bridge sidecars. `active` rows are excluded because they're already
    /// running and visible in the sidebar; `resumed` rows are excluded
    /// because they were already superseded by a replacement session that
    /// appears in its own right once it finishes. Most-recent-first, capped
    /// at `limit` rows. This is a plain fetch: the Mode step narrows it to
    /// the form's harness and directory, keeping this order as-is.
    pub fn get_resumable_sessions(&self, limit: usize) -> Result<Vec<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}
             FROM interactive_sessions
             WHERE status IN ('completed', 'error') AND session_type != 'bridge'
             ORDER BY started_at DESC
             LIMIT ?1",
        ))?;
        let rows = stmt
            .query_map(params![limit as i64], session_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Get active sessions of a specific `session_type` (e.g. "bridge").
    ///
    /// Used at startup to reconcile bridge sidecars whose owning process
    /// died without calling `finish_standalone_session` — those rows would
    /// otherwise stay `active` forever.
    pub fn get_active_sessions_by_type(
        &self,
        session_type: &str,
    ) -> Result<Vec<InteractiveSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SESSION_COLUMNS}
             FROM interactive_sessions WHERE status = 'active' AND session_type = ?1
             ORDER BY started_at DESC",
        ))?;
        let rows = stmt
            .query_map(params![session_type], session_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark a single session 'orphaned'. Only transitions rows that are
    /// still 'active', so it's a no-op if the session was already resumed
    /// or handled elsewhere.
    ///
    /// Retained as a test-only fixture (B32): the product no longer orphans
    /// interactive sessions — an unrecoverable session is closed via
    /// [`Self::mark_session_closed`] instead — but tests still need a way to
    /// synthesize a historic `orphaned` row to exercise the startup sweep in
    /// [`Self::close_orphaned_interactive_sessions`].
    #[cfg(test)]
    pub fn mark_session_orphaned(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET status = 'orphaned' WHERE id = ?1 AND status = 'active'",
            params![id],
        )?;
        Ok(())
    }

    /// Mark a single active session closed (`completed`) — the terminal state
    /// for a session auto-resume could not recover. There is no session-admin
    /// surface to revive an unrecoverable session, so rather than leaving it
    /// as a red, un-enterable `orphaned` row it simply becomes a finished
    /// session and disappears from the sidebar. The row is kept for history;
    /// only its status changes. Guarded to `active` rows like
    /// `mark_session_resumed`, so it's a no-op if the session was already
    /// resumed or finished elsewhere.
    pub fn mark_session_closed(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET status = 'completed' WHERE id = ?1 AND status = 'active'",
            params![id],
        )?;
        Ok(())
    }

    /// Startup sweep: move any interactive session still in the retired
    /// `orphaned` status to `completed`, so historic red orphan rows written
    /// before orphaning was removed disappear from the sidebar. Returns the
    /// number of rows swept. Rows are kept for history — only the status
    /// changes.
    pub fn close_orphaned_interactive_sessions(&self) -> Result<usize> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let rows = conn.execute(
            "UPDATE interactive_sessions SET status = 'completed' WHERE status = 'orphaned'",
            [],
        )?;
        Ok(rows)
    }

    /// Reconcile stale interactive session rows: an `active`/`resumed` row
    /// whose process is already gone is closed with the same values
    /// [`Self::finish_interactive_session`] writes for a failed exit
    /// (`status = 'error'`, `exited_at` set, `exit_code = 1`) — no new
    /// status. Liveness is [`crate::system::session_process_is_live`]
    /// (same boot id + PID alive, with a PID-only fail-closed fallback when
    /// either boot id is unknown), the same rule `busy_reasons` counts by,
    /// so a reconciled row stops blocking maintenance and stops listing as
    /// live anywhere. Covers every `session_type`, including bridge
    /// sidecars. Kills nothing: only rows whose process is already gone are
    /// touched. Returns the number of rows closed.
    pub fn reconcile_stale_interactive_sessions(&self) -> Result<usize> {
        let current_boot_id = crate::system::boot_id();
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let stale_ids = stale_interactive_session_ids(&conn, current_boot_id.as_deref())?;
        if stale_ids.is_empty() {
            return Ok(0);
        }
        let now = Utc::now().to_rfc3339();
        let mut closed = 0;
        for id in &stale_ids {
            closed += conn.execute(
                "UPDATE interactive_sessions
                 SET status = 'error', exited_at = ?1, exit_code = 1
                 WHERE id = ?2 AND status IN ('active', 'resumed')",
                params![now, id],
            )?;
        }
        Ok(closed)
    }

    /// Mark a single session 'resumed', once its replacement process has
    /// been launched. See `mark_session_orphaned` for why this is per-row
    /// rather than a mass update.
    ///
    /// Retained as a test-only fixture: a resumed harness conversation now
    /// reactivates its own row (see [`Self::reactivate_interactive_session`])
    /// instead of superseding it, but tests still need a way to synthesize a
    /// historic `resumed` row.
    #[cfg(test)]
    pub fn mark_session_resumed(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE interactive_sessions SET status = 'resumed' WHERE id = ?1 AND status = 'active'",
            params![id],
        )?;
        Ok(())
    }

    /// Insert a terminal session record.
    pub fn insert_terminal_session(
        &self,
        id: &str,
        name: &str,
        shell: &str,
        working_dir: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT OR REPLACE INTO terminal_sessions (id, name, shell, working_dir, created_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, 'idle')",
            params![id, name, shell, working_dir, Utc::now().to_rfc3339()],
        )?;
        Ok(())
    }

    /// Mark a terminal session as finished.
    pub fn finish_terminal_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE terminal_sessions SET status = 'finished', last_active = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Update the working directory of an active terminal session.
    pub fn update_terminal_session_working_dir(&self, id: &str, working_dir: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE terminal_sessions SET working_dir = ?1, last_active = ?2 WHERE id = ?3",
            params![working_dir, Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    /// Get all terminal sessions that are still active (idle = was active when canopy last ran).
    pub fn get_active_terminal_sessions(&self) -> Result<Vec<TerminalSession>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, shell, working_dir, created_at
             FROM terminal_sessions WHERE status = 'idle' ORDER BY created_at DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(TerminalSession {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    shell: row.get(2)?,
                    working_dir: row.get(3)?,
                    created_at: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark all active terminal sessions as orphaned (called on startup cleanup).
    pub fn mark_orphaned_terminal_sessions(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE terminal_sessions SET status = 'orphaned' WHERE status = 'idle'",
            [],
        )?;
        Ok(())
    }

    /// Test-only: age a terminal session's `last_active`, the instant the
    /// shelf's 7-day retention clock starts from, so retention tests do not
    /// have to wait real days (same pattern as `set_shelf_created_at`).
    #[cfg(test)]
    pub fn set_terminal_last_active(&self, id: &str, at: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE terminal_sessions SET last_active = ?1 WHERE id = ?2",
            params![at, id],
        )?;
        Ok(())
    }

    /// Get the session_type for an interactive session by id.
    pub fn get_session_type(&self, session_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt =
            conn.prepare("SELECT session_type FROM interactive_sessions WHERE id = ?1")?;
        let result = stmt.query_row(params![session_id], |row| row.get(0)).ok();
        Ok(result)
    }

    /// Remove an interactive session record entirely (used for nursery cleanup).
    pub fn remove_interactive_session(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "DELETE FROM interactive_sessions WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    pub fn count_interactive_sessions(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM interactive_sessions", [], |row| {
                row.get(0)
            })?;
        Ok(count)
    }

    pub fn count_terminal_sessions(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM terminal_sessions", [], |row| {
            row.get(0)
        })?;
        Ok(count)
    }

    pub fn count_background_agents(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM agents", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn count_runs(&self) -> Result<i64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM runs", [], |row| row.get(0))?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    #[test]
    fn insert_and_get_interactive_session() {
        let db = test_db();
        let session_id = "test-session-123";
        let name = "test-session";
        let cli = "bash";
        let workdir = "/tmp/test";
        let args = Some("arg1 arg2");
        let pid = Some(12345i64);
        let session_type = "interactive";
        let boot_id = Some("boot-123");

        db.insert_interactive_session(
            session_id,
            name,
            cli,
            workdir,
            args,
            pid,
            session_type,
            boot_id,
        )
        .unwrap();

        let retrieved_args = db.get_interactive_session_args(session_id).unwrap();
        assert!(retrieved_args.is_some());
        let retrieved_args = retrieved_args.unwrap();
        assert!(retrieved_args.contains("arg1"));
        assert!(retrieved_args.contains("arg2"));

        let workdir = db.get_session_workdir(session_id).unwrap();
        assert_eq!(workdir, Some("/tmp/test".to_string()));
    }

    #[test]
    fn get_interactive_session_args_not_found() {
        let db = test_db();
        let result = db.get_interactive_session_args("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_session_workdir_not_found() {
        let db = test_db();
        let result = db.get_session_workdir("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn finish_interactive_session() {
        let db = test_db();
        let session_id = "test-session-456";
        let name = "test-session";
        let cli = "bash";
        let workdir = "/tmp/test";
        let args = Some("arg1");
        let pid = Some(12345i64);
        let session_type = "interactive";
        let boot_id = Some("boot-123");

        db.insert_interactive_session(
            session_id,
            name,
            cli,
            workdir,
            args,
            pid,
            session_type,
            boot_id,
        )
        .unwrap();

        db.finish_interactive_session(session_id, 0).unwrap();

        let status = db.get_interactive_session_status(session_id).unwrap();
        assert_eq!(status, Some("completed".to_string()));
    }

    #[test]
    fn get_active_sessions_empty() {
        let db = test_db();
        let sessions = db.get_active_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn get_active_sessions_with_sessions() {
        let db = test_db();
        db.insert_interactive_session(
            "session1",
            "name1",
            "bash",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.insert_interactive_session(
            "session2",
            "name2",
            "bash",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let sessions = db.get_active_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn get_orphaned_sessions_empty() {
        let db = test_db();
        let sessions = db.get_orphaned_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    // ── list_hookable_sessions / get_hookable_session (CM16) ───────────

    #[test]
    fn list_hookable_sessions_returns_only_active_non_bridge() {
        let db = test_db();
        db.insert_interactive_session(
            "hook-live",
            "boletus",
            "opencode",
            "/tmp/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.insert_interactive_session(
            "hook-done",
            "done-name",
            "opencode",
            "/tmp/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("hook-done", 0).unwrap();
        db.insert_interactive_session(
            "hook-sidecar",
            "sidecar",
            "opencode",
            "/tmp/proj",
            None,
            None,
            "bridge",
            None,
        )
        .unwrap();

        let sessions = db.list_hookable_sessions(200).unwrap();
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "hook-live");
        assert_eq!(s.name, "boletus");
        assert_eq!(s.cli, "opencode");
        assert_eq!(s.working_dir, "/tmp/proj");
        assert_eq!(s.status, "active");
        // Same acceptance set the interactive hook enforces: the listed id
        // must be present in get_active_sessions.
        let active = db.get_active_sessions().unwrap();
        assert!(active.iter().any(|a| a.id == s.id));
    }

    #[test]
    fn get_hookable_session_unknown_returns_none() {
        let db = test_db();
        db.insert_interactive_session(
            "hook-sidecar",
            "sidecar",
            "opencode",
            "/tmp",
            None,
            None,
            "bridge",
            None,
        )
        .unwrap();
        assert!(db.get_hookable_session("missing-id").unwrap().is_none());
        assert!(db.get_hookable_session("hook-sidecar").unwrap().is_none());
    }

    #[test]
    fn list_hookable_sessions_respects_limit() {
        let db = test_db();
        for i in 0..3 {
            db.insert_interactive_session(
                &format!("s{i}"),
                &format!("n{i}"),
                "opencode",
                "/tmp",
                None,
                None,
                "interactive",
                None,
            )
            .unwrap();
        }
        assert_eq!(db.list_hookable_sessions(2).unwrap().len(), 2);
        assert_eq!(db.list_hookable_sessions(200).unwrap().len(), 3);
    }

    // ── get_resumable_sessions (C25 regression) ────────────────────

    /// The reported bug: several finished sessions on record, and zero rows
    /// in the retired `orphaned` status (nothing writes it any more), must
    /// still yield resumable candidates.
    #[test]
    fn get_resumable_sessions_finds_completed_sessions_with_no_orphaned_rows() {
        let db = test_db();
        for i in 0..3 {
            let id = format!("session-{i}");
            db.insert_interactive_session(
                &id,
                &id,
                "claude",
                "/tmp/project",
                None,
                None,
                "interactive",
                None,
            )
            .unwrap();
            db.finish_interactive_session(&id, 0).unwrap();
        }

        assert!(db.get_orphaned_sessions().unwrap().is_empty());

        let resumable = db.get_resumable_sessions(20).unwrap();
        assert!(
            !resumable.is_empty(),
            "finished sessions must be resumable even though nothing is 'orphaned'"
        );
        assert_eq!(resumable.len(), 3);
    }

    #[test]
    fn get_resumable_sessions_includes_completed_and_error() {
        let db = test_db();
        db.insert_interactive_session(
            "ok",
            "ok",
            "claude",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("ok", 0).unwrap();
        db.insert_interactive_session(
            "bad",
            "bad",
            "claude",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("bad", 1).unwrap();

        let resumable = db.get_resumable_sessions(20).unwrap();
        assert_eq!(resumable.len(), 2);
    }

    #[test]
    fn get_resumable_sessions_excludes_active() {
        let db = test_db();
        db.insert_interactive_session(
            "still-active",
            "still-active",
            "claude",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let resumable = db.get_resumable_sessions(20).unwrap();
        assert!(resumable.is_empty());
    }

    #[test]
    fn get_resumable_sessions_excludes_resumed() {
        let db = test_db();
        db.insert_interactive_session(
            "superseded",
            "superseded",
            "claude",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.mark_session_resumed("superseded").unwrap();

        let resumable = db.get_resumable_sessions(20).unwrap();
        assert!(resumable.is_empty());
    }

    #[test]
    fn get_resumable_sessions_excludes_bridge_sidecars() {
        let db = test_db();
        db.insert_interactive_session(
            "sidecar", "sidecar", "claude", "/tmp", None, None, "bridge", None,
        )
        .unwrap();
        db.finish_interactive_session("sidecar", 0).unwrap();

        let resumable = db.get_resumable_sessions(20).unwrap();
        assert!(resumable.is_empty());
    }

    #[test]
    fn get_resumable_sessions_respects_limit() {
        let db = test_db();
        for i in 0..5 {
            let id = format!("session-{i}");
            db.insert_interactive_session(
                &id,
                &id,
                "claude",
                "/tmp",
                None,
                None,
                "interactive",
                None,
            )
            .unwrap();
            db.finish_interactive_session(&id, 0).unwrap();
        }

        let resumable = db.get_resumable_sessions(2).unwrap();
        assert_eq!(resumable.len(), 2);
    }

    #[test]
    fn count_terminal_sessions_empty() {
        let db = test_db();
        let count = db.count_terminal_sessions().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn count_background_agents_empty() {
        let db = test_db();
        let count = db.count_background_agents().unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn count_runs_empty() {
        let db = test_db();
        let count = db.count_runs().unwrap();
        assert_eq!(count, 0);
    }

    // ── harness conversation identity across a resume ──────────────────────

    /// Register a launch the way `App::launch_interactive` does: the spawned
    /// process has its own id/name, and reports the harness conversation id
    /// when it is a resume.
    fn register(
        db: &Database,
        id: &str,
        name: &str,
        working_dir: &str,
        harness_conversation_id: Option<&str>,
    ) -> RegistrationOutcome {
        db.register_interactive_session(&SessionRegistration {
            id,
            name,
            cli: "claude",
            working_dir,
            args: Some("--continue"),
            pid: Some(4242),
            session_type: "interactive",
            boot_id: Some("boot-new"),
            harness_conversation_id,
        })
        .unwrap()
    }

    /// A finished session whose harness conversation was recorded before it
    /// ended — the shape a TUI restart finds when the user resumes.
    fn finished_known_session(db: &Database, harness_id: &str) {
        db.insert_interactive_session(
            "sess-orch",
            "marasmius",
            "claude",
            "/tmp/proj",
            Some("--continue"),
            Some(111),
            "interactive",
            Some("boot-old"),
        )
        .unwrap();
        db.set_session_harness_conversation_id("sess-orch", harness_id)
            .unwrap();
        db.finish_interactive_session("sess-orch", 0).unwrap();
    }

    #[test]
    fn resume_of_known_harness_conversation_keeps_id_and_name() {
        let db = test_db();
        finished_known_session(&db, "conv-abc");

        let outcome = register(&db, "fresh-uuid", "populus", "/tmp/proj", Some("conv-abc"));

        assert_eq!(
            outcome,
            RegistrationOutcome::Reactivated {
                id: "sess-orch".to_string(),
                name: "marasmius".to_string(),
            }
        );
        // One live row, under the earlier identity — never two rows (FR5).
        let live = db.list_hookable_sessions(200).unwrap();
        assert_eq!(live.len(), 1, "a resumed conversation must not add a row");
        assert_eq!(live[0].id, "sess-orch");
        assert_eq!(live[0].name, "marasmius");
        assert_eq!(live[0].status, "active");
        // Only liveness fields moved.
        assert_eq!(live[0].pid, Some(4242));
        assert_eq!(live[0].boot_id.as_deref(), Some("boot-new"));
        assert_eq!(db.count_interactive_sessions().unwrap(), 1);
        assert!(db.get_hookable_session("fresh-uuid").unwrap().is_none());
        assert_eq!(
            db.find_session_by_harness_id("conv-abc")
                .unwrap()
                .unwrap()
                .id,
            "sess-orch",
            "the harness id survives the resume"
        );
    }

    #[test]
    fn hook_targeting_the_name_of_a_resumed_session_resolves_to_it() {
        use crate::db::session_target::SessionTarget;

        let db = test_db();
        finished_known_session(&db, "conv-abc");
        assert_eq!(
            db.resolve_target_session_name("marasmius").unwrap(),
            SessionTarget::Missing,
            "the finished session is not a hook target yet"
        );

        register(&db, "fresh-uuid", "populus", "/tmp/proj", Some("conv-abc"));

        // A hook addressed to the name still finds the resumed session...
        assert_eq!(
            db.resolve_target_session_name("marasmius").unwrap(),
            SessionTarget::Found {
                id: "sess-orch".to_string(),
                name: Some("marasmius".to_string()),
            }
        );
        // ...and so does one addressed to its id.
        assert_eq!(
            db.resolve_target_session_id("sess-orch").unwrap(),
            SessionTarget::Found {
                id: "sess-orch".to_string(),
                name: None,
            }
        );
        // The name that was never minted resolves to nothing.
        assert_eq!(
            db.resolve_target_session_name("populus").unwrap(),
            SessionTarget::Missing
        );
    }

    #[test]
    fn unknown_harness_conversation_gets_a_new_id_and_name() {
        let db = test_db();
        finished_known_session(&db, "conv-abc");

        let outcome = register(&db, "sess-new", "populus", "/tmp/proj", Some("conv-new"));

        assert_eq!(outcome, RegistrationOutcome::Inserted);
        // Two conversations on record; only the new one is live.
        assert_eq!(db.count_interactive_sessions().unwrap(), 2);
        let live = db.list_hookable_sessions(200).unwrap();
        assert_eq!(live.len(), 1);
        assert!(live
            .iter()
            .any(|s| s.id == "sess-new" && s.name == "populus"));
        // Both conversations are on record under their own harness ids.
        assert_eq!(
            db.find_session_by_harness_id("conv-new")
                .unwrap()
                .unwrap()
                .id,
            "sess-new"
        );
        assert_eq!(
            db.find_session_by_harness_id("conv-abc")
                .unwrap()
                .unwrap()
                .name,
            "marasmius"
        );
    }

    #[test]
    fn registration_without_a_harness_id_keeps_todays_behaviour() {
        let db = test_db();
        finished_known_session(&db, "conv-abc");

        let outcome = register(&db, "sess-2", "populus", "/tmp/proj", None);

        assert_eq!(outcome, RegistrationOutcome::Inserted);
        assert_eq!(
            db.count_interactive_sessions().unwrap(),
            2,
            "two sessions in the same workdir are distinct: identity is never \
             inferred from workdir, model or timing"
        );
        let live = db.list_hookable_sessions(200).unwrap();
        assert_eq!(live.len(), 1);
        assert!(live.iter().any(|s| s.id == "sess-2" && s.name == "populus"));
        assert_eq!(
            db.get_hookable_session("sess-orch").unwrap().unwrap().name,
            "marasmius",
            "the earlier session keeps its own row and name"
        );
    }

    #[test]
    fn recorded_harness_id_is_never_overwritten() {
        let db = test_db();
        db.insert_interactive_session(
            "sess-1",
            "marasmius",
            "claude",
            "/tmp/proj",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        db.set_session_harness_conversation_id("sess-1", "conv-first")
            .unwrap();
        db.set_session_harness_conversation_id("sess-1", "conv-second")
            .unwrap();

        assert_eq!(
            db.find_session_by_harness_id("conv-first")
                .unwrap()
                .unwrap()
                .id,
            "sess-1"
        );
        assert!(db
            .find_session_by_harness_id("conv-second")
            .unwrap()
            .is_none());
    }

    #[test]
    fn bridge_sidecars_are_never_matched_by_harness_id() {
        let db = test_db();
        db.insert_interactive_session(
            "sidecar", "sidecar", "claude", "/tmp", None, None, "bridge", None,
        )
        .unwrap();
        db.set_session_harness_conversation_id("sidecar", "conv-abc")
            .unwrap();

        assert!(
            db.find_session_by_harness_id("conv-abc").unwrap().is_none(),
            "a bridge sidecar is a proxy, never a resumable conversation"
        );
        let outcome = register(&db, "sess-new", "populus", "/tmp", Some("conv-abc"));
        assert_eq!(outcome, RegistrationOutcome::Inserted);
    }

    // ── resume by explicit session id ─────────────────────────────────

    #[test]
    fn resume_of_known_id_reactivates_the_same_row() {
        let db = test_db();
        finished_known_session(&db, "conv-abc");

        // The relaunch carries no harness identifier at all: the id alone is
        // the identity (FR1), yet the source row reactivates in place (FR2)
        // instead of a second row appearing.
        let outcome = register(&db, "sess-orch", "marasmius", "/tmp/proj", None);

        assert_eq!(
            outcome,
            RegistrationOutcome::Reactivated {
                id: "sess-orch".to_string(),
                name: "marasmius".to_string(),
            }
        );
        // No second row: the source row is reactivated in place (FR2).
        assert_eq!(db.count_interactive_sessions().unwrap(), 1);
        let row = db.get_interactive_session("sess-orch").unwrap().unwrap();
        assert_eq!(row.id, "sess-orch");
        assert_eq!(row.name, "marasmius");
        assert_eq!(row.status, "active");
        assert_eq!(row.pid, Some(4242));
        assert_eq!(row.boot_id.as_deref(), Some("boot-new"));
        assert_eq!(row.args.as_deref(), Some("--continue"));
        assert_eq!(
            row.harness_conversation_id.as_deref(),
            Some("conv-abc"),
            "the recorded harness id survives the resume"
        );
        assert!(db.get_interactive_session("no-such-id").unwrap().is_none());
    }

    #[test]
    fn resume_of_known_id_with_a_fresh_name_renames_and_keeps_the_id() {
        let db = test_db();
        finished_known_session(&db, "conv-abc");

        // FR5: the source name was held by another live session, so the
        // spawn kept the id under a fresh name.
        let outcome = register(&db, "sess-orch", "pholiota", "/tmp/proj", Some("conv-abc"));

        assert_eq!(
            outcome,
            RegistrationOutcome::Reactivated {
                id: "sess-orch".to_string(),
                name: "pholiota".to_string(),
            }
        );
        assert_eq!(db.count_interactive_sessions().unwrap(), 1);
        let row = db.get_interactive_session("sess-orch").unwrap().unwrap();
        assert_eq!(row.id, "sess-orch");
        assert_eq!(row.name, "pholiota");
        assert_eq!(row.status, "active");
    }

    #[test]
    fn resume_of_known_id_records_a_carried_harness_id_without_overwriting() {
        let db = test_db();
        db.insert_interactive_session(
            "sess-bare",
            "populus",
            "claude",
            "/tmp/proj",
            Some("--continue"),
            Some(111),
            "interactive",
            Some("boot-old"),
        )
        .unwrap();
        db.finish_interactive_session("sess-bare", 0).unwrap();

        // First resume carries the conversation id: it is recorded.
        let outcome = register(&db, "sess-bare", "populus", "/tmp/proj", Some("conv-first"));
        assert_eq!(
            outcome,
            RegistrationOutcome::Reactivated {
                id: "sess-bare".to_string(),
                name: "populus".to_string(),
            }
        );
        assert_eq!(
            db.find_session_by_harness_id("conv-first")
                .unwrap()
                .unwrap()
                .id,
            "sess-bare"
        );

        // A later resume carrying a different id must not overwrite it.
        register(
            &db,
            "sess-bare",
            "populus",
            "/tmp/proj",
            Some("conv-second"),
        );
        assert_eq!(
            db.get_interactive_session("sess-bare")
                .unwrap()
                .unwrap()
                .harness_conversation_id
                .as_deref(),
            Some("conv-first")
        );
        assert!(db
            .find_session_by_harness_id("conv-second")
            .unwrap()
            .is_none());
    }
}
