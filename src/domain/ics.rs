//! Pure RFC 5545 `.ics` (iCalendar) parser and exporter.
//!
//! Supports read-only `VEVENT` ingestion (line unfolding, `UID`, `SUMMARY`,
//! `DESCRIPTION`, `DTSTART` in UTC `Z` / `TZID` / `VALUE=DATE`, `DTEND`, and
//! `RRULE` `FREQ=DAILY|WEEKLY|MONTHLY`) and exporting Canopy events and
//! milestones as a `VCALENDAR`.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::domain::calendar::{parse_rfc3339_utc, CalendarEvent, CalendarMilestone, Recurrence};

/// Configuration for a single read-only `.ics` calendar feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarFeedConfig {
    pub name: String,
    pub source: String,
    #[serde(default = "default_calendar_feed_refresh_minutes")]
    pub refresh_minutes: u64,
}

fn default_calendar_feed_refresh_minutes() -> u64 {
    15
}

/// `[calendar]` section in `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarConfig {
    #[serde(default)]
    pub feeds: Vec<CalendarFeedConfig>,
}

/// A parsed `VEVENT` from an `.ics` feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IcsEvent {
    pub uid: String,
    pub title: String,
    pub notes: Option<String>,
    pub at: DateTime<Utc>,
    pub dtend: Option<DateTime<Utc>>,
    pub recurrence: Recurrence,
}

/// Unfold RFC 5545 folded lines (`CRLF` or `LF` followed by a single space or tab).
pub fn unfold_lines(raw: &str) -> Vec<String> {
    let normalized = raw.replace("\r\n", "\n");
    let mut lines: Vec<String> = Vec::new();
    for segment in normalized.split('\n') {
        if let Some(rest) = segment
            .strip_prefix(' ')
            .or_else(|| segment.strip_prefix('\t'))
        {
            if let Some(last) = lines.last_mut() {
                last.push_str(rest);
                continue;
            }
        }
        lines.push(segment.to_string());
    }
    lines
}

/// Parse an RFC 5545 `.ics` payload into `IcsEvent` records.
///
/// Malformed `VEVENT` blocks (missing `UID`, `SUMMARY`, or unparseable `DTSTART`)
/// are skipped without panicking. Nested components such as `VALARM` are ignored.
pub fn parse_ics(raw: &str) -> Vec<IcsEvent> {
    let lines = unfold_lines(raw);
    let mut events = Vec::new();
    let mut current: Option<PartialVEvent> = None;
    let mut nested_depth: usize = 0;

    for line in lines {
        let trimmed = line.trim_end_matches('\r');
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.eq_ignore_ascii_case("BEGIN:VEVENT") {
            current = Some(PartialVEvent::default());
            nested_depth = 0;
            continue;
        }
        if trimmed.eq_ignore_ascii_case("END:VEVENT") {
            if let Some(partial) = current.take() {
                if let Some(ev) = partial.finish() {
                    events.push(ev);
                }
            }
            nested_depth = 0;
            continue;
        }
        let Some(ref mut ev) = current else {
            continue;
        };
        if let Some(rest) = strip_ascii_prefix(trimmed, "BEGIN:") {
            let _ = rest;
            nested_depth += 1;
            continue;
        }
        if let Some(rest) = strip_ascii_prefix(trimmed, "END:") {
            let _ = rest;
            nested_depth = nested_depth.saturating_sub(1);
            continue;
        }
        if nested_depth > 0 {
            continue;
        }
        ev.apply_line(trimmed);
    }

    events
}

