//! Outbound queue tests: the byte bound, the snapshot fallback, coalescing and
//! control-frame survival, all in-process (no daemon, no transport).

use super::{terminal_spawn, test_db, test_hub, KillGuard};
use crate::sessions::outbound::{self, PushOut, SessionCoal, MAX_BYTES};
use crate::sessions::protocol::{error_frame, hello_frame, ok_frame, snapshot_frame, ERR_INTERNAL};
use crate::sessions::ws::{flush_coal, forward_recv};

fn json(frame: &str) -> serde_json::Value {
    serde_json::from_str(frame).expect("frame is JSON")
}

/// A slow consumer that never drains gets at most the byte bound plus one
/// fresh snapshot, never the whole 16 MiB burst.
#[test]
fn a_slow_consumer_gets_the_bound_plus_one_snapshot_not_the_whole_burst() {
    const BURST: usize = 16 * 1024 * 1024;
    let (tx, mut rx) = outbound::channel();
    let mut coal = SessionCoal::new(false);
    let chunk = vec![b'x'; 4096];
    let mut seq = 0u64;
    for _ in 0..(BURST / chunk.len()) {
        seq += 1;
        match coal.push_out(&tx, seq, &chunk) {
            PushOut::NeedSnapshot => {
                coal.mark_snapshot_sent();
                assert!(tx.push_control(snapshot_frame("s", 80, 24, seq, b"fresh")));
            }
            PushOut::Buffered => {
                flush_coal("s", &tx, &mut coal);
            }
            PushOut::Dropped => {}
        }
    }

    let mut out_bytes = 0;
    let mut snapshot_bytes = 0;
    let mut snapshots = 0;
    let mut out_seqs = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        let value = json(&frame);
        match value["t"].as_str() {
            Some("out") => {
                out_bytes += frame.len();
                out_seqs.push(value["seq"].as_u64().expect("out seq"));
            }
            Some("snapshot") => {
                snapshot_bytes += frame.len();
                snapshots += 1;
            }
            other => panic!("unexpected frame before the snapshot: {other:?}"),
        }
    }

    assert_eq!(snapshots, 1, "one fallback snapshot, not one per chunk");
    assert!(
        out_bytes <= MAX_BYTES,
        "queued out bytes {out_bytes} exceed the {MAX_BYTES} bound"
    );
    assert!(
        out_bytes + snapshot_bytes < BURST / 4,
        "a slow consumer must not receive the burst ({out_bytes} + {snapshot_bytes} of {BURST})"
    );
    assert!(
        out_seqs.windows(2).all(|pair| pair[0] <= pair[1]),
        "out seq stays monotonic while the queue saturates"
    );
}

/// A queue that stops draining for longer than the stale window forces a
/// snapshot even though it is far below the byte bound: fresh, low-rate output
/// must not keep piling up behind a stalled link.
#[tokio::test]
async fn a_stalled_queue_falls_back_to_a_snapshot_after_half_a_second() {
    let (tx, _rx) = outbound::channel();
    let mut coal = SessionCoal::new(false);
    let chunk = vec![b'x'; 12 * 1024];
    assert_eq!(coal.push_out(&tx, 1, &chunk), PushOut::Buffered);
    assert!(flush_coal("s", &tx, &mut coal));
    assert!(tx.pending() > 0, "the flushed frame is still queued");
    tokio::time::sleep(std::time::Duration::from_millis(550)).await;
    assert_eq!(
        coal.push_out(&tx, 2, &chunk),
        PushOut::NeedSnapshot,
        "a queue that has not drained for 500 ms forces a snapshot"
    );
}

