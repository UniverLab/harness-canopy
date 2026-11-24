//! Hermetic tests for the pid-file belief rule and the direct-spawn
//! readiness wait: fake probes, fake clock, tempdirs only — no real process
//! is spawned and no `canopy` binary ever runs.

use super::*;
use std::cell::Cell;
use std::time::{Duration, Instant};

fn write_pid_file(dir: &std::path::Path, pid: u32) {
    std::fs::write(dir.join("daemon.pid"), pid.to_string()).unwrap();
}

fn alive_map(live: &[u32]) -> impl Fn(u32) -> bool + '_ {
    move |pid| live.contains(&pid)
}

#[test]
fn stale_pid_file_is_not_believed_so_start_removes_it() {
    let dir = tempfile::tempdir().unwrap();
    write_pid_file(dir.path(), 12345);
    // The pid is alive (reused by an unrelated program) but does not hold
    // the port: `None`, while the raw pid file is still there — exactly the
    // condition on which `handle_start` removes the file and continues.
    let live = pid_file_is_live(dir.path(), 7755, &alive_map(&[12345]), &|_| None);
    assert_eq!(live, None);
    assert_eq!(read_pid(dir.path()), Some(12345));
}

#[test]
fn reused_pid_holding_another_port_is_not_believed() {
    let dir = tempfile::tempdir().unwrap();
    write_pid_file(dir.path(), 12345);
    let live = pid_file_is_live(dir.path(), 7755, &alive_map(&[12345]), &|_| Some(9999));
    assert_eq!(live, None);
}

#[test]
fn live_occupant_equal_to_pid_file_is_believed() {
    let dir = tempfile::tempdir().unwrap();
    write_pid_file(dir.path(), 12345);
    let live = pid_file_is_live(dir.path(), 7755, &alive_map(&[12345]), &|_| Some(12345));
    assert_eq!(live, Some(12345));
}

#[test]
fn missing_pid_file_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let live = pid_file_is_live(dir.path(), 7755, &alive_map(&[12345]), &|_| Some(12345));
    assert_eq!(live, None);
}

#[test]
fn dead_pid_file_is_not_believed() {
    let dir = tempfile::tempdir().unwrap();
    write_pid_file(dir.path(), 12345);
    let live = pid_file_is_live(dir.path(), 7755, &alive_map(&[]), &|_| None);
    assert_eq!(live, None);
}

fn fake_clock() -> (Cell<Instant>, Cell<usize>) {
    (Cell::new(Instant::now()), Cell::new(0usize))
}

#[test]
fn wait_returns_ok_as_soon_as_port_opens() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(&log, "").unwrap();
    let (current, ticks) = fake_clock();
    let calls = Cell::new(0usize);
    let result = wait_until_ready(
        7755,
        4242,
        &log,
        0,
        &alive_map(&[4242]),
        &|_| {
            calls.set(calls.get() + 1);
            calls.get() >= 3
        },
        &|| {
            ticks.set(ticks.get() + 1);
            current.set(current.get() + Duration::from_millis(250));
        },
        &|| current.get(),
    );
    assert!(result.is_ok());
    assert_eq!(ticks.get(), 2);
}

#[test]
fn wait_reports_first_error_line_when_child_dies() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(
        &log,
        "starting up\nINFO listening soon\nERROR failed to bind port 7755\nmore context\n",
    )
    .unwrap();
    let err = wait_until_ready(
        7755,
        4242,
        &log,
        0,
        &alive_map(&[]),
        &|_| false,
        &|| {},
        &Instant::now,
    )
    .expect_err("dead child must fail the wait");
    let text = err.to_string();
    assert!(
        text.contains("daemon exited during start: ERROR failed to bind port 7755"),
        "unexpected message: {text}"
    );
    for line in [
        "starting up",
        "INFO listening soon",
        "ERROR failed to bind port 7755",
        "more context",
    ] {
        assert!(
            text.contains(&format!("\n  {line}")),
            "tail line missing, message was: {text}"
        );
    }
}

