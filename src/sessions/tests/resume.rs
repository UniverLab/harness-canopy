//! Startup reconciliation: the daemon resumes the sessions its previous run
//! left `active`. Rules moved here from the TUI unchanged, plus the resume
//! path end to end: id/name kept, bridges skipped, terminals open clean.

use super::*;
use crate::sessions::resume::{
    process_is_alive, process_matches_cli, process_outlives_grace, reconcile_at_startup,
    resume_decision, should_resume_session, ResumeDecision,
};
use crate::sessions::{SessionKind, SpawnRequest};

// ── the resume rules the TUI used to own ────────────────────────────

#[test]
fn test_process_is_alive_for_own_pid() {
    assert!(process_is_alive(std::process::id() as i64));
}

#[test]
fn test_process_is_alive_false_for_implausible_pid() {
    assert!(!process_is_alive(999_999_999));
}

#[test]
fn test_should_resume_session_with_no_pid_always_resumes() {
    let current = crate::system::boot_id();
    assert!(should_resume_session(
        None,
        current.as_deref(),
        current.as_deref()
    ));
}

#[test]
fn test_should_resume_session_skips_when_owner_process_is_alive() {
    let current = crate::system::boot_id();
    assert!(!should_resume_session(
        Some(std::process::id() as i64),
        current.as_deref(),
        current.as_deref()
    ));
}

#[test]
fn test_should_resume_session_resumes_when_pid_is_gone() {
    let current = crate::system::boot_id();
    assert!(should_resume_session(
        Some(999_999_999),
        current.as_deref(),
        current.as_deref()
    ));
}

#[test]
fn test_should_resume_session_resumes_on_boot_id_mismatch_even_with_live_pid() {
    let current = crate::system::boot_id();
    // Stored boot_id differs from current → PID is meaningless, always resume.
    assert!(should_resume_session(
        Some(std::process::id() as i64),
        Some("old-boot-id-from-previous-reboot"),
        current.as_deref(),
    ));
}

#[test]
fn test_should_resume_session_resumes_when_stored_boot_id_is_null() {
    let current = crate::system::boot_id();
    // Legacy row with NULL boot_id → always resume.
    assert!(should_resume_session(
        Some(std::process::id() as i64),
        None,
        current.as_deref(),
    ));
}

#[test]
fn test_should_resume_session_resumes_when_current_boot_id_is_none() {
    // Non-Linux host where boot_id can't be read → always resume.
    assert!(should_resume_session(
        Some(std::process::id() as i64),
        Some("some-stored-boot-id"),
        None,
    ));
}

#[test]
fn test_resume_decision_skips_when_pid_alive_same_boot_and_outlives_grace() {
    // The only genuine conflict: a live PID on this boot that survives
    // the full grace window — some other process still holds the lock.
    assert_eq!(
        resume_decision(true, true),
        ResumeDecision::SkipHeldByLiveProcess
    );
}

#[test]
fn test_resume_decision_resumes_when_pid_alive_same_boot_but_grace_expires() {
    // Live PID, same boot, but it didn't survive the grace window: mid-death
    // or recycled, not a real holder.
    assert_eq!(resume_decision(true, false), ResumeDecision::Resume);
}

#[test]
fn test_resume_decision_resumes_when_pid_not_alive_same_boot() {
    assert_eq!(resume_decision(false, false), ResumeDecision::Resume);
}

#[test]
fn test_resume_decision_resumes_when_pid_not_alive_same_boot_even_if_grace_flag_set() {
    // outlives_grace is meaningless when the PID isn't the live-same-boot
    // case in the first place; pid_alive_same_boot alone must gate it.
    assert_eq!(resume_decision(false, true), ResumeDecision::Resume);
}

#[cfg(target_os = "linux")]
#[test]
fn test_process_matches_cli_detects_recycled_pids() {
    // Our own PID is alive but its comm is the test binary, not "claude" —
    // exactly the recycled-PID case that must NOT orphan a session.
    let own_pid = std::process::id() as i64;
    assert!(!process_matches_cli(own_pid, "claude"));

    // And it does match its own real comm.
    let own_comm = std::fs::read_to_string(format!("/proc/{own_pid}/comm"))
        .expect("read own comm")
        .trim()
        .to_string();
    assert!(process_matches_cli(own_pid, &own_comm));
}

#[test]
fn test_process_outlives_grace_false_for_dead_or_recycled_pids() {
    // A PID nothing owns: resume immediately, no grace wait.
    assert!(!process_outlives_grace(-1, "claude"));
    // A live PID whose comm is another binary (recycled): also no wait.
    assert!(!process_outlives_grace(std::process::id() as i64, "claude"));
}

#[test]
fn test_process_outlives_grace_waits_out_a_dying_process() {
    // A child that exits shortly after we check: the grace graph must
    // observe the death and report "no conflict" instead of orphaning.
    // The child is reaped on a side thread — an unreaped zombie would
    // still answer kill(pid, 0). (In production the contended PID never
    // belongs to a child of the new daemon, so there is no zombie window.)
    let mut child = std::process::Command::new("sleep")
        .arg("0.3")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id() as i64;
    let reaper = std::thread::spawn(move || {
        let _ = child.wait();
    });
    assert!(!process_outlives_grace(pid, "sleep"));
    reaper.join().expect("join reaper");
}

// ── reconciliation against the database ─────────────────────────────

