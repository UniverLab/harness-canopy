//! Resize and exit tests: shrink keeps rows in scrollback, `kill` sends
//! `exited` + `session_removed`, and natural exits reach attached clients.

use super::{
    agent_spawn, connect_client, encode_b64, recv_json, send_json, serve_state, terminal_spawn,
    test_db, test_hub, KillGuard,
};
use crate::sessions::ws::SessionsState;

/// Shrinking a hub session moves the rows that fall off into scrollback
/// instead of dropping them, and the metadata follows the new size.
#[test]
fn resize_shrink_keeps_rows_in_scrollback() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );

    for i in 0..10u32 {
        session.ingest_chunk(format!("content line {i}\r\n").as_bytes());
    }
    session.resize(80, 5);

    let item = session.item();
    assert_eq!(item.cols, 80);
    assert_eq!(item.rows, 5);
    let all = session.test_all_text().expect("all text");
    for i in 0..10u32 {
        let line = format!("content line {i}");
        assert!(all.contains(&line), "{line} was dropped by the shrink");
    }

    session.resize(80, 24);
    assert_eq!(session.item().rows, 24);
    assert!(
        session
            .test_contents()
            .expect("contents")
            .contains("content line 9"),
        "the newest line must survive a shrink and regrow"
    );
}

/// `resize` through the protocol changes the size a later `attach` sees.
#[tokio::test]
async fn ws_resize_roundtrip() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "spawn", "req": "s1", "kind": "terminal",
            "cli": "cat", "workdir": workdir.path().to_string_lossy(),
        }),
    )
    .await;
    let mut id = String::new();
    for _ in 0..2 {
        let frame = recv_json(&mut client, "spawned/added").await;
        if frame["t"] == "spawned" {
            id = frame["id"].as_str().expect("spawned id").to_string();
        }
    }
    assert!(!id.is_empty());

    send_json(&mut client, &serde_json::json!({"t": "attach", "id": id})).await;
    let before = recv_json(&mut client, "snapshot").await;
    assert_eq!(before["cols"].as_u64(), Some(80));
    assert_eq!(before["rows"].as_u64(), Some(24));

    send_json(
        &mut client,
        &serde_json::json!({"t": "resize", "id": id, "cols": 100, "rows": 30}),
    )
    .await;
    // The server applies frames in order, so a re-attach sees the new size.
    send_json(&mut client, &serde_json::json!({"t": "attach", "id": id})).await;
    let after = recv_json(&mut client, "snapshot").await;
    assert_eq!(after["t"], "snapshot");
    assert_eq!(after["cols"].as_u64(), Some(100));
    assert_eq!(after["rows"].as_u64(), Some(30));

    send_json(
        &mut client,
        &serde_json::json!({"t": "kill", "req": "k", "id": id}),
    )
    .await;
    let mut done = false;
    while !done {
        let frame = recv_json(&mut client, "kill frames").await;
        done |= frame["t"] == "ok";
    }
    server.abort();
}

