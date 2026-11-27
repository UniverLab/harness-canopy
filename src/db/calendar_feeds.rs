//! SQLite persistence for cached read-only `.ics` feed items (`calendar_feed_items`).

use anyhow::{anyhow, Result};
use chrono::{SecondsFormat, Utc};
use rusqlite::{params, types::Value, Connection, OptionalExtension, Row};
use std::collections::HashSet;
use std::str::FromStr;

use crate::db::Database;
use crate::domain::calendar::{next_occurrence, parse_rfc3339_utc, CalendarFeedItem, Recurrence};
use crate::domain::ics::IcsEvent;

/// Schema for `calendar_feed_items` and its indexes.
pub(super) const CALENDAR_FEEDS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS calendar_feed_items (
                id TEXT PRIMARY KEY,
                feed_name TEXT NOT NULL,
                uid TEXT NOT NULL,
                title TEXT NOT NULL,
                notes TEXT,
                at TEXT NOT NULL,
                recurrence TEXT NOT NULL DEFAULT 'none',
                lead_minutes INTEGER NOT NULL DEFAULT 10,
                snoozed_until TEXT,
                dismissed_at TEXT,
                last_notified_at TEXT,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_calendar_feed_items_feed
                ON calendar_feed_items(feed_name);
            CREATE INDEX IF NOT EXISTS idx_calendar_feed_items_at
                ON calendar_feed_items(at);";

const FEED_ITEM_COLUMNS: &str = "id, feed_name, uid, title, notes, at, recurrence, lead_minutes, snoozed_until, dismissed_at, last_notified_at, updated_at";

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn row_to_feed_item(row: &Row<'_>) -> rusqlite::Result<CalendarFeedItem> {
    let recurrence_raw: String = row.get(6)?;
    let recurrence = Recurrence::from_str(&recurrence_raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, e.into())
    })?;
    Ok(CalendarFeedItem {
        id: row.get(0)?,
        feed_name: row.get(1)?,
        uid: row.get(2)?,
        title: row.get(3)?,
        notes: row.get(4)?,
        at: row.get(5)?,
        recurrence,
        lead_minutes: row.get(7)?,
        snoozed_until: row.get(8)?,
        dismissed_at: row.get(9)?,
        last_notified_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn select_feed_item_by_id(conn: &Connection, id: &str) -> Result<CalendarFeedItem> {
    let sql = format!("SELECT {FEED_ITEM_COLUMNS} FROM calendar_feed_items WHERE id = ?1");
    conn.query_row(&sql, [id], row_to_feed_item)
        .optional()?
        .ok_or_else(|| anyhow!("Calendar feed item '{id}' not found"))
}

impl Database {
    /// Ensure `calendar_feed_items` exists on older databases.
    pub(super) fn migrate_calendar_feeds(conn: &Connection) -> Result<()> {
        conn.execute_batch(CALENDAR_FEEDS_SCHEMA)
            .map_err(|e| anyhow!("calendar_feed_items schema migration failed: {e}"))?;
        Ok(())
    }

    /// Upsert parsed `.ics` events for `feed_name`, preserving existing
    /// `snoozed_until`, `dismissed_at`, `last_notified_at`, and `lead_minutes`.
    /// Removes rows for `feed_name` that are no longer present in `items`.
    pub fn upsert_calendar_feed_items(&self, feed_name: &str, items: &[IcsEvent]) -> Result<()> {
        let feed_name = feed_name.trim();
        if feed_name.is_empty() {
            return Err(anyhow!("feed_name must not be empty"));
        }
        let now = now_rfc3339();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let tx = conn.transaction()?;

        let mut active_ids: HashSet<String> = HashSet::with_capacity(items.len());
        {
            let mut stmt = tx.prepare(
                "INSERT INTO calendar_feed_items
                    (id, feed_name, uid, title, notes, at, recurrence, lead_minutes, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 10, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                    feed_name = excluded.feed_name,
                    uid = excluded.uid,
                    title = excluded.title,
                    notes = excluded.notes,
                    at = excluded.at,
                    recurrence = excluded.recurrence,
                    updated_at = excluded.updated_at",
            )?;
            for item in items {
                let id = format!("{feed_name}:{}", item.uid);
                let at = item.at.to_rfc3339_opts(SecondsFormat::Secs, true);
                stmt.execute(params![
                    id,
                    feed_name,
                    item.uid,
                    item.title,
                    item.notes,
                    at,
                    item.recurrence.as_str(),
                    now,
                ])?;
                active_ids.insert(id);
            }
        }

        let existing_ids: Vec<String> = {
            let mut stmt = tx.prepare("SELECT id FROM calendar_feed_items WHERE feed_name = ?1")?;
            let rows = stmt.query_map([feed_name], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        {
            let mut del = tx.prepare("DELETE FROM calendar_feed_items WHERE id = ?1")?;
            for old_id in existing_ids {
                if !active_ids.contains(&old_id) {
                    del.execute([old_id])?;
                }
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// List cached `.ics` feed items, optionally filtered by `feed_name` and dismissal.
    pub fn list_calendar_feed_items(
        &self,
        feed_name: Option<&str>,
        include_dismissed: bool,
    ) -> Result<Vec<CalendarFeedItem>> {
        let mut clauses: Vec<&'static str> = Vec::new();
        let mut bind: Vec<Value> = Vec::new();
        if !include_dismissed {
            clauses.push("dismissed_at IS NULL");
        }
        if let Some(name) = feed_name.map(str::trim).filter(|s| !s.is_empty()) {
            clauses.push("feed_name = ?");
            bind.push(Value::Text(name.to_string()));
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {FEED_ITEM_COLUMNS} FROM calendar_feed_items{where_sql} ORDER BY at ASC, id ASC"
        );
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(bind), row_to_feed_item)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Record that a feed item fired a notification at `now`.
    pub fn record_calendar_feed_item_notified(
        &self,
        id: &str,
        now: &str,
    ) -> Result<CalendarFeedItem> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("id must not be empty"));
        }
        let now_norm = parse_rfc3339_utc(now)?.to_rfc3339_opts(SecondsFormat::Secs, true);
        let updated_at = now_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let affected = conn.execute(
            "UPDATE calendar_feed_items
             SET last_notified_at = ?2, updated_at = ?3
             WHERE id = ?1",
            params![id, now_norm, updated_at],
        )?;
        if affected == 0 {
            return Err(anyhow!("Calendar feed item '{id}' not found"));
        }
        select_feed_item_by_id(&conn, id)
    }

    /// Snooze a feed item until `until` (clears `dismissed_at`).
    pub fn snooze_calendar_feed_item(&self, id: &str, until: &str) -> Result<CalendarFeedItem> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("id must not be empty"));
        }
        let until_norm = parse_rfc3339_utc(until)?.to_rfc3339_opts(SecondsFormat::Secs, true);
        let updated_at = now_rfc3339();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let affected = conn.execute(
            "UPDATE calendar_feed_items
             SET snoozed_until = ?2, dismissed_at = NULL, updated_at = ?3
             WHERE id = ?1",
            params![id, until_norm, updated_at],
        )?;
        if affected == 0 {
            return Err(anyhow!("Calendar feed item '{id}' not found"));
        }
        select_feed_item_by_id(&conn, id)
    }

    /// Dismiss a feed item at `now`.
    pub fn dismiss_calendar_feed_item(&self, id: &str, now: &str) -> Result<CalendarFeedItem> {
        let id = id.trim();
        if id.is_empty() {
            return Err(anyhow!("id must not be empty"));
        }
        let now_dt = parse_rfc3339_utc(now)?;
        let now_norm = now_dt.to_rfc3339_opts(SecondsFormat::Secs, true);
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let mut item = select_feed_item_by_id(&conn, id)?;
        if item.recurrence == Recurrence::None {
            item.dismissed_at = Some(now_norm);
        } else {
            let at_dt = parse_rfc3339_utc(&item.at)?;
            if let Some(next_at) = next_occurrence(at_dt, item.recurrence, now_dt) {
                item.at = next_at.to_rfc3339_opts(SecondsFormat::Secs, true);
                item.snoozed_until = None;
                item.last_notified_at = None;
                item.dismissed_at = None;
            } else {
                item.dismissed_at = Some(now_norm);
            }
        }
        item.updated_at = now_rfc3339();
        conn.execute(
            "UPDATE calendar_feed_items
             SET at = ?2, snoozed_until = ?3, dismissed_at = ?4, last_notified_at = ?5, updated_at = ?6
             WHERE id = ?1",
            params![
                item.id,
                item.at,
                item.snoozed_until,
                item.dismissed_at,
                item.last_notified_at,
                item.updated_at,
            ],
        )?;
        Ok(item)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ics::parse_ics;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, Database) {
        let dir = TempDir::new().expect("temp dir");
        let db = Database::new(&dir.path().join("canopy.db")).expect("open temp db");
        (dir, db)
    }

    #[test]
    fn upsert_preserves_alert_state_and_prunes_removed_items() {
        let (_dir, db) = temp_db();
        let ics_v1 = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:First\r\nDTSTART:20261201T100000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:u2\r\nSUMMARY:Second\r\nDTSTART:20261202T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let events_v1 = parse_ics(ics_v1);
        db.upsert_calendar_feed_items("Work", &events_v1).unwrap();

        db.snooze_calendar_feed_item("Work:u1", "2026-12-01T09:55:00Z")
            .unwrap();
        db.record_calendar_feed_item_notified("Work:u1", "2026-12-01T09:50:00Z")
            .unwrap();

        let ics_v2 = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:First Renamed\r\nDTSTART:20261201T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let events_v2 = parse_ics(ics_v2);
        db.upsert_calendar_feed_items("Work", &events_v2).unwrap();

        let items = db.list_calendar_feed_items(Some("Work"), true).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "First Renamed");
        assert_eq!(
            items[0].snoozed_until.as_deref(),
            Some("2026-12-01T09:55:00Z")
        );
        assert_eq!(
            items[0].last_notified_at.as_deref(),
            Some("2026-12-01T09:50:00Z")
        );
    }
}