/// Export Canopy calendar events and optional milestones as an RFC 5545 `VCALENDAR`.
pub fn export_ics(events: &[CalendarEvent], milestones: &[CalendarMilestone]) -> String {
    let mut out = String::new();
    push_crlf(&mut out, "BEGIN:VCALENDAR");
    push_crlf(&mut out, "VERSION:2.0");
    push_crlf(&mut out, "PRODID:-//Canopy//Calendar//EN");
    push_crlf(&mut out, "CALSCALE:GREGORIAN");

    let dtstamp = format_ics_utc(Utc::now());

    for event in events {
        let Ok(at) = parse_rfc3339_utc(&event.at) else {
            continue;
        };
        push_crlf(&mut out, "BEGIN:VEVENT");
        push_crlf(&mut out, &format!("UID:{}", escape_ics_text(&event.id)));
        push_crlf(&mut out, &format!("DTSTAMP:{dtstamp}"));
        push_crlf(&mut out, &format!("DTSTART:{}", format_ics_utc(at)));
        push_crlf(
            &mut out,
            &format!("SUMMARY:{}", escape_ics_text(&event.title)),
        );
        if let Some(notes) = event.notes.as_deref().filter(|s| !s.trim().is_empty()) {
            push_crlf(&mut out, &format!("DESCRIPTION:{}", escape_ics_text(notes)));
        }
        if let Some(rrule) = recurrence_to_rrule(event.recurrence) {
            push_crlf(&mut out, &format!("RRULE:{rrule}"));
        }
        push_crlf(&mut out, "END:VEVENT");
    }

    for milestone in milestones {
        let Some(due_raw) = milestone.due_at.as_deref() else {
            continue;
        };
        let Ok(due_at) = parse_rfc3339_utc(due_raw) else {
            continue;
        };
        push_crlf(&mut out, "BEGIN:VEVENT");
        push_crlf(
            &mut out,
            &format!("UID:milestone-{}", escape_ics_text(&milestone.id)),
        );
        push_crlf(&mut out, &format!("DTSTAMP:{dtstamp}"));
        push_crlf(&mut out, &format!("DTSTART:{}", format_ics_utc(due_at)));
        push_crlf(
            &mut out,
            &format!("SUMMARY:{}", escape_ics_text(&milestone.title)),
        );
        let desc = match milestone.notes.as_deref().filter(|s| !s.trim().is_empty()) {
            Some(notes) => format!(
                "{notes} [status: {} | project: {}]",
                milestone.status, milestone.project_path
            ),
            None => format!(
                "status: {} | project: {}",
                milestone.status, milestone.project_path
            ),
        };
        push_crlf(&mut out, &format!("DESCRIPTION:{}", escape_ics_text(&desc)));
        push_crlf(&mut out, "END:VEVENT");
    }

    push_crlf(&mut out, "END:VCALENDAR");
    out
}

#[derive(Default)]
struct PartialVEvent {
    uid: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    dtstart: Option<DateTime<Utc>>,
    dtend: Option<DateTime<Utc>>,
    recurrence: Recurrence,
}

impl PartialVEvent {
    fn apply_line(&mut self, line: &str) {
        let Some((name_and_params, value)) = line.split_once(':') else {
            return;
        };
        let mut parts = name_and_params.split(';');
        let prop_name = parts.next().unwrap_or("").trim().to_ascii_uppercase();
        let params: Vec<&str> = parts.collect();

        match prop_name.as_str() {
            "UID" => {
                let v = value.trim();
                if !v.is_empty() {
                    self.uid = Some(v.to_string());
                }
            }
            "SUMMARY" => {
                let v = unescape_ics_text(value.trim());
                if !v.is_empty() {
                    self.summary = Some(v);
                }
            }
            "DESCRIPTION" => {
                let v = unescape_ics_text(value.trim());
                if !v.is_empty() {
                    self.description = Some(v);
                }
            }
            "DTSTART" => {
                self.dtstart = parse_ics_datetime(value.trim(), &params);
            }
            "DTEND" => {
                self.dtend = parse_ics_datetime(value.trim(), &params);
            }
            "RRULE" => {
                self.recurrence = parse_rrule(value.trim());
            }
            _ => {}
        }
    }

    fn finish(self) -> Option<IcsEvent> {
        Some(IcsEvent {
            uid: self.uid?,
            title: self.summary?,
            notes: self.description,
            at: self.dtstart?,
            dtend: self.dtend,
            recurrence: self.recurrence,
        })
    }
}