/// `kill` ends the child, sends `exited` + `session_removed` to every
/// connection, drops the session from `list`, and finishes its DB row.
#[tokio::test]
async fn kill_sends_exited_and_removed() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "spawn", "req": "s1", "kind": "agent",
            "cli": "cat", "workdir": workdir.path().to_string_lossy(),
        }),
    )
    .await;
    let mut id = String::new();
    for _ in 0..2 {
        let frame = recv_json(&mut client, "spawned/added").await;
        if frame["t"] == "spawned" {
            id = frame["id"].as_str().expect("spawned id").to_string();
        }
    }
    send_json(&mut client, &serde_json::json!({"t": "attach", "id": id})).await;
    assert_eq!(recv_json(&mut client, "snapshot").await["t"], "snapshot");

    send_json(
        &mut client,
        &serde_json::json!({"t": "kill", "req": "k1", "id": id}),
    )
    .await;
    let mut ok = false;
    let mut exited = false;
    let mut removed = false;
    while !(ok && exited && removed) {
        let frame = recv_json(&mut client, "kill frames").await;
        match frame["t"].as_str() {
            Some("ok") => {
                assert_eq!(frame["req"], "k1");
                ok = true;
            }
            Some("exited") => {
                assert_eq!(frame["id"], id);
                assert_eq!(frame["code"], 1, "a signalled child reports its death");
                exited = true;
            }
            Some("session_removed") => {
                assert_eq!(frame["id"], id);
                removed = true;
            }
            // Straggler pre-kill output.
            Some("out") => {}
            other => panic!("unexpected frame during kill: {other:?}"),
        }
    }

    send_json(&mut client, &serde_json::json!({"t": "list", "req": "l1"})).await;
    let listed = recv_json(&mut client, "sessions").await;
    assert_eq!(listed["t"], "sessions");
    assert!(
        listed["items"].as_array().expect("items").is_empty(),
        "a killed session leaves the list"
    );
    // `ok` is sent after the hub finished the kill, so the row is final.
    assert_eq!(
        db.get_interactive_session_status(&id)
            .expect("db read")
            .as_deref(),
        Some("error")
    );
    server.abort();
}

/// A child that exits on its own sends `exited` to attached connections
/// and stays listed as exited. The exit is triggered after the attach (an
/// instant child like `true` would exit before anyone attaches, and `exited`
/// goes only to currently attached connections).
#[tokio::test]
async fn natural_exit_sends_exited_to_attached() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let state = SessionsState::for_test(test_hub(&db), "token".to_string());
    let (addr, server) = serve_state(state).await;
    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");

    send_json(
        &mut client,
        &serde_json::json!({
            "t": "spawn", "req": "s1", "kind": "terminal",
            "cli": "sh", "workdir": workdir.path().to_string_lossy(),
        }),
    )
    .await;
    let mut id = String::new();
    for _ in 0..2 {
        let frame = recv_json(&mut client, "spawned/added").await;
        if frame["t"] == "spawned" {
            assert_eq!(frame["req"], "s1");
            id = frame["id"].as_str().expect("spawned id").to_string();
        }
    }
    assert!(!id.is_empty());
    send_json(&mut client, &serde_json::json!({"t": "attach", "id": id})).await;
    assert_eq!(recv_json(&mut client, "snapshot").await["t"], "snapshot");

    send_json(
        &mut client,
        &serde_json::json!({"t": "input", "id": id, "data": encode_b64(b"exit\n")}),
    )
    .await;
    let mut exited = false;
    while !exited {
        let frame = recv_json(&mut client, "out/exited").await;
        match frame["t"].as_str() {
            Some("out") => {}
            Some("exited") => {
                assert_eq!(frame["id"], id);
                assert_eq!(frame["code"], 0);
                exited = true;
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    send_json(&mut client, &serde_json::json!({"t": "list", "req": "l1"})).await;
    let listed = recv_json(&mut client, "sessions").await;
    let items = listed["items"].as_array().expect("items");
    assert_eq!(items.len(), 1, "an exited session stays listed");
    assert_eq!(items[0]["status"], "exited");
    assert_eq!(items[0]["exit_code"], 0);
    server.abort();
}

/// A natural agent exit marks its `interactive_sessions` row finished.
#[test]
fn natural_exit_marks_agent_row_finished() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &agent_spawn("true", &workdir.path().to_string_lossy()),
    );
    let id = session.id.clone();
    drop(session);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let status = db.get_interactive_session_status(&id).expect("db read");
        if status.as_deref() == Some("completed") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the exited row was never marked finished"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Hub-level kill of a session whose child already exited still removes it.
#[test]
fn kill_after_natural_exit_removes_the_session() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, guard) = KillGuard::spawn(
        &hub,
        &agent_spawn("true", &workdir.path().to_string_lossy()),
    );
    let id = session.id.clone();
    drop(session);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while hub
        .get(&id)
        .is_some_and(|s| s.status() != crate::sessions::SessionStatus::Exited(0))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the child never exited"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(hub.kill(&id), Some(0));
    assert!(hub.get(&id).is_none());
    drop(guard);
}