#[test]
fn wait_picks_panicked_line_over_plain_lines() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(
        &log,
        "starting up\nthread 'main' panicked at src/main.rs:1\n",
    )
    .unwrap();
    let err = wait_until_ready(
        7755,
        4242,
        &log,
        0,
        &alive_map(&[]),
        &|_| false,
        &|| {},
        &Instant::now,
    )
    .expect_err("dead child must fail the wait");
    assert!(
        err.to_string()
            .contains("daemon exited during start: thread 'main' panicked at src/main.rs:1"),
        "unexpected message: {err}"
    );
}

#[test]
fn wait_falls_back_to_last_non_empty_line() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(&log, "starting up\nshutting down\n\n").unwrap();
    let err = wait_until_ready(
        7755,
        4242,
        &log,
        0,
        &alive_map(&[]),
        &|_| false,
        &|| {},
        &Instant::now,
    )
    .expect_err("dead child must fail the wait");
    assert!(
        err.to_string()
            .contains("daemon exited during start: shutting down"),
        "unexpected message: {err}"
    );
}

#[test]
fn wait_reports_no_output_when_nothing_appended() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(&log, "old line from a previous run\n").unwrap();
    let base = std::fs::metadata(&log).unwrap().len();
    let err = wait_until_ready(
        7755,
        4242,
        &log,
        base,
        &alive_map(&[]),
        &|_| false,
        &|| {},
        &Instant::now,
    )
    .expect_err("dead child must fail the wait");
    let text = err.to_string();
    assert!(
        text.contains("daemon exited during start: no output"),
        "unexpected message: {text}"
    );
}

#[test]
fn wait_stops_at_once_when_child_dies() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(&log, "bye\n").unwrap();
    let ticks = Cell::new(0usize);
    let err = wait_until_ready(
        7755,
        4242,
        &log,
        0,
        &alive_map(&[]),
        &|_| false,
        &|| {
            ticks.set(ticks.get() + 1);
        },
        &Instant::now,
    )
    .expect_err("dead child must fail the wait");
    assert_eq!(ticks.get(), 0);
    assert!(err.to_string().contains("daemon exited during start:"));
}

#[test]
fn wait_times_out_after_20s_naming_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("daemon.log");
    std::fs::write(&log, "").unwrap();
    let (current, ticks) = fake_clock();
    let err = wait_until_ready(
        7755,
        4242,
        &log,
        0,
        &alive_map(&[4242]),
        &|_| false,
        &|| {
            ticks.set(ticks.get() + 1);
            current.set(current.get() + Duration::from_millis(250));
        },
        &|| current.get(),
    )
    .expect_err("a port that never opens must time out");
    let text = err.to_string();
    assert!(
        text.contains("daemon did not become ready in 20 s (log: "),
        "unexpected message: {text}"
    );
    assert!(
        text.contains(&log.display().to_string()),
        "log path missing from: {text}"
    );
    assert_eq!(ticks.get(), 80);
}

#[test]
fn stop_targets_never_include_an_unbelieved_pid() {
    // Stale file (pid reused elsewhere) plus a real occupant: only the
    // occupant may be signalled.
    assert_eq!(
        stop_targets(Some(1111), None, Some(2222), &alive_map(&[1111, 2222])),
        vec![2222]
    );
    // Stale file and nothing on the port: nobody to signal.
    assert_eq!(
        stop_targets(Some(1111), None, None, &alive_map(&[1111])),
        Vec::<u32>::new()
    );
    // Believed pid that also holds the port: signalled once, not twice.
    assert_eq!(
        stop_targets(Some(1111), Some(1111), Some(1111), &alive_map(&[1111])),
        vec![1111]
    );
    // Believed pid plus a different live occupant: both are signalled.
    assert_eq!(
        stop_targets(
            Some(1111),
            Some(1111),
            Some(2222),
            &alive_map(&[1111, 2222])
        ),
        vec![1111, 2222]
    );
    // A believed pid that disagrees with the pid file is never trusted.
    assert_eq!(
        stop_targets(
            Some(1111),
            Some(3333),
            Some(2222),
            &alive_map(&[2222, 3333])
        ),
        vec![2222]
    );
}