fn parse_ics_datetime(value: &str, params: &[&str]) -> Option<DateTime<Utc>> {
    let is_date_value = params
        .iter()
        .any(|p| p.trim().eq_ignore_ascii_case("VALUE=DATE"));
    let tzid = params.iter().find_map(|p| {
        let (k, v) = p.split_once('=')?;
        k.trim()
            .eq_ignore_ascii_case("TZID")
            .then(|| v.trim().trim_matches('"'))
    });

    if is_date_value || (value.len() == 8 && value.chars().all(|c| c.is_ascii_digit())) {
        let date = NaiveDate::parse_from_str(value, "%Y%m%d").ok()?;
        let ndt = date.and_hms_opt(0, 0, 0)?;
        return Some(if tzid.is_some() {
            resolve_tz_datetime(ndt, tzid)
        } else {
            Utc.from_utc_datetime(&ndt)
        });
    }

    if let Some(stripped) = value.strip_suffix('Z') {
        let ndt = NaiveDateTime::parse_from_str(stripped, "%Y%m%dT%H%M%S").ok()?;
        return Some(Utc.from_utc_datetime(&ndt));
    }

    let ndt = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    Some(resolve_tz_datetime(ndt, tzid))
}

fn resolve_tz_datetime(ndt: NaiveDateTime, tzid: Option<&str>) -> DateTime<Utc> {
    if let Some(tz_name) = tzid {
        if let Ok(tz) = tz_name.parse::<Tz>() {
            if let Some(dt) = tz.from_local_datetime(&ndt).earliest() {
                return dt.with_timezone(&Utc);
            }
        }
    }
    Local
        .from_local_datetime(&ndt)
        .earliest()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&ndt))
}

fn parse_rrule(value: &str) -> Recurrence {
    for part in value.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            if k.trim().eq_ignore_ascii_case("FREQ") {
                return match v.trim().to_ascii_uppercase().as_str() {
                    "DAILY" => Recurrence::Daily,
                    "WEEKLY" => Recurrence::Weekly,
                    "MONTHLY" => Recurrence::Monthly,
                    _ => Recurrence::None,
                };
            }
        }
    }
    Recurrence::None
}

fn recurrence_to_rrule(recurrence: Recurrence) -> Option<&'static str> {
    match recurrence {
        Recurrence::None => None,
        Recurrence::Daily => Some("FREQ=DAILY"),
        Recurrence::Weekly => Some("FREQ=WEEKLY"),
        Recurrence::Monthly => Some("FREQ=MONTHLY"),
    }
}

fn format_ics_utc(dt: DateTime<Utc>) -> String {
    dt.format("%Y%m%dT%H%M%SZ").to_string()
}

fn push_crlf(out: &mut String, line: &str) {
    out.push_str(line);
    out.push_str("\r\n");
}

fn escape_ics_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            other => out.push(other),
        }
    }
    out
}

