//! Domain types and pure scheduling helpers for Canopy Calendar.

use anyhow::{anyhow, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Recurrence cadence for a personal calendar event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Recurrence {
    #[default]
    None,
    Daily,
    Weekly,
    Monthly,
}

impl Recurrence {
    /// Canonical lowercase string representation stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
}

impl fmt::Display for Recurrence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Recurrence {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            other => Err(anyhow!(
                "Invalid recurrence '{other}': expected one of none, daily, weekly, monthly"
            )),
        }
    }
}

/// Lifecycle status for a per-project roadmap milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MilestoneStatus {
    #[default]
    Planned,
    Active,
    Done,
}

impl MilestoneStatus {
    /// Canonical lowercase string representation stored in SQLite.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Done => "done",
        }
    }
}

impl fmt::Display for MilestoneStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MilestoneStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "planned" => Ok(Self::Planned),
            "active" => Ok(Self::Active),
            "done" => Ok(Self::Done),
            other => Err(anyhow!(
                "Invalid milestone status '{other}': expected one of planned, active, done"
            )),
        }
    }
}

/// A personal calendar event (meeting or reminder).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    pub notes: Option<String>,
    /// RFC 3339 UTC timestamp of the event's anchor occurrence.
    pub at: String,
    pub recurrence: Recurrence,
    pub lead_minutes: i64,
    pub snoozed_until: Option<String>,
    pub dismissed_at: Option<String>,
    pub last_notified_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A per-project internal roadmap milestone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarMilestone {
    pub id: String,
    pub project_path: String,
    pub title: String,
    pub notes: Option<String>,
    /// Optional RFC 3339 UTC due timestamp.
    pub due_at: Option<String>,
    pub status: MilestoneStatus,
    pub created_at: String,
    pub updated_at: String,
}

/// Parse an RFC 3339 timestamp string and convert it to UTC.
pub fn parse_rfc3339_utc(raw: &str) -> Result<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("Timestamp must not be empty"));
    }
    DateTime::parse_from_rfc3339(trimmed)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| anyhow!("Invalid RFC 3339 timestamp '{trimmed}': {e}"))
}

/// Validate that `lead_minutes` is non-negative.
pub fn validate_lead_minutes(lead_minutes: i64) -> Result<i64> {
    if lead_minutes < 0 {
        return Err(anyhow!(
            "lead_minutes must be non-negative, got {lead_minutes}"
        ));
    }
    Ok(lead_minutes)
}

/// Compute the alert timestamp for an occurrence at `at` with `lead_minutes` lead time.
#[allow(dead_code)]
pub fn alert_at(at: DateTime<Utc>, lead_minutes: i64) -> DateTime<Utc> {
    at - Duration::minutes(lead_minutes)
}

/// Compute the next occurrence strictly after `after` for an event anchored at `at`.
#[allow(dead_code)]
pub fn next_occurrence(
    at: DateTime<Utc>,
    recurrence: Recurrence,
    after: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    match recurrence {
        Recurrence::None => (at > after).then_some(at),
        Recurrence::Daily => Some(advance_fixed_step(at, Duration::days(1), after)),
        Recurrence::Weekly => Some(advance_fixed_step(at, Duration::days(7), after)),
        Recurrence::Monthly => Some(advance_monthly(at, after)),
    }
}

#[allow(dead_code)]
fn advance_fixed_step(at: DateTime<Utc>, step: Duration, after: DateTime<Utc>) -> DateTime<Utc> {
    if at > after {
        return at;
    }
    let step_secs = step.num_seconds();
    let elapsed_secs = (after - at).num_seconds();
    let jumps = (elapsed_secs / step_secs) + 1;
    let mut candidate = at + step * i32::try_from(jumps).unwrap_or(i32::MAX);
    while candidate <= after {
        candidate += step;
    }
    candidate
}

#[allow(dead_code)]
fn advance_monthly(at: DateTime<Utc>, after: DateTime<Utc>) -> DateTime<Utc> {
    if at > after {
        return at;
    }
    let anchor_day = at.day();
    let anchor_time = at.time();
    let base_months = at.year() * 12 + (at.month0() as i32);
    let after_months = after.year() * 12 + (after.month0() as i32);
    let mut offset = (after_months - base_months).max(0) as u32;

    loop {
        let total_months = base_months + offset as i32;
        let year = total_months.div_euclid(12);
        let month = (total_months.rem_euclid(12) + 1) as u32;
        let max_day = days_in_month(year, month);
        let day = anchor_day.min(max_day);
        let date = NaiveDate::from_ymd_opt(year, month, day)
            .expect("clamped year/month/day is always a valid date");
        let candidate = Utc.from_utc_datetime(&date.and_time(anchor_time));
        if candidate > after {
            return candidate;
        }
        offset += 1;
    }
}

