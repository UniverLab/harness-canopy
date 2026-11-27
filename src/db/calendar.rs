//! SQLite persistence for Canopy Calendar (`calendar_events` and `calendar_milestones`).

use anyhow::{anyhow, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};
use std::str::FromStr;
use uuid::Uuid;

use crate::db::Database;
use crate::domain::calendar::{
    parse_rfc3339_utc, validate_lead_minutes, CalendarEvent, CalendarMilestone, MilestoneStatus,
    Recurrence,
};

/// Schema for `calendar_events`, `calendar_milestones`, and their indexes.
pub(super) const CALENDAR_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS calendar_events (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                notes TEXT,
                at TEXT NOT NULL,
                recurrence TEXT NOT NULL DEFAULT 'none',
                lead_minutes INTEGER NOT NULL DEFAULT 10,
                snoozed_until TEXT,
                dismissed_at TEXT,
                last_notified_at TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_calendar_events_at
                ON calendar_events(at);

            CREATE TABLE IF NOT EXISTS calendar_milestones (
                id TEXT PRIMARY KEY,
                project_path TEXT NOT NULL,
                title TEXT NOT NULL,
                notes TEXT,
                due_at TEXT,
                status TEXT NOT NULL DEFAULT 'planned',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_calendar_milestones_project_status
                ON calendar_milestones(project_path, status);";

const EVENT_COLUMNS: &str = "id, title, notes, at, recurrence, lead_minutes, snoozed_until, dismissed_at, last_notified_at, created_at, updated_at";
const MILESTONE_COLUMNS: &str =
    "id, project_path, title, notes, due_at, status, created_at, updated_at";

/// Fields for updating an existing [`CalendarEvent`].
#[derive(Debug, Default)]
pub struct CalendarEventUpdate<'a> {
    pub title: Option<&'a str>,
    pub notes: Option<&'a str>,
    pub at: Option<&'a str>,
    pub recurrence: Option<&'a str>,
    pub lead_minutes: Option<i64>,
    pub snoozed_until: Option<&'a str>,
    pub dismissed_at: Option<&'a str>,
}

/// Fields for updating an existing [`CalendarMilestone`].
#[derive(Debug, Default)]
pub struct CalendarMilestoneUpdate<'a> {
    pub title: Option<&'a str>,
    pub notes: Option<&'a str>,
    pub due_at: Option<&'a str>,
    pub status: Option<&'a str>,
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn normalize_timestamp(raw: &str) -> Result<String> {
    Ok(parse_rfc3339_utc(raw)?.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn require_non_empty<'a>(value: &'a str, field: &str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("{field} must not be empty"));
    }
    Ok(trimmed)
}

