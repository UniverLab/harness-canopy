//! The remote bridge's per-session output forwarder: snapshot and `out`
//! frame splitting, the coalescing pump, and the lag/budget snapshot
//! fallback. Split from `bridge.rs` for the file-size gate.

use std::sync::Arc;

use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::sessions::outbound::{self, PushOut, SessionCoal};
use crate::sessions::protocol::{out_frame, snapshot_frame, snapshot_more_frame};
use crate::sessions::ws::OutTx;
use crate::sessions::{HubSession, OutputReceiver};

use super::CHUNK_BYTES;

/// Split one raw output payload into wire frames of at most
/// `MAX_FRAME_BYTES`, each keeping the source `seq`. An empty payload is
/// one empty frame; nothing is ever truncated.
pub fn split_out_frames(id: &str, seq: u64, data: &[u8]) -> Vec<String> {
    if data.is_empty() {
        return vec![out_frame(id, seq, &[])];
    }
    data.chunks(CHUNK_BYTES)
        .map(|chunk| out_frame(id, seq, chunk))
        .collect()
}

/// Split one snapshot into a leading `snapshot` frame (cols/rows/seq and the
/// first chunk) followed by `snapshot_more` frames, the final one carrying
/// `last: true`. A snapshot that fits the first frame still ends with an
/// empty `snapshot_more{last:true}`: the device only knows the snapshot is
/// whole when it sees `last`, so a lone `snapshot` would never be drawn.
pub fn split_snapshot_frames(id: &str, cols: u16, rows: u16, seq: u64, data: &[u8]) -> Vec<String> {
    let mut chunks = data.chunks(CHUNK_BYTES);
    let first = chunks.next().unwrap_or(&[]);
    let mut frames = vec![snapshot_frame(id, cols, rows, seq, first)];
    let rest: Vec<&[u8]> = chunks.collect();
    if rest.is_empty() {
        frames.push(snapshot_more_frame(id, &[], true));
        return frames;
    }
    let total = rest.len();
    for (index, chunk) in rest.into_iter().enumerate() {
        frames.push(snapshot_more_frame(id, chunk, index + 1 == total));
    }
    frames
}

pub(crate) fn spawn_forwarder(
    session: Arc<HubSession>,
    from_seq: u64,
    rx: OutputReceiver,
    out: OutTx,
    snapshot_wire: usize,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = rx;
        let mut next = from_seq;
        let mut coal = SessionCoal::remote();
        coal.allow_snapshot(snapshot_wire);
        loop {
            let deadline = coal.next_deadline();
            tokio::select! {
                outcome = rx.recv() => {
                    if !pump(&session, &mut next, outcome, &out, &mut coal) {
                        return;
                    }
                }
                _ = outbound::wait_until(deadline) => {
                    if !flush_coal(&session.id, &out, &mut coal) {
                        return;
                    }
                }
            }
        }
    })
}

/// Send the coalesced buffer as size-bounded `out` frames; false ends the
/// forwarder.
pub(crate) fn flush_coal(id: &str, out: &OutTx, coal: &mut SessionCoal) -> bool {
    match coal.take_flush() {
        Some((seq, data)) => send_frames(out, split_out_frames(id, seq, &data)),
        None => true,
    }
}

/// Buffer one broadcast outcome (or fall back to a snapshot); false ends the
/// forwarder.
pub(crate) fn pump(
    session: &HubSession,
    next: &mut u64,
    outcome: Result<(u64, Vec<u8>), broadcast::error::RecvError>,
    out: &OutTx,
    coal: &mut SessionCoal,
) -> bool {
    match outcome {
        Ok((seq, chunk)) => {
            if seq <= *next {
                return true;
            }
            *next = seq;
            match coal.push_out(out, seq, &chunk) {
                PushOut::Buffered | PushOut::Dropped => true,
                PushOut::NeedSnapshot => send_snapshot(session, next, out, coal),
            }
        }
        Err(broadcast::error::RecvError::Lagged(_)) => send_snapshot(session, next, out, coal),
        Err(broadcast::error::RecvError::Closed) => false,
    }
}

/// Drop buffered output, queue one fresh snapshot, and resume after its seq.
fn send_snapshot(
    session: &HubSession,
    next: &mut u64,
    out: &OutTx,
    coal: &mut SessionCoal,
) -> bool {
    let Some(snapshot) = session.fresh_snapshot() else {
        return false;
    };
    *next = snapshot.seq;
    coal.mark_snapshot_sent();
    let frames = split_snapshot_frames(
        &session.id,
        snapshot.cols,
        snapshot.rows,
        snapshot.seq,
        &snapshot.data,
    );
    send_frames(out, frames)
}

/// Send every frame in order; `false` when the connection is gone.
fn send_frames(out: &OutTx, frames: Vec<String>) -> bool {
    frames.into_iter().all(|frame| out.send(frame).is_ok())
}
