//! Part 02 of `tests` (split from `src/scheduler/cron_scheduler.rs`, pure move).
use super::*;

#[test]
fn test_run_outcome_is_failure() {
    assert!(!run_outcome_is_failure(&Ok(0)), "clean exit is success");
    assert!(run_outcome_is_failure(&Ok(1)), "non-zero exit is failure");
    assert!(
        run_outcome_is_failure(&Err(anyhow::anyhow!("spawn failed"))),
        "spawn error is failure"
    );
}

#[test]
fn test_should_retry_respects_enabled_and_cap() {
    let on = RetryPolicy {
        enabled: true,
        delay_minutes: 60,
        max_retries: 3,
    };
    // attempts 0,1,2 retry; the 3rd failed attempt (index 3) does not.
    assert!(should_retry(&on, 0));
    assert!(should_retry(&on, 2));
    assert!(!should_retry(&on, 3));

    let off = RetryPolicy {
        enabled: false,
        ..on
    };
    assert!(!should_retry(&off, 0), "disabled policy never retries");
}

#[test]
fn test_cron_parse_after_conversion() {
    let cases = vec![
        "*/5 * * * *",    // every 5 min
        "0 9 * * *",      // daily at 9am
        "0 9 * * 1-5",    // weekdays at 9am
        "30 14 1,15 * *", // 1st and 15th at 2:30pm
    ];

    for expr in cases {
        let converted = to_7field_cron(expr);
        let result = Schedule::from_str(&converted);
        assert!(
            result.is_ok(),
            "Failed to parse '{}' -> '{}': {:?}",
            expr,
            converted,
            result.err()
        );
    }
}

#[test]
fn test_to_7field_cron_trims_whitespace() {
    assert_eq!(to_7field_cron("  */5 * * * *  "), "0 */5 * * * * *");
    assert_eq!(to_7field_cron("\t0 9 * * *\t"), "0 0 9 * * * *");
}

#[test]
fn test_cron_schedule_next_fire_time() {
    let converted = to_7field_cron("* * * * *");
    let schedule = Schedule::from_str(&converted).unwrap();
    let now = chrono::Utc::now();
    let next = schedule.after(&now).next();
    assert!(next.is_some());
}

/// The schedule iterator interprets cron fields in the timezone of the
/// "now" reference. We feed it a `Local` "now" so user-authored cron
/// expressions like `0 9 * * *` mean "9 AM on the user's wall clock",
/// not 9 AM UTC. This test verifies the field interpretation by
/// comparing local vs UTC.
#[test]
fn test_cron_field_uses_local_timezone() {
    use chrono::Timelike;
    let converted = to_7field_cron("0 9 * * *");
    let schedule = Schedule::from_str(&converted).unwrap();
    let now_local = chrono::Local::now();
    let next_local = schedule.after(&now_local).next().expect("next fire time");
    // The hour field of the *local* fire time must be 9 — that's the
    // whole point of evaluating against a Local reference.
    assert_eq!(next_local.hour(), 9);
    // And the local hour must differ from the UTC hour whenever the
    // system isn't in UTC, otherwise the test isn't proving anything.
    // (Skip the assertion in the rare case the test runs in UTC, e.g.
    // CI on a server with TZ=UTC.)
    let next_utc = next_local.with_timezone(&chrono::Utc);
    if chrono::Local::now().offset().local_minus_utc() != 0 {
        assert_ne!(
            next_local.hour(),
            next_utc.hour(),
            "local hour and UTC hour are equal — the scheduler would be \
                 treating cron fields as UTC, which is the bug we are guarding against"
        );
    }
}

/// `next_fire_utc` must land the fire at the local wall-clock time named
/// in the cron expression. For `30 8 * * *` the next fire, converted back
/// to local, must read 08:30 — regardless of the machine's UTC offset.
#[test]
fn test_next_fire_utc_lands_at_local_wall_clock() {
    use chrono::Timelike;
    let schedule = Schedule::from_str(&to_7field_cron("30 8 * * *")).unwrap();
    let next_utc = next_fire_utc(&schedule, chrono::Local::now()).expect("next fire time");
    let next_local = next_utc.with_timezone(&chrono::Local);
    assert_eq!(next_local.hour(), 8, "fire must be at 08:xx local");
    assert_eq!(next_local.minute(), 30, "fire must be at xx:30 local");
}

