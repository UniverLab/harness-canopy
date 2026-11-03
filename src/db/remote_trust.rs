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
                all_sessions INTEGER NOT NULL DEFAULT 0,
                upload INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS remote_trust_grants (
                login TEXT NOT NULL REFERENCES remote_trust_users(login) ON DELETE CASCADE,
                session_name TEXT NOT NULL,
                PRIMARY KEY (login, session_name)
            );
            CREATE TABLE IF NOT EXISTS remote_invites (
                code_hash TEXT PRIMARY KEY,
                login TEXT NOT NULL,
                uid INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                used INTEGER NOT NULL DEFAULT 0
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
    /// Whether the profile may upload files into its granted sessions.
    pub upload: bool,
}

/// Canonical login form: trimmed and lowercased (GitHub logins are
/// case-insensitive, so `OctoCat` and `octocat` are one profile).
fn normalize(login: &str) -> String {
    login.trim().to_lowercase()
}

/// One guest invite: the SHA256 hex of the code (the code itself is never
/// stored), the login it was issued to, that login's GitHub uid, and when it
/// stops opening the guest door.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInvite {
    /// SHA256 hex of the canonical `CNR-…` code.
    pub code_hash: String,
    /// Lowercased GitHub login the invite was issued to.
    pub login: String,
    /// That login's GitHub uid, resolved at issue time.
    pub uid: u64,
    /// Unix seconds when the invite stops opening the door.
    pub expires_at: i64,
    /// Whether a guest offer already redeemed this invite (single-use).
    pub used: bool,
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
        let flags: Option<(i64, i64)> = conn
            .query_row(
                "SELECT all_sessions, upload FROM remote_trust_users WHERE login = ?1",
                [&login],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((all, upload)) = flags else {
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
            upload: upload != 0,
        }))
    }

    /// Every trusted profile, ascending by login.
    pub fn list_trust(&self) -> Result<Vec<TrustedProfile>> {
        let logins: Vec<(String, i64, i64)> = {
            let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
            let mut stmt = conn.prepare(
                "SELECT login, all_sessions, upload FROM remote_trust_users ORDER BY login ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                let login: String = row.get(0)?;
                let all: i64 = row.get(1)?;
                let upload: i64 = row.get(2)?;
                Ok((login, all, upload))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut profiles = Vec::with_capacity(logins.len());
        for (login, all, upload) in logins {
            let sessions = self.grant_names(&login)?;
            profiles.push(TrustedProfile {
                login,
                all_sessions: all != 0,
                sessions,
                upload: upload != 0,
            });
        }
        Ok(profiles)
    }

    /// Set one login's upload grant (default deny). Both [`Self::upsert_trust`]
    /// and [`Self::set_grants`] leave it untouched, so only this call opens
    /// the door.
    pub fn set_upload(&self, login: &str, upload: bool) -> Result<()> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "UPDATE remote_trust_users SET upload = ?1 WHERE login = ?2",
            params![i64::from(upload), normalize(login)],
        )?;
        Ok(())
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

    /// Store one invite by its code hash. The cleartext code never reaches
    /// this table — the caller hashes it first and prints it once.
    pub fn insert_invite(
        &self,
        code_hash: &str,
        login: &str,
        uid: u64,
        expires_at: i64,
    ) -> Result<()> {
        let uid = i64::try_from(uid).map_err(|_| anyhow::anyhow!("invite uid out of range"))?;
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        conn.execute(
            "INSERT INTO remote_invites (code_hash, login, uid, expires_at, used)
             VALUES (?1, ?2, ?3, ?4, 0)",
            params![code_hash, normalize(login), uid, expires_at],
        )?;
        Ok(())
    }

    /// Every invite, ascending by login then hash (deterministic, so the
    /// `guest_allow` publish and its fingerprint are stable).
    pub fn list_invites(&self) -> Result<Vec<RemoteInvite>> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT code_hash, login, uid, expires_at, used FROM remote_invites
             ORDER BY login ASC, code_hash ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            let uid: i64 = row.get(2)?;
            let used: i64 = row.get(4)?;
            Ok(RemoteInvite {
                code_hash: row.get(0)?,
                login: row.get(1)?,
                uid: u64::try_from(uid).unwrap_or(0),
                expires_at: row.get(3)?,
                used: used != 0,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Revoke one invite by its code hash, in any state. Returns whether a
    /// row was removed.
    pub fn revoke_invite(&self, code_hash: &str) -> Result<bool> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let removed = conn.execute(
            "DELETE FROM remote_invites WHERE code_hash = ?1",
            [code_hash],
        )?;
        Ok(removed > 0)
    }

    /// Revoke every live (unused, unexpired) invite for one login. Returns
    /// how many rows were removed.
    pub fn revoke_invites_for_login(&self, login: &str, now: i64) -> Result<u64> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let removed = conn.execute(
            "DELETE FROM remote_invites WHERE login = ?1 AND used = 0 AND expires_at > ?2",
            params![normalize(login), now],
        )?;
        Ok(removed as u64)
    }

    /// Burn every live invite for one uid: the first accepted guest offer
    /// redeems the code, so sharing it afterwards opens nothing. Returns how
    /// many rows were burned.
    pub fn consume_invites_for_uid(&self, uid: u64, now: i64) -> Result<u64> {
        let uid = i64::try_from(uid).map_err(|_| anyhow::anyhow!("invite uid out of range"))?;
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let burned = conn.execute(
            "UPDATE remote_invites SET used = 1 WHERE uid = ?1 AND used = 0 AND expires_at > ?2",
            params![uid, now],
        )?;
        Ok(burned as u64)
    }

    /// Create the trust tables on databases that predate them. Guarded
    /// `CREATE TABLE IF NOT EXISTS` — a no-op on a fresh database (already
    /// in `BASE_SCHEMA_BATCHES`) and on an already-migrated one.
    pub(super) fn migrate_remote_trust(conn: &rusqlite::Connection) -> Result<()> {
        conn.execute_batch(super::remote_trust::REMOTE_TRUST_SCHEMA)
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        // The `upload` grant postdates the table: every existing profile
        // stays deny (the column default) until explicitly opened.
        let has_upload: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('remote_trust_users') WHERE name = 'upload'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_upload {
            conn.execute(
                "ALTER TABLE remote_trust_users ADD COLUMN upload INTEGER NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}
