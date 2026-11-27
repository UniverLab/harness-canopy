//! Pure cron-schedule math shared by agents and cron graphs.
//!
//! Converting next-fire instants into sleep durations, folding candidate
//! fire times, and resolving due ticks — all timezone-aware (cron
//! expressions are authored in the user's local wall clock).

use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Local, Utc};
use cron::Schedule;

/// Convert a next-fire instant into the sleep duration until it: `ZERO` for
/// a time already passed, otherwise the positive delta in milliseconds.
pub(super) fn earliest_duration_before(now_utc: DateTime<Utc>, next: DateTime<Utc>) -> Duration {
    let delta = next.signed_duration_since(now_utc);
    if delta.num_milliseconds() <= 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(delta.num_milliseconds() as u64)
    }
}

/// Fold a one-shot schedule instant into `earliest` when it is nearer than
/// whatever is there now (`None` counts as "nearer" — first one wins).
pub(super) fn fold_earliest_instant(
    earliest: &mut Option<DateTime<Utc>>,
    instant: Option<DateTime<Utc>>,
) {
    let Some(instant) = instant else {
        return;
    };
    let nearer = match earliest {
        Some(e) => instant < *e,
        None => true,
    };
    if nearer {
        *earliest = Some(instant);
    }
}

/// Legacy agent due-tick math (`agent_due_tick`) is deleted: agents fire
/// through their backing graphs since the agent-to-graph migration, and the
/// graph tick resolves due ticks from the graph's own schedule. The shared
/// `due_fire_local` helper it wrapped stays (graph path + unit tests).
/// Compute the next fire instant (in UTC) for a cron schedule, evaluated
/// against the user's local wall clock.
///
/// Cron expressions are authored in the user's local timezone (someone who
/// types `0 9 * * *` expects 9 AM on their wall clock, not 9 AM UTC), so we
/// feed the schedule iterator a `Local` reference and only convert the
/// resulting instant to UTC for wall-clock-independent delta math. Returns
/// `None` if the schedule has no future occurrence.
pub(super) fn next_fire_utc(
    schedule: &Schedule,
    now_local: DateTime<Local>,
) -> Option<DateTime<Utc>> {
    schedule
        .after(&now_local)
        .next()
        .map(|next_local| next_local.with_timezone(&Utc))
}

/// Compute the next fire instant (in UTC) for a cron expression string,
/// evaluated against `now_local`. Returns `None` if unparseable or no future
/// occurrence exists.
pub(crate) fn next_cron_occurrence_utc(
    schedule_expr: &str,
    now_local: DateTime<Local>,
) -> Option<DateTime<Utc>> {
    let schedule = Schedule::from_str(&to_7field_cron(schedule_expr)).ok()?;
    next_fire_utc(&schedule, now_local)
}

/// Parse `schedule_expr` and, if it yields a nearer next fire than `earliest`,
/// update `earliest`. A `None` expression or an unparseable one is skipped.
pub(super) fn fold_earliest(
    earliest: &mut Option<DateTime<Utc>>,
    schedule_expr: Option<&str>,
    now_local: DateTime<Local>,
) {
    let Some(schedule_expr) = schedule_expr else {
        return;
    };
    let Ok(schedule) = Schedule::from_str(&to_7field_cron(schedule_expr)) else {
        return;
    };
    if let Some(next_utc) = next_fire_utc(&schedule, now_local) {
        let nearer = match earliest {
            Some(e) => next_utc < *e,
            None => true,
        };
        if nearer {
            *earliest = Some(next_utc);
        }
    }
}

/// Decide whether a cron schedule is due at `now_local`, using a 60-second
/// lookback so a scheduler hiccup doesn't skip a fire scheduled just before
/// "now". Returns the matched fire time (in local wall-clock time) when due,
/// or `None` otherwise.
///
/// Like [`next_fire_utc`], the schedule is evaluated in the local frame so
/// the cron fields mean local wall-clock times.
pub(super) fn due_fire_local(
    schedule: &Schedule,
    now_local: DateTime<Local>,
) -> Option<DateTime<Local>> {
    let window_start = now_local - chrono::Duration::seconds(60);
    let candidate = schedule.after(&window_start).next()?;
    (candidate <= now_local).then_some(candidate)
}

/// Convert a standard 5-field cron expression to the 7-field format
/// expected by the `cron` crate: `sec min hour day month dow year`.
///
/// Input:  `*/5 * * * *`       (min hour day month dow)
/// Output: `0 */5 * * * * *`   (sec min hour day month dow year)
pub(super) fn to_7field_cron(expr: &str) -> String {
    format!("0 {} *", expr.trim())
}