fn row_to_event(row: &Row<'_>) -> rusqlite::Result<CalendarEvent> {
    let recurrence_raw: String = row.get(4)?;
    let recurrence = Recurrence::from_str(&recurrence_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(CalendarEvent {
        id: row.get(0)?,
        title: row.get(1)?,
        notes: row.get(2)?,
        at: row.get(3)?,
        recurrence,
        lead_minutes: row.get(5)?,
        snoozed_until: row.get(6)?,
        dismissed_at: row.get(7)?,
        last_notified_at: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

fn row_to_milestone(row: &Row<'_>) -> rusqlite::Result<CalendarMilestone> {
    let status_raw: String = row.get(5)?;
    let status = MilestoneStatus::from_str(&status_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(CalendarMilestone {
        id: row.get(0)?,
        project_path: row.get(1)?,
        title: row.get(2)?,
        notes: row.get(3)?,
        due_at: row.get(4)?,
        status,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

impl Database {
    /// Ensure `calendar_events` and `calendar_milestones` exist on older databases.
    pub(super) fn migrate_calendar_tables(conn: &Connection) -> Result<()> {
        conn.execute_batch(CALENDAR_SCHEMA)
            .map_err(|e| anyhow!("calendar schema migration failed: {e}"))?;
        Ok(())
    }

    /// List calendar events optionally bounded by `[from, to]` and filtered by dismissal.
    pub fn list_calendar_events(
        &self,
        from: Option<&str>,
        to: Option<&str>,
        include_dismissed: bool,
    ) -> Result<Vec<CalendarEvent>> {
        let from_norm = from.map(normalize_timestamp).transpose()?;
        let to_norm = to.map(normalize_timestamp).transpose()?;

        let mut clauses: Vec<&'static str> = Vec::new();
        let mut bind: Vec<Value> = Vec::new();
        if !include_dismissed {
            clauses.push("dismissed_at IS NULL");
        }
        if let Some(f) = from_norm {
            clauses.push("at >= ?");
            bind.push(Value::Text(f));
        }
        if let Some(t) = to_norm {
            clauses.push("at <= ?");
            bind.push(Value::Text(t));
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {EVENT_COLUMNS} FROM calendar_events{where_sql} ORDER BY at ASC, id ASC"
        );

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind), row_to_event)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Create a new personal calendar event.
    pub fn create_calendar_event(
        &self,
        title: &str,
        at: &str,
        notes: Option<&str>,
        recurrence: Option<&str>,
        lead_minutes: Option<i64>,
    ) -> Result<CalendarEvent> {
        let title = require_non_empty(title, "title")?.to_string();
        let at = normalize_timestamp(at)?;
        let recurrence = match recurrence {
            Some(r) => Recurrence::from_str(r)?,
            None => Recurrence::None,
        };
        let lead_minutes = validate_lead_minutes(lead_minutes.unwrap_or(10))?;
        let now = now_rfc3339();
        let event = CalendarEvent {
            id: Uuid::new_v4().to_string(),
            title,
            notes: notes.map(str::to_string),
            at,
            recurrence,
            lead_minutes,
            snoozed_until: None,
            dismissed_at: None,
            last_notified_at: None,
            created_at: now.clone(),
            updated_at: now,
        };

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        conn.execute(
            "INSERT INTO calendar_events
                (id, title, notes, at, recurrence, lead_minutes, snoozed_until, dismissed_at, last_notified_at, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                event.id,
                event.title,
                event.notes,
                event.at,
                event.recurrence.as_str(),
                event.lead_minutes,
                event.snoozed_until,
                event.dismissed_at,
                event.last_notified_at,
                event.created_at,
                event.updated_at,
            ],
        )?;
        Ok(event)
    }

    /// Update an existing calendar event by id.
    pub fn update_calendar_event(
        &self,
        id: &str,
        update: &CalendarEventUpdate<'_>,
    ) -> Result<CalendarEvent> {
        let id = require_non_empty(id, "id")?;
        let new_title = update
            .title
            .map(|t| require_non_empty(t, "title").map(str::to_string))
            .transpose()?;
        let new_at = update.at.map(normalize_timestamp).transpose()?;
        let new_recurrence = update.recurrence.map(Recurrence::from_str).transpose()?;
        let new_lead = update.lead_minutes.map(validate_lead_minutes).transpose()?;
        let new_snoozed = update.snoozed_until.map(normalize_timestamp).transpose()?;
        let new_dismissed = update.dismissed_at.map(normalize_timestamp).transpose()?;

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let select_sql = format!("SELECT {EVENT_COLUMNS} FROM calendar_events WHERE id = ?1");
        let mut event = conn
            .query_row(&select_sql, [id], row_to_event)
            .optional()?
            .ok_or_else(|| anyhow!("Calendar event '{id}' not found"))?;

        if let Some(t) = new_title {
            event.title = t;
        }
        if let Some(n) = update.notes {
            event.notes = Some(n.to_string());
        }
        if let Some(a) = new_at {
            event.at = a;
        }
        if let Some(r) = new_recurrence {
            event.recurrence = r;
        }
        if let Some(l) = new_lead {
            event.lead_minutes = l;
        }
        if let Some(s) = new_snoozed {
            event.snoozed_until = Some(s);
        }
        if let Some(d) = new_dismissed {
            event.dismissed_at = Some(d);
        }
        event.updated_at = now_rfc3339();

        conn.execute(
            "UPDATE calendar_events
             SET title = ?2, notes = ?3, at = ?4, recurrence = ?5, lead_minutes = ?6,
                 snoozed_until = ?7, dismissed_at = ?8, updated_at = ?9
             WHERE id = ?1",
            params![
                event.id,
                event.title,
                event.notes,
                event.at,
                event.recurrence.as_str(),
                event.lead_minutes,
                event.snoozed_until,
                event.dismissed_at,
                event.updated_at,
            ],
        )?;
        Ok(event)
    }

    /// Delete a calendar event by id. Returns `true` if a row was removed.
    pub fn delete_calendar_event(&self, id: &str) -> Result<bool> {
        let id = require_non_empty(id, "id")?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let affected = conn.execute("DELETE FROM calendar_events WHERE id = ?1", [id])?;
        Ok(affected > 0)
    }

    /// List milestones optionally filtered by `project_path` and `status`.
    pub fn list_calendar_milestones(
        &self,
        project_path: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<CalendarMilestone>> {
        let status_norm = status.map(MilestoneStatus::from_str).transpose()?;
        let project_norm = project_path
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let mut clauses: Vec<&'static str> = Vec::new();
        let mut bind: Vec<Value> = Vec::new();
        if let Some(p) = project_norm {
            clauses.push("project_path = ?");
            bind.push(Value::Text(p));
        }
        if let Some(s) = status_norm {
            clauses.push("status = ?");
            bind.push(Value::Text(s.as_str().to_string()));
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {MILESTONE_COLUMNS} FROM calendar_milestones{where_sql}
             ORDER BY CASE WHEN due_at IS NULL THEN 1 ELSE 0 END ASC, due_at ASC, created_at ASC, id ASC"
        );

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind), row_to_milestone)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Create a new per-project roadmap milestone.
    pub fn create_calendar_milestone(
        &self,
        project_path: &str,
        title: &str,
        notes: Option<&str>,
        due_at: Option<&str>,
        status: Option<&str>,
    ) -> Result<CalendarMilestone> {
        let project_path = require_non_empty(project_path, "project_path")?.to_string();
        let title = require_non_empty(title, "title")?.to_string();
        let due_at = due_at.map(normalize_timestamp).transpose()?;
        let status = match status {
            Some(s) => MilestoneStatus::from_str(s)?,
            None => MilestoneStatus::Planned,
        };
        let now = now_rfc3339();
        let milestone = CalendarMilestone {
            id: Uuid::new_v4().to_string(),
            project_path,
            title,
            notes: notes.map(str::to_string),
            due_at,
            status,
            created_at: now.clone(),
            updated_at: now,
        };

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        conn.execute(
            "INSERT INTO calendar_milestones
                (id, project_path, title, notes, due_at, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                milestone.id,
                milestone.project_path,
                milestone.title,
                milestone.notes,
                milestone.due_at,
                milestone.status.as_str(),
                milestone.created_at,
                milestone.updated_at,
            ],
        )?;
        Ok(milestone)
    }

    /// Update an existing milestone by id.
    pub fn update_calendar_milestone(
        &self,
        id: &str,
        update: &CalendarMilestoneUpdate<'_>,
    ) -> Result<CalendarMilestone> {
        let id = require_non_empty(id, "id")?;
        let new_title = update
            .title
            .map(|t| require_non_empty(t, "title").map(str::to_string))
            .transpose()?;
        let new_due_at = update.due_at.map(normalize_timestamp).transpose()?;
        let new_status = update.status.map(MilestoneStatus::from_str).transpose()?;

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let select_sql =
            format!("SELECT {MILESTONE_COLUMNS} FROM calendar_milestones WHERE id = ?1");
        let mut milestone = conn
            .query_row(&select_sql, [id], row_to_milestone)
            .optional()?
            .ok_or_else(|| anyhow!("Calendar milestone '{id}' not found"))?;

        if let Some(t) = new_title {
            milestone.title = t;
        }
        if let Some(n) = update.notes {
            milestone.notes = Some(n.to_string());
        }
        if let Some(d) = new_due_at {
            milestone.due_at = Some(d);
        }
        if let Some(s) = new_status {
            milestone.status = s;
        }
        milestone.updated_at = now_rfc3339();

        conn.execute(
            "UPDATE calendar_milestones
             SET title = ?2, notes = ?3, due_at = ?4, status = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                milestone.id,
                milestone.title,
                milestone.notes,
                milestone.due_at,
                milestone.status.as_str(),
                milestone.updated_at,
            ],
        )?;
        Ok(milestone)
    }

    /// Delete a milestone by id. Returns `true` if a row was removed.
    pub fn delete_calendar_milestone(&self, id: &str) -> Result<bool> {
        let id = require_non_empty(id, "id")?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let affected = conn.execute("DELETE FROM calendar_milestones WHERE id = ?1", [id])?;
        Ok(affected > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, Database) {
        let dir = TempDir::new().expect("temp dir");
        let db = Database::new(&dir.path().join("canopy.db")).expect("open temp db");
        (dir, db)
    }

    #[test]
    fn calendar_events_crud_roundtrip_and_filters() {
        let (_dir, db) = temp_db();

        let e1 = db
            .create_calendar_event(
                "Standup",
                "2026-11-27T09:00:00Z",
                Some("Daily sync"),
                Some("daily"),
                Some(15),
            )
            .expect("create e1");
        assert_eq!(e1.recurrence, Recurrence::Daily);
        assert_eq!(e1.lead_minutes, 15);

        let e2 = db
            .create_calendar_event("Release", "2026-12-01T17:00:00Z", None, None, None)
            .expect("create e2");
        assert_eq!(e2.recurrence, Recurrence::None);
        assert_eq!(e2.lead_minutes, 10);

        let all = db
            .list_calendar_events(None, None, false)
            .expect("list all");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, e1.id);

        let ranged = db
            .list_calendar_events(
                Some("2026-11-28T00:00:00Z"),
                Some("2026-12-02T00:00:00Z"),
                false,
            )
            .expect("list ranged");
        assert_eq!(ranged.len(), 1);
        assert_eq!(ranged[0].id, e2.id);

        let updated = db
            .update_calendar_event(
                &e1.id,
                &CalendarEventUpdate {
                    title: Some("Team Standup"),
                    dismissed_at: Some("2026-11-27T09:05:00Z"),
                    ..Default::default()
                },
            )
            .expect("update e1");
        assert_eq!(updated.title, "Team Standup");
        assert!(updated.dismissed_at.is_some());

        let active = db
            .list_calendar_events(None, None, false)
            .expect("list active");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, e2.id);

        let with_dismissed = db
            .list_calendar_events(None, None, true)
            .expect("list with dismissed");
        assert_eq!(with_dismissed.len(), 2);

        assert!(db.delete_calendar_event(&e1.id).expect("delete e1"));
        assert!(!db.delete_calendar_event(&e1.id).expect("delete missing"));
    }

    #[test]
    fn calendar_milestones_crud_roundtrip_and_filters() {
        let (_dir, db) = temp_db();

        let m1 = db
            .create_calendar_milestone(
                "/tmp/proj-a",
                "Alpha",
                Some("First cut"),
                Some("2026-12-10T00:00:00Z"),
                Some("active"),
            )
            .expect("create m1");
        let m2 = db
            .create_calendar_milestone("/tmp/proj-b", "Beta", None, None, None)
            .expect("create m2");
        assert_eq!(m2.status, MilestoneStatus::Planned);

        let proj_a = db
            .list_calendar_milestones(Some("/tmp/proj-a"), None)
            .expect("filter by project");
        assert_eq!(proj_a.len(), 1);
        assert_eq!(proj_a[0].id, m1.id);

        let planned = db
            .list_calendar_milestones(None, Some("planned"))
            .expect("filter by status");
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id, m2.id);

        let updated = db
            .update_calendar_milestone(
                &m1.id,
                &CalendarMilestoneUpdate {
                    status: Some("done"),
                    ..Default::default()
                },
            )
            .expect("update m1");
        assert_eq!(updated.status, MilestoneStatus::Done);

        assert!(db.delete_calendar_milestone(&m1.id).expect("delete m1"));
        assert!(!db
            .delete_calendar_milestone(&m1.id)
            .expect("delete missing"));
    }

    #[test]
    fn calendar_validation_rejects_invalid_inputs() {
        let (_dir, db) = temp_db();

        assert!(db
            .create_calendar_event("Bad time", "not-rfc3339", None, None, None)
            .is_err());
        assert!(db
            .create_calendar_event(
                "Bad rec",
                "2026-11-27T10:00:00Z",
                None,
                Some("yearly"),
                None
            )
            .is_err());
        assert!(db
            .create_calendar_event("Bad lead", "2026-11-27T10:00:00Z", None, None, Some(-5))
            .is_err());
        assert!(db
            .create_calendar_milestone("/tmp/p", "Bad status", None, None, Some("bogus"))
            .is_err());
        assert!(db
            .create_calendar_milestone("/tmp/p", "Bad due", None, Some("bad-date"), None)
            .is_err());
    }
}
