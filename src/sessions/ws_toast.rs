//! WebSocket handlers for calendar reminder toast actions (`toast_dismiss`, `toast_snooze`).

use chrono::{Duration, SecondsFormat, Utc};

use crate::db::Database;
use crate::scheduler::calendar_alerts::DEFAULT_SNOOZE_MINUTES;
use crate::sessions::protocol::{
    error_frame, ok_frame, ClientFrame, ERR_BAD_REQUEST, ERR_NOT_FOUND,
};

/// Handle a `toast_dismiss` or `toast_snooze` client frame and return the response frame JSON.
pub(crate) fn handle(db: &Database, frame: ClientFrame) -> String {
    match frame {
        ClientFrame::ToastDismiss { req, event_id } => {
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
            match db.dismiss_calendar_event(&event_id, &now) {
                Ok(_) => ok_frame(&req),
                Err(err) if err.to_string().contains("not found") => {
                    match db.dismiss_calendar_feed_item(&event_id, &now) {
                        Ok(_) => ok_frame(&req),
                        Err(feed_err) => toast_err(&req, &feed_err),
                    }
                }
                Err(err) => toast_err(&req, &err),
            }
        }
        ClientFrame::ToastSnooze {
            req,
            event_id,
            minutes,
        } => {
            let mins = i64::from(minutes.unwrap_or(DEFAULT_SNOOZE_MINUTES).max(1));
            let until =
                (Utc::now() + Duration::minutes(mins)).to_rfc3339_opts(SecondsFormat::Secs, true);
            match db.snooze_calendar_event(&event_id, &until) {
                Ok(_) => ok_frame(&req),
                Err(err) if err.to_string().contains("not found") => {
                    match db.snooze_calendar_feed_item(&event_id, &until) {
                        Ok(_) => ok_frame(&req),
                        Err(feed_err) => toast_err(&req, &feed_err),
                    }
                }
                Err(err) => toast_err(&req, &err),
            }
        }
        _ => error_frame(None, ERR_BAD_REQUEST, "unexpected frame for ws_toast"),
    }
}

fn toast_err(req: &str, err: &anyhow::Error) -> String {
    let msg = err.to_string();
    let code = if msg.contains("not found") {
        ERR_NOT_FOUND
    } else {
        ERR_BAD_REQUEST
    };
    error_frame(Some(req), code, &msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::calendar::parse_rfc3339_utc;
    use tempfile::TempDir;

    fn temp_db() -> (TempDir, Database) {
        let dir = TempDir::new().expect("temp dir");
        let db = Database::new(&dir.path().join("canopy.db")).expect("open temp db");
        (dir, db)
    }

    #[test]
    fn handle_dismiss_and_snooze_updates_db_and_returns_ok_or_not_found() {
        let (_dir, db) = temp_db();
        let ev = db
            .create_calendar_event("Sync", "2026-11-27T10:00:00Z", None, None, Some(10))
            .unwrap();

        let snooze_reply = handle(
            &db,
            ClientFrame::ToastSnooze {
                req: "r1".to_string(),
                event_id: ev.id.clone(),
                minutes: None,
            },
        );
        assert!(snooze_reply.contains(r#""t":"ok""#), "{snooze_reply}");
        let snoozed = db
            .list_calendar_events(None, None, false)
            .unwrap()
            .into_iter()
            .find(|e| e.id == ev.id)
            .unwrap();
        assert!(snoozed.snoozed_until.is_some());
        assert!(snoozed.dismissed_at.is_none());
        let snooze_dt = parse_rfc3339_utc(snoozed.snoozed_until.as_deref().unwrap()).unwrap();
        assert!(snooze_dt > Utc::now() + Duration::minutes(4));

        let dismiss_reply = handle(
            &db,
            ClientFrame::ToastDismiss {
                req: "r2".to_string(),
                event_id: ev.id,
            },
        );
        assert!(dismiss_reply.contains(r#""t":"ok""#), "{dismiss_reply}");
        assert!(db
            .list_calendar_events(None, None, false)
            .unwrap()
            .is_empty());

        let missing_reply = handle(
            &db,
            ClientFrame::ToastDismiss {
                req: "r3".to_string(),
                event_id: "missing-id".to_string(),
            },
        );
        assert!(missing_reply.contains(ERR_NOT_FOUND), "{missing_reply}");
    }

    #[test]
    fn handle_dismiss_and_snooze_supports_feed_items() {
        use crate::domain::ics::parse_ics;

        let (_dir, db) = temp_db();
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:u1\r\nSUMMARY:Feed Sync\r\nDTSTART:20261127T100000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        db.upsert_calendar_feed_items("Work", &parse_ics(ics))
            .unwrap();

        let snooze_reply = handle(
            &db,
            ClientFrame::ToastSnooze {
                req: "r1".to_string(),
                event_id: "Work:u1".to_string(),
                minutes: Some(5),
            },
        );
        assert!(snooze_reply.contains(r#""t":"ok""#), "{snooze_reply}");
        let items = db.list_calendar_feed_items(Some("Work"), false).unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].snoozed_until.is_some());

        let dismiss_reply = handle(
            &db,
            ClientFrame::ToastDismiss {
                req: "r2".to_string(),
                event_id: "Work:u1".to_string(),
            },
        );
        assert!(dismiss_reply.contains(r#""t":"ok""#), "{dismiss_reply}");
        assert!(db
            .list_calendar_feed_items(Some("Work"), false)
            .unwrap()
            .is_empty());
    }
}
