//! Snapshot tests: a client parser fed snapshot + out matches the hub
//! parser, and a lagged subscriber gets a fresh snapshot and continues.

use super::{
    connect_client, recv_json, send_json, serve_state, terminal_spawn, test_db, test_hub, KillGuard,
};
use crate::sessions::hub::HubVt;
use crate::sessions::ws::SessionsState;
use crate::sessions::{SessionHub, VT_SCROLLBACK_LINES};

/// Feed the hub session, attach, feed more, and replay snapshot + outs into
/// a fresh client parser: visible text and scrollback must match the hub's.
#[test]
fn client_parser_matches_hub_after_snapshot_plus_out() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );

    for i in 0..40u32 {
        if i == 5 {
            session.ingest_chunk(b"\x1b[1;31mbold red 05\x1b[0m\r\n");
        } else {
            session.ingest_chunk(format!("scrollback line {i:02}\r\n").as_bytes());
        }
    }
    session.ingest_chunk(b"visible tail\r\n");

    let (snapshot, mut rx) = session.attach().expect("attach");
    for i in 0..5u32 {
        session.ingest_chunk(format!("late line {i}\r\n").as_bytes());
    }
    let mut outs = Vec::new();
    while let Ok((seq, chunk)) = rx.try_recv() {
        outs.push((seq, chunk));
    }
    assert_eq!(outs.len(), 5, "the five late chunks arrive as out frames");
    assert!(
        outs.windows(2).all(|w| w[1].0 == w[0].0 + 1) && outs[0].0 == snapshot.seq + 1,
        "out seqs continue the snapshot's with no gap"
    );

    let mut client = HubVt::new_with_callbacks(24, 80, VT_SCROLLBACK_LINES, ());
    client.process(&snapshot.data);
    for (_, chunk) in &outs {
        client.process(chunk);
    }

    assert_eq!(
        client.screen().contents(),
        session.test_contents().expect("hub contents"),
        "client visible screen must match the hub's"
    );
    client.screen_mut().set_scrollback(usize::MAX);
    let client_scrollback = client.screen().contents();
    let hub_scrollback = session.test_scrollback_contents().expect("hub scrollback");
    assert_eq!(
        client_scrollback, hub_scrollback,
        "client scrollback must match the hub's"
    );
    assert!(
        client_scrollback.contains("scrollback line 00"),
        "the oldest scrollback row must survive the round trip"
    );
    assert!(
        client_scrollback.contains("bold red 05"),
        "styled scrollback rows must survive the round trip"
    );
}

/// A subscriber that falls behind gets a fresh snapshot instead of a gap,
/// then keeps receiving `out` frames after it.
#[tokio::test(flavor = "current_thread")]
async fn lagged_subscriber_gets_fresh_snapshot_and_continues() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 2);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );
    let state = SessionsState::for_test(hub, "token".to_string());
    let (addr, server) = serve_state(state).await;

    let mut client = connect_client(addr, Some("token")).await;
    assert_eq!(recv_json(&mut client, "hello").await["t"], "hello");
    send_json(
        &mut client,
        &serde_json::json!({"t": "attach", "id": session.id}),
    )
    .await;
    let first = recv_json(&mut client, "snapshot").await;
    assert_eq!(first["t"], "snapshot");
    assert_eq!(first["seq"], 0);

    // Five chunks into a capacity-2 channel with no awaits in between: the
    // forwarder cannot run until the test reads again, so it must lag.
    for i in 0..5u32 {
        session.ingest_chunk(format!("lag line {i}\r\n").as_bytes());
    }
    let resync = recv_json(&mut client, "resync snapshot").await;
    assert_eq!(
        resync["t"], "snapshot",
        "a lagged subscriber gets a fresh snapshot, not a gap: {resync}"
    );
    assert_eq!(resync["seq"], 5);
    assert_eq!(resync["cols"], 80);
    assert_eq!(resync["rows"], 24);

    session.ingest_chunk(b"after lag\r\n");
    let out = recv_json(&mut client, "out after resync").await;
    assert_eq!(out["t"], "out");
    assert_eq!(out["seq"], 6);
    server.abort();
}