/// A cron graph shares the agents' fire math: `fold_earliest` on a graph's
/// schedule lands the next fire at the local wall-clock time the expression
/// names (08:30 local for `30 8 * * *`), not 08:30 UTC.
#[test]
fn fold_earliest_lands_graph_cron_at_local_wall_clock() {
    use chrono::Timelike;
    let mut earliest: Option<chrono::DateTime<Utc>> = None;
    fold_earliest(&mut earliest, Some("30 8 * * *"), chrono::Local::now());
    let next_local = earliest
        .expect("cron graph yields a fire time")
        .with_timezone(&Local);
    assert_eq!(next_local.hour(), 8, "graph fire must be at 08:xx local");
    assert_eq!(next_local.minute(), 30, "graph fire must be at xx:30 local");
}

/// A manual graph (no schedule) never contributes a fire time, so the
/// scheduler never launches it on its own — it only runs via `graph_control`.
#[test]
fn fold_earliest_ignores_manual_graph() {
    let mut earliest: Option<chrono::DateTime<Utc>> = Some(Utc::now());
    let before = earliest;
    fold_earliest(&mut earliest, None, chrono::Local::now());
    assert_eq!(
        earliest, before,
        "a manual graph must not change the nearest fire time"
    );
}

#[test]
fn graph_key_namespaces_ids() {
    assert_eq!(graph_key("abc"), "graph:abc");
}

/// `due_fire_local` fires within the local minute the cron field names and
/// only within the 60-second lookback window — never for a future minute.
#[test]
fn test_due_fire_local_window() {
    use chrono::{TimeZone, Timelike};
    let schedule = Schedule::from_str(&to_7field_cron("30 8 * * *")).unwrap();

    // A few seconds past 08:30 local → due (matched fire is 08:30 local).
    let just_after = Local.with_ymd_and_hms(2026, 7, 3, 8, 30, 20).unwrap();
    let fired = due_fire_local(&schedule, just_after).expect("should be due");
    assert_eq!(
        (fired.hour(), fired.minute()),
        (8, 30),
        "matched fire must be the 08:30 local occurrence"
    );

    // 08:00 local → the next occurrence after 07:59 is 08:30, which is in
    // the future, so it must not be due yet.
    let before = Local.with_ymd_and_hms(2026, 7, 3, 8, 0, 0).unwrap();
    assert!(
        due_fire_local(&schedule, before).is_none(),
        "08:00 must not fire the 08:30 schedule"
    );

    // Just over a minute past 08:30 → outside the lookback window; the
    // next occurrence after 08:30:30 is tomorrow's 08:30, in the future.
    let stale = Local.with_ymd_and_hms(2026, 7, 3, 8, 31, 30).unwrap();
    assert!(
        due_fire_local(&schedule, stale).is_none(),
        "08:31:30 is past the 60s lookback and must not re-fire"
    );
}

/// B15: booting the scheduler and then evaluating the same cron tick a
/// second time (simulating a second firing path racing the regular tick
/// graph) must record exactly one execution for that tick, never two.
///
/// Uses an every-minute schedule: thanks to `due_fire_local`'s 60-second
/// lookback, the most recent minute boundary is *always* due at the
/// instant this test runs, so there's no need to align to (or wait for)
/// a real cron boundary. `start_paused` means the scheduler's internal
/// `tokio::time::sleep` for its first tick is advanced virtually — the
/// test performs no real wall-clock sleep.
#[tokio::test(start_paused = true)]
async fn scheduler_fires_a_cron_agent_at_most_once_per_tick() {
    let (db, scheduler) = test_scheduler();
    let mut agent = manual_agent("b15-once-per-tick", true);
    agent.trigger = Some(Trigger::Cron {
        schedule_expr: "* * * * *".to_string(),
    });
    // A binary that can't resolve: `run_cli_process` fails fast without
    // depending on any real external CLI, but the executor still runs
    // its full start-run/finalize-run path and records the run row —
    // all this test needs to count fires.
    agent.cli = Cli::new("definitely-not-a-real-cli-binary-b15");
    db.upsert_agent(&agent).unwrap();

    let scheduler = Arc::new(scheduler);
    let _cancel = Arc::clone(&scheduler).start();

    // Advance the virtual clock past the scheduler's first computed
    // sleep (at most ~60s until the next minute boundary) in small
    // steps, yielding between each so the background `run_graph` task
    // actually gets polled and reaches its own `fire_due_tasks` call —
    // a single large `advance` only fast-forwards the clock, it doesn't
    // by itself guarantee the woken task has run before this test task
    // continues.
    for _ in 0..200 {
        tokio::time::advance(Duration::from_millis(500)).await;
        tokio::task::yield_now().await;
        if !db.list_runs("b15-once-per-tick", 10).unwrap().is_empty() {
            break;
        }
    }
    assert!(
        !db.list_runs("b15-once-per-tick", 10).unwrap().is_empty(),
        "the scheduler's own tick graph never fired the due agent"
    );

    // Simulate a second evaluation path for the same tick (e.g. a
    // concurrent reconcile) landing right on top of the regular tick.
    scheduler.fire_due_tasks().await.unwrap();

    // Let the spawned executions (detached tokio tasks) record their run
    // rows on the paused-clock executor. Waits for the recorded count to
    // go quiet rather than stopping at the first row seen — stopping
    // early would miss a second spawn still in flight and turn this
    // into a false-negative regression test.
    let mut runs = db.list_runs("b15-once-per-tick", 10).unwrap();
    let mut quiet_iters = 0;
    for _ in 0..500 {
        tokio::task::yield_now().await;
        let current = db.list_runs("b15-once-per-tick", 10).unwrap();
        if current.len() == runs.len() {
            quiet_iters += 1;
            if quiet_iters >= 20 {
                break;
            }
        } else {
            quiet_iters = 0;
        }
        runs = current;
    }

    assert_eq!(
        runs.len(),
        1,
        "the same tick must fire at most once, got {} run(s): {:?}",
        runs.len(),
        runs
    );
}

