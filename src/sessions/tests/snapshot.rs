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

/// The scrollback prefix rebuilds the same rows in a fresh parser, in order
/// and with their attributes, across more than one page of paging, and leaves
/// the source showing its live screen.
#[test]
fn scrollback_render_round_trips_rows_and_attributes() {
    let mut source = vt100::Parser::new(5, 20, 100);
    for i in 0..13 {
        let line = if i == 3 {
            "\x1b[31mline 3\x1b[m\r\n".to_string()
        } else {
            format!("line {i}\r\n")
        };
        source.process(line.as_bytes());
    }

    let bytes = crate::sessions::hub::scrollback_formatted(source.screen_mut());
    assert_eq!(source.screen().scrollback(), 0, "the source stays live");

    let mut client = vt100::Parser::new(5, 20, 100);
    client.process(&bytes);
    client.process(&source.screen().contents_formatted());
    assert_eq!(client.screen().contents(), source.screen().contents());

    for parser in [&mut source, &mut client] {
        parser.screen_mut().set_scrollback(usize::MAX);
        assert_eq!(parser.screen().scrollback(), 9);
    }
    assert_eq!(client.screen().contents(), source.screen().contents());
    let red = client.screen().cell(3, 0).expect("cell in range").fgcolor();
    assert_eq!(red, vt100::Color::Idx(1), "line 3 keeps its colour");
    let plain = client.screen().cell(4, 0).expect("cell in range").fgcolor();
    assert_eq!(plain, vt100::Color::Default, "the colour does not leak");
}

/// A session attached while a full-screen app runs: the snapshot leaves a
/// fresh client on the alternate screen showing the app, with the main
/// buffer (scrollback and screen) intact underneath, so the app's later
/// `?1049l` brings back exactly what the hub shows.
#[test]
fn snapshot_of_an_alternate_screen_restores_it_on_the_alternate_screen() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );

    // The raw stream also feeds a reference parser, to compare the cursor.
    let mut reference = HubVt::new_with_callbacks(24, 80, VT_SCROLLBACK_LINES, ());
    let mut feed = |chunk: &[u8]| {
        session.ingest_chunk(chunk);
        reference.process(chunk);
    };
    for i in 0..40u32 {
        feed(format!("main line {i:02}\r\n").as_bytes());
    }
    feed(b"shell prompt$ ");
    feed(b"\x1b[?1049h\x1b[H\x1b[2Jfull-screen app\r\n\x1b[7mstatus bar\x1b[m");

    let (snapshot, _rx) = session.attach().expect("attach");
    let mut client = HubVt::new_with_callbacks(24, 80, VT_SCROLLBACK_LINES, ());
    client.process(&snapshot.data);

    assert!(
        client.screen().alternate_screen(),
        "the client ends on the alternate screen"
    );
    assert_eq!(
        client.screen().contents(),
        session.test_contents().expect("hub contents"),
        "the client's alternate screen matches the hub's"
    );
    assert!(
        !client.screen().contents().contains("main line"),
        "main-buffer rows never paint onto the alternate screen"
    );

    // The app exits: both return to the main buffer, which must match.
    feed(b"\x1b[?1049l");
    client.process(b"\x1b[?1049l");
    assert!(!client.screen().alternate_screen());
    assert_eq!(
        client.screen().contents(),
        session.test_contents().expect("hub contents"),
        "the main screen behind the app survives the snapshot"
    );
    assert_eq!(
        client.screen().cursor_position(),
        reference.screen().cursor_position(),
        "the main cursor is restored where the hub has it"
    );
    client.screen_mut().set_scrollback(usize::MAX);
    assert!(
        client.screen().contents().contains("main line 00"),
        "the main scrollback stays in the client's normal buffer"
    );
}