/// Control frames (`hello`, `ok`, `error`) are queued even while output is
/// saturated and the snapshot fallback is in flight.
#[test]
fn control_frames_survive_a_saturated_output_queue() {
    let (tx, mut rx) = outbound::channel();
    let mut coal = SessionCoal::new(false);
    let chunk = vec![b'x'; 12 * 1024];
    let mut seq = 0u64;
    let mut saturated = false;
    for _ in 0..200 {
        seq += 1;
        match coal.push_out(&tx, seq, &chunk) {
            PushOut::NeedSnapshot => {
                coal.mark_snapshot_sent();
                saturated = true;
                break;
            }
            PushOut::Buffered | PushOut::Dropped => {
                flush_coal("s", &tx, &mut coal);
            }
        }
    }
    assert!(saturated, "output eventually saturates the connection");
    assert!(tx.pending() > 0, "frames are still queued");

    assert!(tx.push_control(hello_frame("host", &[])));
    assert!(tx.push_control(ok_frame("r1")));
    assert!(tx.push_control(error_frame(Some("r2"), ERR_INTERNAL, "boom")));

    // Simulate the forwarder's fallback, then the next chunk is dropped.
    seq += 1;
    let _ = coal.push_out(&tx, seq, &chunk);
    coal.mark_snapshot_sent();
    seq += 1;
    assert_eq!(
        coal.push_out(&tx, seq, &chunk),
        PushOut::Dropped,
        "output stays dropped while a snapshot is in flight"
    );

    let mut kinds = Vec::new();
    while let Ok(frame) = rx.try_recv() {
        kinds.push(json(&frame)["t"].as_str().unwrap_or("").to_string());
    }
    for kind in ["hello", "ok", "error"] {
        assert!(
            kinds.iter().any(|seen| seen == kind),
            "control frame '{kind}' was dropped under saturation: {kinds:?}"
        );
    }
}

/// The TUI hub's `forward_recv` path applies the same bound: a real hub
/// session fed faster than the consumer drains coalesces and falls back.
#[tokio::test]
async fn the_tui_hub_path_applies_the_same_output_bound() {
    let (db, _db_dir) = test_db();
    let workdir = tempfile::tempdir().expect("workdir");
    let hub = test_hub(&db);
    let (session, _guard) = KillGuard::spawn(
        &hub,
        &terminal_spawn("cat", &workdir.path().to_string_lossy()),
    );

    let (tx, mut rx) = outbound::channel();
    let mut coal = SessionCoal::new(false);
    let mut next = 0u64;
    let chunk = vec![b'y'; 4096];
    for _ in 0..400 {
        let seq = next + 1;
        session.ingest_chunk(&chunk);
        assert!(forward_recv(
            &session,
            &mut next,
            Ok((seq, chunk.clone())),
            &tx,
            &mut coal
        ));
        flush_coal(&session.id, &tx, &mut coal);
    }
    // One more chunk, in case the fallback had not fired yet.
    let seq = next + 1;
    session.ingest_chunk(&chunk);
    assert!(forward_recv(
        &session,
        &mut next,
        Ok((seq, chunk)),
        &tx,
        &mut coal
    ));

    let mut out_bytes = 0;
    let mut snapshot_seq = None;
    let mut last_out_seq = 0;
    let mut snapshots = 0;
    while let Ok(frame) = rx.try_recv() {
        let value = json(&frame);
        match value["t"].as_str() {
            Some("out") => {
                out_bytes += frame.len();
                last_out_seq = value["seq"].as_u64().expect("out seq");
            }
            Some("snapshot") => {
                snapshots += 1;
                snapshot_seq = value["seq"].as_u64();
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }

    assert_eq!(snapshots, 1, "the hub connection falls back exactly once");
    assert!(
        out_bytes <= MAX_BYTES + 24 * 1024,
        "the hub connection queued {out_bytes} bytes past a {MAX_BYTES} bound"
    );
    let snapshot_seq = snapshot_seq.expect("a fallback snapshot");
    assert!(
        snapshot_seq >= last_out_seq,
        "the snapshot ({snapshot_seq}) resumes at or after the last out ({last_out_seq})"
    );
}
