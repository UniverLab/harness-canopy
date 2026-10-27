//! Round-trip tests: spawn + attach + echo through `input`, and
//! snapshot-then-out continuity under a continuous writer.

use super::{
    agent_spawn, connect_client, decode_b64, encode_b64, recv_json, send_json, serve_state,
    terminal_spawn, test_db, test_hub, try_recv_json, KillGuard,
};
use crate::sessions::ws::SessionsState;

#[tokio::test]
async fn spawn_attach_echo_roundtrip() {
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
    // `spawned` and `session_added` race each other; collect both.
    let (spawned, added) = {
        let first = recv_json(&mut client, "spawned/added").await;
        let second = recv_json(&mut client, "spawned/added").await;
        if first["t"] == "spawned" {
            (first, second)
        } else {
            (second, first)
        }
    };
    assert_eq!(spawned["t"], "spawned");
    assert_eq!(spawned["req"], "s1");
    let id = spawned["id"].as_str().expect("spawned id").to_string();
    assert_eq!(added["t"], "session_added");
    assert_eq!(added["item"]["id"], id);
    assert_eq!(added["item"]["kind"], "terminal");

    send_json(&mut client, &serde_json::json!({"t": "attach", "id": id})).await;
    let snapshot = recv_json(&mut client, "snapshot").await;
    assert_eq!(snapshot["t"], "snapshot");
    let snap_seq = snapshot["seq"].as_u64().expect("snapshot seq");

    send_json(
        &mut client,
        &serde_json::json!({"t": "input", "id": id, "data": encode_b64(b"hello-roundtrip\n")}),
    )
    .await;
    // `cat` echoes the line back; PTY echo may add a chunk of its own.
    // Either way every `out` seq continues the snapshot's with no gap.
    let mut seen: Vec<u8> = Vec::new();
    let mut expected = snap_seq + 1;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let needle = b"hello-roundtrip".as_slice();
    while !seen.windows(needle.len()).any(|w| w == needle) {
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for the echo"
        );
        let frame = try_recv_json(&mut client, std::time::Duration::from_secs(10)).await;
        let frame = frame.expect("out frame");
        assert_eq!(frame["t"], "out", "only out frames expected here: {frame}");
        assert_eq!(frame["id"], id);
        let seq = frame["seq"].as_u64().expect("out seq");
        assert_eq!(seq, expected, "out seq must continue the snapshot's");
        expected += 1;
        seen.extend(decode_b64(frame["data"].as_str().expect("out data")));
    }

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
                assert!(frame["code"].is_i64());
                exited = true;
            }
            Some("session_removed") => {
                assert_eq!(frame["id"], id);
                removed = true;
            }
            // Straggler pre-kill output (the echo's second chunk).
            Some("out") => {}
            other => panic!("unexpected frame during kill: {other:?}"),
        }
    }
    server.abort();
}

/// Snapshot then `out` under a writer producing output continuously: every
/// chunk after the subscribe arrives exactly once, in order, with no gap.
#[test]
fn snapshot_then_out_has_no_gap_or_duplicate() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );
    assert_eq!(guard.id(), session.id);

    let (snapshot, mut rx) = session.attach().expect("attach");
    assert_eq!(snapshot.seq, 0, "output seq starts at 0");

    let writer = std::thread::spawn({
        let session = std::sync::Arc::clone(&session);
        move || {
            for i in 0..200u32 {
                session.ingest_chunk(format!("writer line {i}\r\n").as_bytes());
            }
        }
    });
    writer.join().expect("writer thread");

    let mut seqs = Vec::new();
    while let Ok((seq, _)) = rx.try_recv() {
        seqs.push(seq);
    }
    let expected: Vec<u64> = (1..=200).collect();
    assert_eq!(seqs, expected, "no gap and no duplicate after the snapshot");
}

/// Agent spawns record an `interactive_sessions` row like the TUI launch.
#[test]
fn agent_spawn_writes_interactive_session_row() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) =
        KillGuard::spawn(&hub, &agent_spawn("cat", &workdir.path().to_string_lossy()));
    let status = db
        .get_interactive_session_status(&session.id)
        .expect("db read");
    assert_eq!(status.as_deref(), Some("active"));
}
