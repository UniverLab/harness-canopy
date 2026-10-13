//! CM48: the availability roster — declared platform/model availability.
//!
//! Only the MCP, CLI and TUI surfaces write this table: the engine never
//! infers availability from exit codes, stderr or quota messages. The engine
//! reads it at dispatch through
//! [`crate::domain::roster::effective_state`].

use anyhow::{anyhow, Result};
use rusqlite::params;

use crate::db::Database;

/// The declared state of one roster entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RosterState {
    Available,
    Unavailable,
}

impl RosterState {
    pub fn as_str(self) -> &'static str {
        match self {
            RosterState::Available => "available",
            RosterState::Unavailable => "unavailable",
        }
    }

    /// Parse a stored/requested state string. Anything that is not
    /// `available` or `unavailable` is rejected — the table's CHECK would
    /// reject it anyway, but a named error is friendlier at the tool edge.
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim() {
            "available" => Ok(RosterState::Available),
            "unavailable" => Ok(RosterState::Unavailable),
            other => Err(anyhow!(
                "state must be 'available' or 'unavailable', got '{other}'."
            )),
        }
    }
}

/// One `roster_entries` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RosterEntry {
    pub id: i64,
    pub platform: String,
    pub model_pattern: Option<String>,
    pub state: RosterState,
    pub reason: Option<String>,
    pub until: Option<String>,
    pub updated_at: String,
}

/// Parameters for [`Database::roster_upsert`]: bundled so the insert stays
/// within the argument-count lint without changing the row it writes.
#[derive(Debug, Clone, Copy)]
pub struct NewRosterEntry<'a> {
    pub platform: &'a str,
    pub model_pattern: Option<&'a str>,
    pub state: RosterState,
    pub reason: Option<&'a str>,
    pub until: Option<&'a str>,
}

fn read_roster_entry_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RosterEntry> {
    let state: String = row.get(3)?;
    Ok(RosterEntry {
        id: row.get(0)?,
        platform: row.get(1)?,
        model_pattern: row.get(2)?,
        state: if state == "unavailable" {
            RosterState::Unavailable
        } else {
            RosterState::Available
        },
        reason: row.get(4)?,
        until: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

impl Database {
    /// Insert or replace the one entry for `(platform, model_pattern)`.
    /// `until` is refreshed so an update that drops it stores NULL.
    pub fn roster_upsert(&self, entry: NewRosterEntry<'_>) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO roster_entries
                 (platform, model_pattern, state, reason, until, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             ON CONFLICT(platform, model_pattern) DO UPDATE SET
                 state = excluded.state,
                 reason = excluded.reason,
                 until = excluded.until,
                 updated_at = excluded.updated_at",
            params![
                entry.platform,
                entry.model_pattern,
                entry.state.as_str(),
                entry.reason,
                entry.until,
            ],
        )?;
        Ok(())
    }

    /// Delete the one entry for `(platform, model_pattern)`. Idempotent.
    pub fn roster_delete(&self, platform: &str, model_pattern: Option<&str>) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "DELETE FROM roster_entries WHERE platform = ?1 AND model_pattern IS ?2",
            params![platform, model_pattern],
        )?;
        Ok(())
    }

    /// Every entry, optionally filtered to one platform. `model_pattern` sorts
    /// NULLs first so the platform-wide row is listed before its overrides.
    pub fn roster_list(&self, platform: Option<&str>) -> Result<Vec<RosterEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, platform, model_pattern, state, reason, until, updated_at
             FROM roster_entries
             WHERE platform IS ?1 OR ?1 IS NULL
             ORDER BY platform ASC, model_pattern ASC",
        )?;
        let rows = stmt.query_map(params![platform], read_roster_entry_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Every entry for one platform. This is what the engine and preflight
    /// feed to [`crate::domain::roster::effective_state`].
    pub fn roster_list_for_platform(&self, platform: &str) -> Result<Vec<RosterEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, platform, model_pattern, state, reason, until, updated_at
             FROM roster_entries WHERE platform = ?1
             ORDER BY model_pattern ASC",
        )?;
        let rows = stmt.query_map(params![platform], read_roster_entry_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn entry<'a>(
        platform: &'a str,
        pattern: Option<&'a str>,
        state: RosterState,
    ) -> NewRosterEntry<'a> {
        NewRosterEntry {
            platform,
            model_pattern: pattern,
            state,
            reason: Some("quota"),
            until: None,
        }
    }

    #[test]
    fn upsert_then_list_roundtrip() {
        let db = test_db();
        db.roster_upsert(entry(
            "opencode-go",
            Some("opencode-go/*"),
            RosterState::Unavailable,
        ))
        .unwrap();
        let rows = db.roster_list(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].platform, "opencode-go");
        assert_eq!(rows[0].model_pattern.as_deref(), Some("opencode-go/*"));
        assert_eq!(rows[0].state, RosterState::Unavailable);
        assert_eq!(rows[0].reason.as_deref(), Some("quota"));
    }

    #[test]
    fn upsert_overwrites_on_conflict() {
        let db = test_db();
        db.roster_upsert(entry("claude", Some("opus"), RosterState::Unavailable))
            .unwrap();
        db.roster_upsert(entry("claude", Some("opus"), RosterState::Available))
            .unwrap();
        let rows = db.roster_list(None).unwrap();
        assert_eq!(rows.len(), 1, "same key must keep exactly one row");
        assert_eq!(rows[0].state, RosterState::Available);
    }

    #[test]
    fn delete_specific_pattern_leaves_null_pattern() {
        let db = test_db();
        db.roster_upsert(entry("claude", Some("opus"), RosterState::Unavailable))
            .unwrap();
        db.roster_upsert(entry("claude", None, RosterState::Unavailable))
            .unwrap();
        db.roster_delete("claude", Some("opus")).unwrap();
        let rows = db.roster_list(None).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model_pattern, None);
    }

    #[test]
    fn list_filters_by_platform() {
        let db = test_db();
        db.roster_upsert(entry("claude", None, RosterState::Unavailable))
            .unwrap();
        db.roster_upsert(entry("opencode-go", None, RosterState::Unavailable))
            .unwrap();
        let rows = db.roster_list(Some("claude")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].platform, "claude");
    }

    #[test]
    fn state_parse_rejects_unknown() {
        assert!(RosterState::parse("maybe").is_err());
        assert_eq!(
            RosterState::parse("unavailable").unwrap(),
            RosterState::Unavailable
        );
    }
}
