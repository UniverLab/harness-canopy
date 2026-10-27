//! Hub client tests: typed bytes round-trip through a `cat` child into
//! the TUI parser, drops leave sessions running, stale frames are dropped,
//! snapshots rebuild the parser, and a protocol mismatch refuses to attach.

use std::time::{Duration, Instant};

use ratatui::style::Color;

use crate::domain::models::Cli;
use crate::sessions::client::test_client;
use crate::sessions::protocol::{parse_client_frame, spawn_frame, ClientFrame};
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
fn spawn_frame_carries_every_field() {
    let frame = spawn_frame(
        "r1",
        "agent",
        "claude",
        Some("opus"),
        "/work/dir",
        Some("bibi"),
        Some("hello"),
    );
    match parse_client_frame(&frame).expect("spawn frame parses") {
        ClientFrame::Spawn {
            req,
            kind,
            cli,
            model,
            workdir,
            name,
            prompt,
        } => {
            assert_eq!(req, "r1");
            assert_eq!(kind, "agent");
            assert_eq!(cli, "claude");
            assert_eq!(model.as_deref(), Some("opus"));
            assert_eq!(workdir, "/work/dir");
            assert_eq!(name.as_deref(), Some("bibi"));
            assert_eq!(prompt.as_deref(), Some("hello"));
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
            ..
        } => {
            assert_eq!(model, None);
            assert_eq!(name, None);
            assert_eq!(prompt, None);
        }
        other => panic!("spawn frame parsed as {other:?}"),
    }
}
