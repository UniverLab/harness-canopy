//! The hub's shutdown flag: while the daemon is stopping, a session child
//! that exits must keep its row resumable instead of finishing it.

use super::*;
use crate::sessions::SpawnRequest;

#[test]
fn shutting_down_leaves_an_agent_row_active_and_intact() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let request = SpawnRequest {
        harness_conversation_id: Some("conv-keep".to_string()),
        ..agent_spawn("cat", &workdir.path().to_string_lossy())
    };
    let (session, _guard) = KillGuard::spawn(&hub, &request);
    let before = db
        .get_hookable_session(&session.id)
        .expect("db read")
        .expect("agent row exists");
    assert_eq!(before.status, "active");

    hub.set_shutting_down();
    hub.record_natural_exit(&session, 7);

    let after = db
        .get_hookable_session(&session.id)
        .expect("db read")
        .expect("agent row still exists");
    assert_eq!(after.status, "active", "the row stays resumable");
    assert_eq!(after.pid, before.pid, "the pid is kept for the resume");
    assert_eq!(after.boot_id, before.boot_id);
    assert_eq!(after.args, before.args);
    assert_eq!(
        after.harness_conversation_id.as_deref(),
        Some("conv-keep"),
        "the conversation identity is kept for the resume"
    );
    assert!(
        hub.get(&session.id).is_none(),
        "the session still leaves the hub"
    );
}

#[test]
fn shutting_down_leaves_a_terminal_row_idle() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );
    let id = session.id.clone();

    hub.set_shutting_down();
    hub.record_natural_exit(&session, 0);

    assert!(
        db.get_active_terminal_sessions()
            .expect("db read")
            .iter()
            .any(|terminal| terminal.id == id),
        "a terminal that dies with the daemon must stay idle for the next start"
    );
}

#[test]
fn a_running_daemon_finishes_the_rows_as_before() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);

    let (terminal, _terminal_guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );
    let terminal_id = terminal.id.clone();
    hub.record_natural_exit(&terminal, 0);
    assert!(
        db.get_active_terminal_sessions()
            .expect("db read")
            .iter()
            .all(|session| session.id != terminal_id),
        "outside a daemon stop a terminal exit finishes its row"
    );

    let request = SpawnRequest {
        harness_conversation_id: Some("conv-done".to_string()),
        ..agent_spawn("cat", &workdir.path().to_string_lossy())
    };
    let (agent, _agent_guard) = KillGuard::spawn(&hub, &request);
    hub.record_natural_exit(&agent, 0);
    assert_eq!(
        db.get_interactive_session_status(&agent.id)
            .expect("db read")
            .as_deref(),
        Some("completed"),
        "outside a daemon stop an agent exit finishes its row"
    );
}
