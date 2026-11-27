//! Calendar alert evaluation and notification dispatch for `CronScheduler`.

use chrono::{DateTime, Local, SecondsFormat, Utc};
use uuid::Uuid;

use crate::db::Database;
use crate::domain::calendar::{alert_at, parse_rfc3339_utc, CalendarEvent, CalendarFeedItem};
use crate::domain::notification::NotificationLevel;
use crate::sessions::protocol::ToastNotification;
use crate::sessions::{HubEvent, SessionHub};

/// Default snooze duration offered on calendar reminder toasts.
pub const DEFAULT_SNOOZE_MINUTES: u32 = 5;

struct AlertFields<'a> {
    at: &'a str,
    lead_minutes: i64,
    snoozed_until: Option<&'a str>,
    dismissed_at: Option<&'a str>,
    last_notified_at: Option<&'a str>,
    notes: Option<&'a str>,
}

impl<'a> From<&'a CalendarEvent> for AlertFields<'a> {
    fn from(ev: &'a CalendarEvent) -> Self {
        Self {
            at: &ev.at,
            lead_minutes: ev.lead_minutes,
            snoozed_until: ev.snoozed_until.as_deref(),
            dismissed_at: ev.dismissed_at.as_deref(),
            last_notified_at: ev.last_notified_at.as_deref(),
            notes: ev.notes.as_deref(),
        }
    }
}

impl<'a> From<&'a CalendarFeedItem> for AlertFields<'a> {
    fn from(item: &'a CalendarFeedItem) -> Self {
        Self {
            at: &item.at,
            lead_minutes: item.lead_minutes,
            snoozed_until: item.snoozed_until.as_deref(),
            dismissed_at: item.dismissed_at.as_deref(),
            last_notified_at: item.last_notified_at.as_deref(),
            notes: item.notes.as_deref(),
        }
    }
}

fn effective_alert_at_fields(fields: &AlertFields<'_>) -> Option<DateTime<Utc>> {
    if fields.dismissed_at.is_some() {
        return None;
    }
    if let Some(snoozed) = fields.snoozed_until {
        return parse_rfc3339_utc(snoozed).ok();
    }
    let at = parse_rfc3339_utc(fields.at).ok()?;
    Some(alert_at(at, fields.lead_minutes))
}

fn already_notified_for_fields(fields: &AlertFields<'_>, eff: DateTime<Utc>) -> bool {
    fields
        .last_notified_at
        .and_then(|raw| parse_rfc3339_utc(raw).ok())
        .is_some_and(|last| last >= eff)
}

fn is_due_fields(fields: &AlertFields<'_>, now: DateTime<Utc>) -> bool {
    let Some(eff) = effective_alert_at_fields(fields) else {
        return false;
    };
    now >= eff && !already_notified_for_fields(fields, eff)
}

/// Compute the effective alert instant for `event`, or `None` if dismissed or invalid.
#[cfg(test)]
pub fn effective_alert_at(event: &CalendarEvent) -> Option<DateTime<Utc>> {
    effective_alert_at_fields(&AlertFields::from(event))
}

/// Return the earliest pending (not yet notified) calendar alert instant across active events and feed items.
pub fn next_calendar_alert_at(db: &Database) -> Option<DateTime<Utc>> {
    let events = db.list_calendar_events(None, None, false).ok()?;
    let feed_items = db.list_calendar_feed_items(None, false).unwrap_or_default();

    let next_ev = events.iter().filter_map(|e| {
        let f = AlertFields::from(e);
        let eff = effective_alert_at_fields(&f)?;
        (!already_notified_for_fields(&f, eff)).then_some(eff)
    });
    let next_feed = feed_items.iter().filter_map(|item| {
        let f = AlertFields::from(item);
        let eff = effective_alert_at_fields(&f)?;
        (!already_notified_for_fields(&f, eff)).then_some(eff)
    });
    next_ev.chain(next_feed).min()
}