fn unescape_ics_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some('\\') => out.push('\\'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn strip_ascii_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &s[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::calendar::MilestoneStatus;
    use chrono::Timelike;

    #[test]
    fn parse_ics_handles_folded_lines_and_all_dtstart_forms() {
        let raw = concat!(
            "BEGIN:VCALENDAR\r\n",
            "VERSION:2.0\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:ev-utc\r\n",
            "SUMMARY:Standup\r\n",
            "  Meeting\r\n",
            "DESCRIPTION:Line 1\\nLine 2\\, with comma\r\n",
            "\t and tab\r\n",
            "DTSTART:20261201T143000Z\r\n",
            "DTEND:20261201T150000Z\r\n",
            "RRULE:FREQ=DAILY;COUNT=5\r\n",
            "BEGIN:VALARM\r\n",
            "ACTION:DISPLAY\r\n",
            "DESCRIPTION:Alarm text should not overwrite\r\n",
            "END:VALARM\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:ev-tzid\r\n",
            "SUMMARY:NY Sync\r\n",
            "DTSTART;TZID=America/New_York:20261201T100000\r\n",
            "RRULE:FREQ=WEEKLY\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:ev-date\r\n",
            "SUMMARY:Release Day\r\n",
            "DTSTART;VALUE=DATE:20261215\r\n",
            "RRULE:FREQ=MONTHLY\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:malformed-no-dtstart\r\n",
            "SUMMARY:Broken\r\n",
            "END:VEVENT\r\n",
            "BEGIN:VEVENT\r\n",
            "SUMMARY:Missing UID\r\n",
            "DTSTART:20261201T100000Z\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );

        let parsed = parse_ics(raw);
        assert_eq!(parsed.len(), 3);

        assert_eq!(parsed[0].uid, "ev-utc");
        assert_eq!(parsed[0].title, "Standup Meeting");
        assert_eq!(
            parsed[0].notes.as_deref(),
            Some("Line 1\nLine 2, with comma and tab")
        );
        assert_eq!(
            parsed[0].at,
            Utc.with_ymd_and_hms(2026, 12, 1, 14, 30, 0).unwrap()
        );
        assert_eq!(
            parsed[0].dtend,
            Some(Utc.with_ymd_and_hms(2026, 12, 1, 15, 0, 0).unwrap())
        );
        assert_eq!(parsed[0].recurrence, Recurrence::Daily);

        assert_eq!(parsed[1].uid, "ev-tzid");
        // America/New_York in December is EST (UTC-5) -> 15:00Z
        assert_eq!(parsed[1].at.hour(), 15);
        assert_eq!(parsed[1].recurrence, Recurrence::Weekly);

        assert_eq!(parsed[2].uid, "ev-date");
        assert_eq!(
            parsed[2].at,
            Utc.with_ymd_and_hms(2026, 12, 15, 0, 0, 0).unwrap()
        );
        assert_eq!(parsed[2].recurrence, Recurrence::Monthly);
    }

    #[test]
    fn export_ics_roundtrips_events_and_milestones() {
        let events = vec![CalendarEvent {
            id: "ev-1".to_string(),
            title: "Design, Review; Q4".to_string(),
            notes: Some("Line A\nLine B".to_string()),
            at: "2026-12-01T10:00:00Z".to_string(),
            recurrence: Recurrence::Weekly,
            lead_minutes: 10,
            snoozed_until: None,
            dismissed_at: None,
            last_notified_at: None,
            created_at: "2026-11-27T00:00:00Z".to_string(),
            updated_at: "2026-11-27T00:00:00Z".to_string(),
        }];
        let milestones = vec![
            CalendarMilestone {
                id: "m-1".to_string(),
                project_path: "/tmp/proj".to_string(),
                title: "v1.0 Launch".to_string(),
                notes: Some("Ship it".to_string()),
                due_at: Some("2026-12-20T18:00:00Z".to_string()),
                status: MilestoneStatus::Active,
                created_at: "2026-11-27T00:00:00Z".to_string(),
                updated_at: "2026-11-27T00:00:00Z".to_string(),
            },
            CalendarMilestone {
                id: "m-nodate".to_string(),
                project_path: "/tmp/proj".to_string(),
                title: "Backlog".to_string(),
                notes: None,
                due_at: None,
                status: MilestoneStatus::Planned,
                created_at: "2026-11-27T00:00:00Z".to_string(),
                updated_at: "2026-11-27T00:00:00Z".to_string(),
            },
        ];

        let ics = export_ics(&events, &milestones);
        let parsed = parse_ics(&ics);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].uid, "ev-1");
        assert_eq!(parsed[0].title, "Design, Review; Q4");
        assert_eq!(parsed[0].notes.as_deref(), Some("Line A\nLine B"));
        assert_eq!(parsed[0].recurrence, Recurrence::Weekly);

        assert_eq!(parsed[1].uid, "milestone-m-1");
        assert_eq!(parsed[1].title, "v1.0 Launch");
        assert!(parsed[1]
            .notes
            .as_deref()
            .unwrap()
            .contains("Ship it [status: active | project: /tmp/proj]"));
    }
}