// ── Coverage for helpers extracted during the quality refactor ──────

#[test]
fn fold_earliest_instant_sets_first_seen() {
    let mut earliest: Option<chrono::DateTime<Utc>> = None;
    let first = chrono::DateTime::from_timestamp(1_000_000, 0).unwrap();
    fold_earliest_instant(&mut earliest, Some(first));
    assert_eq!(earliest, Some(first));
}

#[test]
fn fold_earliest_instant_keeps_nearer_existing() {
    let earlier = chrono::DateTime::from_timestamp(1_000_000, 0).unwrap();
    let later = chrono::DateTime::from_timestamp(2_000_000, 0).unwrap();
    let mut earliest = Some(later);

    fold_earliest_instant(&mut earliest, Some(earlier));
    assert_eq!(earliest, Some(earlier));

    // A farther instant must not replace the nearer one already folded in.
    let much_later = chrono::DateTime::from_timestamp(3_000_000, 0).unwrap();
    fold_earliest_instant(&mut earliest, Some(much_later));
    assert_eq!(earliest, Some(earlier));
}

#[test]
fn fold_earliest_instant_ignores_none() {
    let mut earliest: Option<chrono::DateTime<Utc>> = None;
    fold_earliest_instant(&mut earliest, None);
    assert!(earliest.is_none());

    let known = chrono::DateTime::from_timestamp(1_000_000, 0).unwrap();
    earliest = Some(known);
    fold_earliest_instant(&mut earliest, None);
    assert_eq!(earliest, Some(known));
}

#[tokio::test]
async fn wait_retry_delay_returns_true_after_zero_delay() {
    let retry = RetryPolicy {
        enabled: true,
        delay_minutes: 0,
        max_retries: 1,
    };
    let cancel = CancellationToken::new();
    let agent = manual_agent("retry-wait-zero-delay", true);
    assert!(wait_retry_delay(&retry, &cancel, &agent).await);
}

#[tokio::test]
async fn wait_retry_delay_returns_false_when_cancelled() {
    let retry = RetryPolicy {
        enabled: true,
        delay_minutes: 5,
        max_retries: 1,
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let agent = manual_agent("retry-wait-cancelled", true);
    assert!(!wait_retry_delay(&retry, &cancel, &agent).await);
}

#[test]
fn agent_due_tick_invalid_cron_is_none() {
    let mut agent = manual_agent("due-tick-invalid-cron", true);
    agent.trigger = Some(Trigger::Cron {
        schedule_expr: "not a cron expression".to_string(),
    });
    assert!(agent_due_tick(&agent, Local::now()).is_none());
}

#[test]
fn agent_due_tick_returns_tick_when_due() {
    let mut agent = manual_agent("due-tick-minutely", true);
    agent.trigger = Some(Trigger::Cron {
        schedule_expr: "* * * * *".to_string(),
    });
    assert!(agent_due_tick(&agent, Local::now()).is_some());
}