#[test]
fn reconcile_resumes_active_row_and_keeps_its_id_and_name() {
    let (db, dir) = test_db();
    db.insert_interactive_session(
        "resume-me",
        "kept-name",
        "sh",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, dir.path());

    let resumed = hub
        .get("resume-me")
        .expect("an active row with no live PTY is resumed");
    assert_eq!(
        resumed.name, "kept-name",
        "the resumed session keeps its name"
    );
    assert_eq!(resumed.kind, SessionKind::Agent);
    assert_eq!(
        db.get_interactive_session_status("resume-me")
            .unwrap()
            .as_deref(),
        Some("active"),
        "the row stays active under the same id"
    );
    let _ = hub.kill("resume-me");
}

#[test]
fn reconcile_does_not_resume_bridge_rows() {
    let (db, dir) = test_db();
    db.insert_interactive_session(
        "bridge-row",
        "standalone",
        "bridge",
        "/tmp",
        Some("canopy bridge"),
        None,
        "bridge",
        None,
    )
    .unwrap();

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, dir.path());

    assert!(
        hub.get("bridge-row").is_none(),
        "bridge sidecars are proxies, never resumed as chat sessions"
    );
    assert!(hub.list().is_empty());
}

#[test]
fn reconcile_preserves_the_harness_conversation_id() {
    let (db, dir) = test_db();
    db.insert_interactive_session(
        "resume-harness",
        "harness-name",
        "sh",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.set_session_harness_conversation_id("resume-harness", "conv-abc")
        .unwrap();

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, dir.path());

    assert!(
        hub.get("resume-harness").is_some(),
        "the active row is resumed"
    );
    // The hub's spawn re-records the row; the harness identifier the
    // previous daemon stored must survive so a later registration for the
    // same harness conversation still resolves to this id and name.
    let row = db
        .get_hookable_session("resume-harness")
        .expect("read resumed row")
        .expect("resumed row exists");
    assert_eq!(row.harness_conversation_id.as_deref(), Some("conv-abc"));
    assert_eq!(
        db.find_session_by_harness_id("conv-abc")
            .expect("lookup by harness id")
            .map(|s| s.id),
        Some("resume-harness".to_string())
    );
    let _ = hub.kill("resume-harness");
}

#[test]
fn reconcile_sweeps_orphaned_rows_to_completed() {
    let (db, dir) = test_db();
    db.insert_interactive_session(
        "orphan-sess",
        "orphan",
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
    db.mark_session_orphaned("orphan-sess").unwrap();
    assert_eq!(db.get_orphaned_sessions().unwrap().len(), 1);

    let hub = test_hub(&db);
    reconcile_at_startup(&hub, dir.path());

    assert!(db.get_orphaned_sessions().unwrap().is_empty());
    assert_eq!(
        db.get_interactive_session_status("orphan-sess")
            .unwrap()
            .as_deref(),
        Some("completed")
    );
}

#[test]
fn reconcile_with_no_active_sessions_starts_nothing() {
    let (db, dir) = test_db();
    let hub = test_hub(&db);

    reconcile_at_startup(&hub, dir.path());

    assert!(
        hub.list().is_empty(),
        "nothing to resume: no PTY is spawned"
    );
}

// ── terminals open clean across a hub restart ──────────────────────

#[test]
fn terminal_opens_clean_after_a_hub_restart() {
    let (db, dir) = test_db();
    let hub = test_hub(&db);
    let request = SpawnRequest {
        id: Some("term-hist".to_string()),
        name: Some("hist-shell".to_string()),
        ..terminal_spawn("sh", "/tmp")
    };
    let session = hub.spawn(&request).expect("spawn terminal");
    session
        .write_to_pty(b"echo canopy-history-probe\n")
        .expect("write to shell");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let contents = session.test_contents().unwrap_or_default();
        // Wait for the command's own output line, proving the pre-restart
        // session really produced output — output the respawn must NOT show.
        if contents
            .lines()
            .any(|line| line.trim() == "canopy-history-probe")
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "shell output never reached the parser: {contents:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // A legacy history.toml still carrying stored scrollback: the respawn
    // must ignore it. Without this seed the test below would pass even if
    // replay-on-resume were reintroduced, since nothing is stored anymore.
    let legacy_dir = dir.path().join("terminals").join("hist-shell");
    std::fs::create_dir_all(&legacy_dir).expect("create session dir");
    std::fs::write(
        legacy_dir.join("history.toml"),
        "scrollback = [\"canopy-legacy-scrollback-marker\"]\n",
    )
    .expect("write legacy history");

    // A restart: the PTY is gone, the row is what the previous daemon left.
    let _ = hub.kill("term-hist");
    drop(hub);
    db.insert_terminal_session("term-hist", "hist-shell", "sh", "/tmp")
        .unwrap();

    let restarted = test_hub(&db);
    reconcile_at_startup(&restarted, dir.path());

    let resumed = restarted.get("term-hist").expect("terminal respawned");
    assert_eq!(resumed.name, "hist-shell");
    let contents = resumed.test_contents().unwrap_or_default();
    assert!(
        !contents.contains("canopy-history-probe"),
        "a respawned terminal must open clean, without the previous run's output: {contents:?}"
    );
    assert!(
        !contents.contains("canopy-legacy-scrollback-marker"),
        "a respawned terminal must ignore legacy stored scrollback: {contents:?}"
    );
    let _ = restarted.kill("term-hist");
}
