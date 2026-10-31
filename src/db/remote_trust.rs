//! Trusted GitHub profiles for remote guests.
//!
//! The owner grants another GitHub user access to remote sessions: either
//! every session (`all_sessions`) or a named set of sessions. Grants are by
//! session *name* — the TUI has no id lookup — so two live sessions that
//! share a name are granted together (v1). Rows live hub-side next to the
//! device registry; the hub enforces them on every offer.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::db::Database;

/// The trust tables, shared by the base schema and the migration.
pub(super) const REMOTE_TRUST_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS remote_trust_users (
                login TEXT PRIMARY KEY,
                all_sessions INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS remote_trust_grants (
                login TEXT NOT NULL REFERENCES remote_trust_users(login) ON DELETE CASCADE,
                session_name TEXT NOT NULL,
                PRIMARY KEY (login, session_name)
            );";

/// One trusted profile with its granted session names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedProfile {
    /// Lowercased GitHub login.
    pub login: String,
    /// Whether the profile grants every session.
    pub all_sessions: bool,
    /// Granted session names, ascending. Empty when `all_sessions`.
    pub sessions: Vec<String>,
}

/// Canonical login form: trimmed and lowercased (GitHub logins are
/// case-insensitive, so `OctoCat` and `octocat` are one profile).
fn normalize(login: &str) -> String {
    login.trim().to_lowercase()
}

/// Canonical granted names: trimmed, empties dropped, order kept.
fn clean_names(sessions: &[String]) -> Vec<String> {
    sessions
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

impl Database {
    /// Trust a login, or update its `all_sessions` flag when already trusted.
    /// Grants are replaced separately through [`Self::set_grants`].
    pub fn upsert_trust(&self, login: &str, all_sessions: bool) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO remote_trust_users (login, all_sessions)
             VALUES (?1, ?2)
             ON CONFLICT(login) DO UPDATE SET all_sessions = excluded.all_sessions",
            params![normalize(login), i64::from(all_sessions)],
        )?;
        Ok(())
    }

    /// Forget a login and its grants. Returns whether a row was removed.
    pub fn remove_trust(&self, login: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let login = normalize(login);
        conn.execute("DELETE FROM remote_trust_grants WHERE login = ?1", [&login])?;
        let removed = conn.execute("DELETE FROM remote_trust_users WHERE login = ?1", [&login])?;
        Ok(removed > 0)
    }

    /// One trusted profile with its grants, or `None` when untrusted.
    pub fn get_trust(&self, login: &str) -> Result<Option<TrustedProfile>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let login = normalize(login);
        let all: Option<i64> = conn
            .query_row(
                "SELECT all_sessions FROM remote_trust_users WHERE login = ?1",
                [&login],
                |row| row.get(0),
            )
            .optional()?;
        let Some(all) = all else {
            return Ok(None);
        };
        let mut stmt = conn.prepare(
            "SELECT session_name FROM remote_trust_grants WHERE login = ?1 ORDER BY session_name ASC",
        )?;
        let sessions = stmt
            .query_map([&login], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(Some(TrustedProfile {
            login,
            all_sessions: all != 0,
            sessions,
        }))
    }

    /// Every trusted profile, ascending by login.
    pub fn list_trust(&self) -> Result<Vec<TrustedProfile>> {
        let logins: Vec<(String, i64)> = {
            let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut stmt = conn
                .prepare("SELECT login, all_sessions FROM remote_trust_users ORDER BY login ASC")?;
            let rows = stmt.query_map([], |row| {
                let login: String = row.get(0)?;
                let all: i64 = row.get(1)?;
                Ok((login, all))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut profiles = Vec::with_capacity(logins.len());
        for (login, all) in logins {
            let sessions = self.grant_names(&login)?;
            profiles.push(TrustedProfile {
                login,
                all_sessions: all != 0,
                sessions,
            });
        }
        Ok(profiles)
    }

    /// Replace a login's granted session names wholesale.
    pub fn set_grants(&self, login: &str, sessions: &[String]) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let login = normalize(login);
        conn.execute("DELETE FROM remote_trust_grants WHERE login = ?1", [&login])?;
        for name in clean_names(sessions) {
            conn.execute(
                "INSERT OR IGNORE INTO remote_trust_grants (login, session_name) VALUES (?1, ?2)",
                params![login, name],
            )?;
        }
        Ok(())
    }

    /// One login's granted names, ascending.
    fn grant_names(&self, login: &str) -> Result<Vec<String>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT session_name FROM remote_trust_grants WHERE login = ?1 ORDER BY session_name ASC",
        )?;
        let names = stmt
            .query_map([login], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names)
    }

    /// Create the trust tables on databases that predate them. Guarded
    /// `CREATE TABLE IF NOT EXISTS` — a no-op on a fresh database (already
    /// in `BASE_SCHEMA_BATCHES`) and on an already-migrated one.
    pub(super) fn migrate_remote_trust(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute_batch(super::remote_trust::REMOTE_TRUST_SCHEMA)
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        Ok(())
    }
}
