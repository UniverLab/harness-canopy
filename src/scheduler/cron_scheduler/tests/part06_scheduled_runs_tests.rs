//! Tests for one-shot `graph_scheduled_runs` evaluation and wakeup folding in `CronScheduler`.

use super::*;
use crate::db::graph_scheduled_runs::GraphScheduledRun;
use crate::domain::graphs::GraphStatus;
use chrono::Duration as ChronoDuration;

fn insert_scheduled_run(
    db: &Database,
    id: &str,
    graph_id: &str,
    fire_at: chrono::DateTime<Utc>,
    on_miss: &str,
) {
    db.insert_graph_scheduled_run(&GraphScheduledRun {
        id: id.to_string(),
        graph_id: graph_id.to_string(),
        fire_at: fire_at.to_rfc3339(),
        idea: Some("one-shot idea".to_string()),
        queue_id: None,
        workdir: None,
        on_miss: on_miss.to_string(),
        status: "pending".to_string(),
        created_at: Utc::now().to_rfc3339(),
        fired_at: None,
        error: None,
    })
    .unwrap();
}

#[tokio::test]
async fn scheduled_run_on_time_fires_and_marks_fired() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());
    let mut g = sample_graph("g-ontime", GraphStatus::Draft);
    g.workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph(&g).unwrap();

    let now = Utc::now();
    insert_scheduled_run(
        &db,
        "sr-ontime",
        "g-ontime",
        now - ChronoDuration::seconds(10),
        "skip",
    );

    scheduler.fire_due_scheduled_runs(now).unwrap();

    let row = db.get_graph_scheduled_run("sr-ontime").unwrap().unwrap();
    assert_eq!(row.status, "fired");
    assert!(row.fired_at.is_some());
    assert!(row.error.is_none());
}

#[tokio::test]
async fn scheduled_run_stale_skip_marks_missed_without_launching() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let g = sample_graph("g-stale-skip", GraphStatus::Draft);
    db.insert_graph(&g).unwrap();

    let now = Utc::now();
    insert_scheduled_run(
        &db,
        "sr-stale-skip",
        "g-stale-skip",
        now - ChronoDuration::seconds(120),
        "skip",
    );

    scheduler.fire_due_scheduled_runs(now).unwrap();

    let row = db
        .get_graph_scheduled_run("sr-stale-skip")
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "missed");
    assert!(row.fired_at.is_none());
    let graph_after = db.get_graph("g-stale-skip").unwrap().unwrap();
    assert_eq!(graph_after.status, GraphStatus::Draft);
}

#[tokio::test]
async fn scheduled_run_stale_run_launches_and_marks_fired() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let dir = tempfile::tempdir().unwrap();
    init_git_repo(dir.path());
    let mut g = sample_graph("g-stale-run", GraphStatus::Draft);
    g.workdir = dir.path().to_string_lossy().to_string();
    db.insert_graph(&g).unwrap();

    let now = Utc::now();
    insert_scheduled_run(
        &db,
        "sr-stale-run",
        "g-stale-run",
        now - ChronoDuration::seconds(120),
        "run",
    );

    scheduler.fire_due_scheduled_runs(now).unwrap();

    let row = db.get_graph_scheduled_run("sr-stale-run").unwrap().unwrap();
    assert_eq!(row.status, "fired");
    assert!(row.fired_at.is_some());
}

#[test]
fn next_sleep_duration_includes_earliest_pending_scheduled_run() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let g = sample_graph("g-wakeup", GraphStatus::Draft);
    db.insert_graph(&g).unwrap();

    let now = Utc::now();
    insert_scheduled_run(
        &db,
        "sr-later",
        "g-wakeup",
        now + ChronoDuration::seconds(40),
        "skip",
    );
    insert_scheduled_run(
        &db,
        "sr-sooner",
        "g-wakeup",
        now + ChronoDuration::seconds(15),
        "skip",
    );

    let dur = scheduler.next_sleep_duration();
    assert!(
        dur <= std::time::Duration::from_secs(16),
        "expected sleep duration <= 16s for 15s-away scheduled run, got {:?}",
        dur
    );
}

#[tokio::test]
async fn scheduled_run_records_error_when_graph_is_completed() {
    let (db, scheduler) = test_scheduler_with_graphs();
    let g = sample_graph("g-completed", GraphStatus::Completed);
    db.insert_graph(&g).unwrap();

    let now = Utc::now();
    insert_scheduled_run(
        &db,
        "sr-completed",
        "g-completed",
        now - ChronoDuration::seconds(5),
        "skip",
    );

    scheduler.fire_due_scheduled_runs(now).unwrap();

    let row = db.get_graph_scheduled_run("sr-completed").unwrap().unwrap();
    assert_eq!(row.status, "error");
    assert!(row.fired_at.is_none());
    assert!(row.error.is_some());
}
