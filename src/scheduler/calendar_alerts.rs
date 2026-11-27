//! Calendar alert evaluation and notification dispatch for `CronScheduler`.

use chrono::{DateTime, Local, SecondsFormat, Utc};
use uuid::Uuid;

use crate::db::Database;
use crate::domain::calendar::{alert_at, parse_rfc3339_utc, CalendarEvent};
use crate::domain::notification::NotificationLevel;
use crate::sessions::protocol::ToastNotification;
use crate::sessions::{HubEvent, SessionHub};

/// Default snooze duration offered on calendar reminder toasts.
pub const DEFAULT_SNOOZE_MINUTES: u32 = 5;

/// Compute the effective alert instant for `event`, or `None` if dismissed or invalid.
pub fn effective_alert_at(event: &CalendarEvent) -> Option<DateTime<Utc>> {
    if event.dismissed_at.is_some() {
        return None;
    }
    if let Some(ref snoozed) = event.snoozed_until {
        return parse_rfc3339_utc(snoozed).ok();
    }
    let at = parse_rfc3339_utc(&event.at).ok()?;
    Some(alert_at(at, event.lead_minutes))
}

/// Return `true` when `event` is due at `now` and has not yet notified for its current alert instant.
pub fn is_due(event: &CalendarEvent, now: DateTime<Utc>) -> bool {
    let Some(eff) = effective_alert_at(event) else {
        return false;
    };
    if now < eff {
        return false;
    }
    !already_notified_for(event, eff)
}

fn already_notified_for(event: &CalendarEvent, eff: DateTime<Utc>) -> bool {
    event
        .last_notified_at
        .as_deref()
        .and_then(|raw| parse_rfc3339_utc(raw).ok())
        .is_some_and(|last| last >= eff)
}

/// Return the earliest pending (not yet notified) calendar alert instant across active events.
pub fn next_calendar_alert_at(db: &Database) -> Option<DateTime<Utc>> {
    let events = db.list_calendar_events(None, None, false).ok()?;
    events
        .iter()
        .filter_map(|event| {
            let eff = effective_alert_at(event)?;
            if already_notified_for(event, eff) {
                None
            } else {
                Some(eff)
            }
        })
        .min()
}

fn format_alert_body(event: &CalendarEvent) -> String {
    let time_str = parse_rfc3339_utc(&event.at)
        .map(|dt| {
            dt.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|_| event.at.clone());
    match event.notes.as_deref().map(str::trim) {
        Some(notes) if !notes.is_empty() => format!("{time_str} - {notes}"),
        _ => time_str,
    }
}

/// Fire native OS notifications and `HubEvent::Toast` for every due calendar event.
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
    let mut fired = 0;

    for event in &events {
        if !is_due(event, now_utc) {
            continue;
        }
        db.record_calendar_event_notified(&event.id, &now_str)?;
        let notif_title = format!("Canopy Calendar: {}", event.title);
        let body = format_alert_body(event);
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
}