#[allow(dead_code)]
fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    let next_first =
        NaiveDate::from_ymd_opt(next_year, next_month, 1).expect("valid first of next month");
    let this_last = next_first - Duration::days(1);
    this_last.day()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(s: &str) -> DateTime<Utc> {
        parse_rfc3339_utc(s).expect("valid test timestamp")
    }

    #[test]
    fn recurrence_and_status_roundtrip_and_reject_invalid() {
        assert_eq!(Recurrence::from_str("none").unwrap(), Recurrence::None);
        assert_eq!(Recurrence::from_str("DAILY").unwrap(), Recurrence::Daily);
        assert_eq!(Recurrence::from_str("weekly").unwrap(), Recurrence::Weekly);
        assert_eq!(
            Recurrence::from_str("monthly").unwrap(),
            Recurrence::Monthly
        );
        assert!(Recurrence::from_str("yearly").is_err());

        assert_eq!(
            MilestoneStatus::from_str("planned").unwrap(),
            MilestoneStatus::Planned
        );
        assert_eq!(
            MilestoneStatus::from_str("ACTIVE").unwrap(),
            MilestoneStatus::Active
        );
        assert_eq!(
            MilestoneStatus::from_str("done").unwrap(),
            MilestoneStatus::Done
        );
        assert!(MilestoneStatus::from_str("cancelled").is_err());
    }

    #[test]
    fn validation_helpers_reject_bad_inputs() {
        assert!(parse_rfc3339_utc("2026-11-27T10:00:00Z").is_ok());
        assert!(parse_rfc3339_utc("2026-11-27T10:00:00-05:00").is_ok());
        assert!(parse_rfc3339_utc("not-a-date").is_err());
        assert!(parse_rfc3339_utc("").is_err());

        assert_eq!(validate_lead_minutes(0).unwrap(), 0);
        assert_eq!(validate_lead_minutes(15).unwrap(), 15);
        assert!(validate_lead_minutes(-1).is_err());
    }

    #[test]
    fn alert_at_subtracts_lead_minutes() {
        let at = utc("2026-11-27T10:00:00Z");
        assert_eq!(alert_at(at, 10), utc("2026-11-27T09:50:00Z"));
        assert_eq!(alert_at(at, 0), at);
    }

    #[test]
    fn next_occurrence_none_daily_weekly() {
        let at = utc("2026-11-01T09:00:00Z");

        assert_eq!(
            next_occurrence(at, Recurrence::None, utc("2026-10-31T09:00:00Z")),
            Some(at)
        );
        assert_eq!(next_occurrence(at, Recurrence::None, at), None);

        assert_eq!(
            next_occurrence(at, Recurrence::Daily, at),
            Some(utc("2026-11-02T09:00:00Z"))
        );
        assert_eq!(
            next_occurrence(at, Recurrence::Daily, utc("2026-11-05T08:59:59Z")),
            Some(utc("2026-11-05T09:00:00Z"))
        );

        assert_eq!(
            next_occurrence(at, Recurrence::Weekly, at),
            Some(utc("2026-11-08T09:00:00Z"))
        );
        assert_eq!(
            next_occurrence(at, Recurrence::Weekly, utc("2026-11-10T00:00:00Z")),
            Some(utc("2026-11-15T09:00:00Z"))
        );
    }

    #[test]
    fn next_occurrence_monthly_clamps_month_end_leap_and_non_leap() {
        // Non-leap year: Jan 31 -> Feb 28 -> Mar 31
        let jan31_2025 = utc("2025-01-31T14:30:00Z");
        assert_eq!(
            next_occurrence(jan31_2025, Recurrence::Monthly, jan31_2025),
            Some(utc("2025-02-28T14:30:00Z"))
        );
        assert_eq!(
            next_occurrence(jan31_2025, Recurrence::Monthly, utc("2025-02-28T14:30:00Z")),
            Some(utc("2025-03-31T14:30:00Z"))
        );

        // Leap year: Jan 31 -> Feb 29 -> Mar 31
        let jan31_2024 = utc("2024-01-31T14:30:00Z");
        assert_eq!(
            next_occurrence(jan31_2024, Recurrence::Monthly, jan31_2024),
            Some(utc("2024-02-29T14:30:00Z"))
        );
        assert_eq!(
            next_occurrence(jan31_2024, Recurrence::Monthly, utc("2024-02-29T14:30:00Z")),
            Some(utc("2024-03-31T14:30:00Z"))
        );
    }
}
