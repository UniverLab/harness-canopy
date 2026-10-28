//! Hub client tests: typed bytes round-trip through a `cat` child into
//! the TUI parser, drops leave sessions running, stale frames are dropped,
//! snapshots rebuild the parser, and a protocol mismatch refuses to attach.

use std::time::{Duration, Instant};

use ratatui::style::Color;

use crate::domain::models::Cli;
use crate::sessions::client::{test_client, HubClient};
use crate::sessions::protocol::{parse_client_frame, spawn_frame, spawn_frame_full, ClientFrame};
use crate::tui::agent::InteractiveAgent;

fn spawn_cat(name: &str) -> InteractiveAgent {
    InteractiveAgent::spawn_terminal("cat", "/tmp", 80, 24, Some(name), &[], Color::White)
        .expect("spawn cat on the test hub")
}

fn wait_for(mut check: impl FnMut() -> bool, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if check() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn typed_input_echoes_onto_the_tui_parser() {
    let mut agent = spawn_cat("client-echo");
    agent
        .write_to_pty(b"hello-hub\n")
        .expect("write to cat's stdin");
    assert!(
        wait_for(
            || agent.visible_text().contains("hello-hub"),
            Duration::from_secs(10)
        ),
        "the cat echo never reached the TUI parser"
    );
    agent.kill();
}

#[test]
fn dropping_the_client_leaves_the_child_alive_for_the_next_client() {
    let hub = &test_client().hub;
    let first = spawn_cat("client-survives");
    first
        .write_to_pty(b"survivor-line\n")
        .expect("write to cat's stdin");
    assert!(
        wait_for(
            || first.visible_text().contains("survivor-line"),
            Duration::from_secs(10)
        ),
        "the cat echo never reached the first client"
    );
    let id = first.id.clone();
    drop(first);

    let session = hub.get(&id).expect("the hub keeps the dropped session");
    assert_eq!(session.status(), crate::sessions::SessionStatus::Running);

    let item = session.item();
    let mut second =
        InteractiveAgent::attach_item(&item, Cli::new("cat"), Color::White, None, 80, 24)
            .expect("a new client attaches to the surviving session");
    assert!(
        second.visible_text().contains("survivor-line"),
        "the new client does not see the same screen"
    );
    second.kill();
}

#[test]
fn stale_out_frames_are_dropped() {
    let mut agent = spawn_cat("client-seq");
    // Above any `seq` the live stream can reach, so the verdicts below do
    // not depend on hub timing.
    let fresh = u64::MAX - 1;
    assert!(agent.apply_out_frame(fresh, b"fresh\r\n"));
    assert!(agent.visible_text().contains("fresh"));
    assert!(!agent.apply_out_frame(fresh, b"stale\r\n"));
    assert!(!agent.apply_out_frame(fresh - 1, b"older\r\n"));
    assert!(!agent.visible_text().contains("stale"));
    assert!(!agent.visible_text().contains("older"));
    agent.kill();
}

#[test]
fn snapshot_rebuilds_the_parser_at_the_snapshot_size() {
    let mut agent = spawn_cat("client-snapshot");
    agent.apply_snapshot(100, 30, 41, b"snap-body\r\n");
    let (rows, cols) = agent.vt.lock().expect("vt lock").screen().size();
    assert_eq!((rows, cols), (30, 100));
    assert!(agent.visible_text().contains("snap-body"));
    // The snapshot `seq` becomes the floor: older `out` frames drop.
    assert!(!agent.apply_out_frame(41, b"dropped\r\n"));
    assert!(agent.apply_out_frame(42, b"kept\r\n"));
    agent.kill();
}

#[test]
fn closing_any_clone_closes_the_shared_connection() {
    // Quitting the TUI closes the control WebSocket through one handle while
    // the shared `ActiveHub` holds another: if a clone kept its own sender,
    // the socket would outlive the quit and every later frame would go into
    // a connection nobody reads.
    let test = test_client();
    let client = HubClient::connect(&test.url, &test.token).expect("control connection");
    let clone = client.clone();
    clone.close();
    assert!(
        client.tx.lock().expect("client tx").is_none(),
        "closing a clone left the shared connection open"
    );
    assert!(
        clone.tx.lock().expect("clone tx").is_none(),
        "the closed clone still holds the sender"
    );
}

#[test]
fn spawn_frame_carries_every_field() {
    use crate::sessions::protocol::{SpawnExt, SpawnFrameArgs};
    let ext = SpawnExt {
        interactive_args: Some("--tui --yolo".to_string()),
        fallback_args: Some("chat".to_string()),
        model_flag: Some("-m".to_string()),
        id: Some("sess-1".to_string()),
        seed_id: Some("seed-1".to_string()),
        accent: Some("1,2,3".to_string()),
        yolo: true,
    };
    let frame = spawn_frame_full(&SpawnFrameArgs {
        req: "r1",
        kind: "agent",
        cli: "claude",
        model: Some("opus"),
        workdir: "/work/dir",
        name: Some("bibi"),
        prompt: Some("hello"),
        ext: &ext,
    });
    match parse_client_frame(&frame).expect("spawn frame parses") {
        ClientFrame::Spawn {
            req,
            kind,
            cli,
            model,
            workdir,
            name,
            prompt,
            ext,
        } => {
            assert_eq!(req, "r1");
            assert_eq!(kind, "agent");
            assert_eq!(cli, "claude");
            assert_eq!(model.as_deref(), Some("opus"));
            assert_eq!(workdir, "/work/dir");
            assert_eq!(name.as_deref(), Some("bibi"));
            assert_eq!(prompt.as_deref(), Some("hello"));
            assert_eq!(ext.interactive_args.as_deref(), Some("--tui --yolo"));
            assert_eq!(ext.fallback_args.as_deref(), Some("chat"));
            assert_eq!(ext.model_flag.as_deref(), Some("-m"));
            assert_eq!(ext.id.as_deref(), Some("sess-1"));
            assert_eq!(ext.seed_id.as_deref(), Some("seed-1"));
            assert_eq!(ext.accent.as_deref(), Some("1,2,3"));
            assert!(ext.yolo, "yolo:true must survive the frame");
        }
        other => panic!("spawn frame parsed as {other:?}"),
    }
    // Absent optionals round-trip as `None`.
    let frame = spawn_frame("r2", "terminal", "bash", None, "/tmp", None, None);
    match parse_client_frame(&frame).expect("spawn frame parses") {
        ClientFrame::Spawn {
            model,
            name,
            prompt,
            ext,
            ..
        } => {
            assert_eq!(model, None);
            assert_eq!(name, None);
            assert_eq!(prompt, None);
            assert_eq!(ext.interactive_args, None);
            assert_eq!(ext.fallback_args, None);
            assert_eq!(ext.model_flag, None);
            assert_eq!(ext.id, None);
            assert_eq!(ext.seed_id, None);
            assert_eq!(ext.accent, None);
            assert!(!ext.yolo, "an explicit yolo:false reads as false");
        }
        other => panic!("spawn frame parsed as {other:?}"),
    }
    // A missing or null `yolo` also reads as false (older clients).
    for text in [
        r#"{"t": "spawn", "req": "r3", "kind": "agent", "cli": "c", "workdir": "/tmp"}"#,
        r#"{"t": "spawn", "req": "r4", "kind": "agent", "cli": "c", "workdir": "/tmp", "yolo": null}"#,
    ] {
        match parse_client_frame(text).expect("spawn frame parses") {
            ClientFrame::Spawn { ext, .. } => {
                assert!(!ext.yolo, "a missing yolo reads as false: {text}");
            }
            other => panic!("spawn frame parsed as {other:?}"),
        }
    }
}