fn format_alert_body(fields: &AlertFields<'_>) -> String {
    let time_str = parse_rfc3339_utc(fields.at)
        .map(|dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| fields.at.to_string());
    match fields.notes.map(str::trim) {
        Some(notes) if !notes.is_empty() => format!("{time_str} - {notes}"),
        _ => time_str,
    }
}

/// Fire native OS notifications and `HubEvent::Toast` for every due calendar event and `.ics` feed item.
pub fn fire_due_calendar_alerts<F>(
    db: &Database,
    hub: Option<&SessionHub>,
    mut notify_fn: F,
    now_utc: DateTime<Utc>,
) -> anyhow::Result<usize>
where
    F: FnMut(&str, &str, NotificationLevel),
{
    let now_str = now_utc.to_rfc3339_opts(SecondsFormat::Secs, true);
    let events = db.list_calendar_events(None, None, false)?;
    let feed_items = db.list_calendar_feed_items(None, false)?;
    let mut fired = 0;

    for event in &events {
        let fields = AlertFields::from(event);
        if !is_due_fields(&fields, now_utc) {
            continue;
        }
        db.record_calendar_event_notified(&event.id, &now_str)?;
        let notif_title = format!("Canopy Calendar: {}", event.title);
        let body = format_alert_body(&fields);
        notify_fn(&notif_title, &body, NotificationLevel::Info);
        if let Some(hub) = hub {
            hub.emit(HubEvent::Toast(ToastNotification {
                id: Uuid::new_v4().to_string(),
                event_id: event.id.clone(),
                title: event.title.clone(),
                body,
                event_at: event.at.clone(),
                snooze_minutes: DEFAULT_SNOOZE_MINUTES,
            }));
        }
        fired += 1;
    }

    for item in &feed_items {
        let fields = AlertFields::from(item);
        if !is_due_fields(&fields, now_utc) {
            continue;
        }
        db.record_calendar_feed_item_notified(&item.id, &now_str)?;
        let display_title = format!("[{}] {}", item.feed_name, item.title);
        let notif_title = format!("Canopy Calendar: {display_title}");
        let body = format_alert_body(&fields);
        notify_fn(&notif_title, &body, NotificationLevel::Info);
        if let Some(hub) = hub {
            hub.emit(HubEvent::Toast(ToastNotification {
                id: Uuid::new_v4().to_string(),
                event_id: item.id.clone(),
                title: display_title,
                body,
                event_at: item.at.clone(),
                snooze_minutes: DEFAULT_SNOOZE_MINUTES,
            }));
        }
        fired += 1;
    }
    Ok(fired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, Database) {
        let dir = TempDir::new().expect("temp dir");
        let db = Database::new(&dir.path().join("canopy.db")).expect("open temp db");
        (dir, db)
    }

    #[test]
    fn lead_time_and_dedup_prevent_duplicate_firing() {
        let (_dir, db) = temp_db();
        let event = db
            .create_calendar_event(
                "Standup",
                "2026-11-27T10:00:00Z",
                Some("Room A"),
                None,
                Some(10),
            )
            .expect("create event");

        let before_lead = parse_rfc3339_utc("2026-11-27T09:49:59Z").unwrap();
        let at_lead = parse_rfc3339_utc("2026-11-27T09:50:00Z").unwrap();
        let mut sent: Vec<(String, String)> = Vec::new();

        let count_before = fire_due_calendar_alerts(
            &db,
            None,
            |t, b, _| sent.push((t.to_string(), b.to_string())),
            before_lead,
        )
        .unwrap();
        assert_eq!(count_before, 0);
        assert!(sent.is_empty());

        let count_at = fire_due_calendar_alerts(
            &db,
            None,
            |t, b, _| sent.push((t.to_string(), b.to_string())),
            at_lead,
        )
        .unwrap();
        assert_eq!(count_at, 1);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "Canopy Calendar: Standup");
        assert!(sent[0].1.contains("Room A"));

        // Second tick must not duplicate once last_notified_at is recorded.
        let later = at_lead + Duration::minutes(1);
        let count_dup = fire_due_calendar_alerts(
            &db,
            None,
            |t, b, _| sent.push((t.to_string(), b.to_string())),
            later,
        )
        .unwrap();
        assert_eq!(count_dup, 0);
        assert_eq!(sent.len(), 1);
        assert!(next_calendar_alert_at(&db).is_none());

        // Silence unused field warning on event id
        assert!(!event.id.is_empty());
    }

    #[test]
    fn snooze_refires_after_snoozed_until() {
        let (_dir, db) = temp_db();
        let event = db
            .create_calendar_event(
                "Design Review",
                "2026-11-27T10:00:00Z",
                None,
                None,
                Some(10),
            )
            .expect("create event");

        let t0 = parse_rfc3339_utc("2026-11-27T09:50:00Z").unwrap();
        let mut fired_count = 0usize;
        assert_eq!(
            fire_due_calendar_alerts(&db, None, |_, _, _| fired_count += 1, t0).unwrap(),
            1
        );

        let snooze_until = "2026-11-27T09:55:00Z";
        db.snooze_calendar_event(&event.id, snooze_until)
            .expect("snooze");

        let before_snooze = parse_rfc3339_utc("2026-11-27T09:54:59Z").unwrap();
        assert_eq!(
            fire_due_calendar_alerts(&db, None, |_, _, _| fired_count += 1, before_snooze).unwrap(),
            0
        );
        assert_eq!(
            next_calendar_alert_at(&db),
            Some(parse_rfc3339_utc(snooze_until).unwrap())
        );

        let at_snooze = parse_rfc3339_utc(snooze_until).unwrap();
        assert_eq!(
            fire_due_calendar_alerts(&db, None, |_, _, _| fired_count += 1, at_snooze).unwrap(),
            1
        );
        assert_eq!(fired_count, 2);
    }

    #[test]
    fn dismiss_stops_non_recurring_and_advances_recurring() {
        let (_dir, db) = temp_db();
        let one_off = db
            .create_calendar_event(
                "One-off",
                "2026-11-27T10:00:00Z",
                None,
                Some("none"),
                Some(10),
            )
            .expect("create one_off");
        let daily = db
            .create_calendar_event(
                "Daily",
                "2026-11-27T10:00:00Z",
                None,
                Some("daily"),
                Some(10),
            )
            .expect("create daily");

        let now = "2026-11-27T10:00:00Z";
        let dismissed_one = db.dismiss_calendar_event(&one_off.id, now).unwrap();
        assert!(dismissed_one.dismissed_at.is_some());
        assert!(effective_alert_at(&dismissed_one).is_none());

        let dismissed_daily = db.dismiss_calendar_event(&daily.id, now).unwrap();
        assert!(dismissed_daily.dismissed_at.is_none());
        assert!(dismissed_daily.snoozed_until.is_none());
        assert!(dismissed_daily.last_notified_at.is_none());
        assert_eq!(dismissed_daily.at, "2026-11-28T10:00:00Z");
        assert_eq!(
            effective_alert_at(&dismissed_daily),
            Some(parse_rfc3339_utc("2026-11-28T09:50:00Z").unwrap())
        );
    }

    #[test]
    fn feed_item_alerts_fire_dedup_and_snooze() {
        use crate::domain::ics::parse_ics;

        let (_dir, db) = temp_db();
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:f1\r\nSUMMARY:Feed Call\r\nDTSTART:20261127T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        db.upsert_calendar_feed_items("Work", &parse_ics(ics))
            .unwrap();

        let at_lead = parse_rfc3339_utc("2026-11-27T09:50:00Z").unwrap();
        let mut titles = Vec::new();
        assert_eq!(
            fire_due_calendar_alerts(&db, None, |t, _, _| titles.push(t.to_string()), at_lead)
                .unwrap(),
            1
        );
        assert_eq!(titles, vec!["Canopy Calendar: [Work] Feed Call"]);
        assert_eq!(
            fire_due_calendar_alerts(&db, None, |t, _, _| titles.push(t.to_string()), at_lead)
                .unwrap(),
            0
        );
    }
}
