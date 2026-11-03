//! Bounded, coalesced output queues for one connection.
//!
//! A connection's writer drains [`ConnRx`]; producers push control frames
//! (never dropped) or output chunks. Output is coalesced per attached session
//! in a [`SessionCoal`] buffer and only sent when it reaches
//! [`COALESCE_MAX`] bytes or [`COALESCE_MS`] milliseconds have passed. When a
//! slow consumer lets the queue pass [`MAX_BYTES`], or the queue goes
//! [`STALE_MS`] without draining while output is still arriving, the buffered
//! output is dropped and replaced by one fresh snapshot, so the client always
//! catches up to the current screen.
//!
//! The remote data channel additionally caps output at
//! `1 / REMOTE_MIN_INTERVAL` frames per second per session.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

/// Per-connection byte budget for queued outbound frames.
pub const MAX_BYTES: usize = 512 * 1024;
/// Coalescing window: a buffered chunk is sent this long after it arrived.
pub const COALESCE_MS: u64 = 25;
/// Coalescing size: a buffered chunk is sent once it reaches this many bytes.
pub const COALESCE_MAX: usize = 12 * 1024;
/// A buffer that cannot be flushed for this long falls back to a snapshot.
pub const STALE_MS: u64 = 500;
/// A buffer larger than this releases its capacity once it is idle.
pub const SHRINK_AT: usize = 1024 * 1024;
/// Minimum gap between two `out` sends on a remote data channel (20 frames/s).
pub const REMOTE_MIN_INTERVAL: Duration = Duration::from_millis(50);

const COALESCE: Duration = Duration::from_millis(COALESCE_MS);
const STALE: Duration = Duration::from_millis(STALE_MS);

/// Upper bound on the wire size of `raw` buffered bytes: base64 expansion
/// plus per-frame JSON envelope, with room for the remote splitter's frames.
fn wire_bytes(raw: usize) -> usize {
    raw.div_ceil(3).saturating_mul(4).saturating_add(4096)
}

/// Sleep until `deadline`, or forever when there is nothing buffered.
pub async fn wait_until(deadline: Option<Instant>) {
    match deadline {
        Some(at) => tokio::time::sleep_until(at.into()).await,
        None => std::future::pending().await,
    }
}

/// Sender half of one connection's outbound queue.
#[derive(Clone)]
pub struct ConnTx {
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<AtomicUsize>,
    transport: Arc<AtomicUsize>,
    /// When the writer last drained a frame, or when the queue last filled
    /// from empty: the anchor for "how far behind is this consumer".
    progress: Arc<Mutex<Instant>>,
}

/// Receiver half: what the connection's writer drains.
pub struct ConnRx {
    rx: mpsc::UnboundedReceiver<String>,
    pending: Arc<AtomicUsize>,
    progress: Arc<Mutex<Instant>>,
}

/// Create one connection's queue; the budget is shared by both halves.
pub fn channel() -> (ConnTx, ConnRx) {
    let (tx, rx) = mpsc::unbounded_channel();
    let pending = Arc::new(AtomicUsize::new(0));
    let transport = Arc::new(AtomicUsize::new(0));
    let progress = Arc::new(Mutex::new(Instant::now()));
    (
        ConnTx {
            tx,
            pending: Arc::clone(&pending),
            transport,
            progress: Arc::clone(&progress),
        },
        ConnRx {
            rx,
            pending,
            progress,
        },
    )
}

impl ConnTx {
    /// Queue a control frame. Control frames are never dropped, even while a
    /// slow consumer saturates the output budget. Returns `false` once the
    /// connection is gone.
    pub fn push_control(&self, frame: String) -> bool {
        let bytes = frame.len();
        // Count the frame before it becomes visible to the receiver: the
        // writer may drain and subtract it the moment `send` publishes it,
        // and subtracting an uncounted frame would wrap `pending`.
        let was_empty = self.pending.fetch_add(bytes, Ordering::Relaxed) == 0;
        if was_empty {
            self.mark_progress();
        }
        if self.tx.send(frame).is_err() {
            self.pending.fetch_sub(bytes, Ordering::Relaxed);
            return false;
        }
        true
    }

    /// Alias for [`Self::push_control`] kept for the control-producing
    /// modules, which all push `hello`, `list`, `ok`, `error`, `shelf` and
    /// `metrics` frames.
    pub fn send(&self, frame: String) -> Result<(), ()> {
        if self.push_control(frame) {
            Ok(())
        } else {
            Err(())
        }
    }

    /// Whether the connection's writer has gone away.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    /// Bytes queued for this connection: frames not yet drained by the writer
    /// plus whatever the transport still buffers.
    pub fn pending(&self) -> usize {
        self.pending
            .load(Ordering::Relaxed)
            .saturating_add(self.transport.load(Ordering::Relaxed))
    }

    /// How long the queued output has waited without the writer draining
    /// anything; `None` when the queue is empty.
    pub fn behind(&self) -> Option<Duration> {
        if self.pending.load(Ordering::Relaxed) == 0 {
            return None;
        }
        self.progress.lock().ok().map(|at| at.elapsed())
    }

    fn mark_progress(&self) {
        if let Ok(mut at) = self.progress.lock() {
            *at = Instant::now();
        }
    }

    /// Record the transport's own backlog (`buffered_amount()`), so the byte
    /// budget covers the SCTP queue the writer has already handed off.
    pub fn set_transport_backlog(&self, bytes: usize) {
        self.transport.store(bytes, Ordering::Relaxed);
    }
}

impl ConnRx {
    /// Next frame, decrementing the queued-byte budget as it leaves.
    pub async fn recv(&mut self) -> Option<String> {
        let frame = self.rx.recv().await?;
        if let Ok(mut at) = self.progress.lock() {
            *at = Instant::now();
        }
        self.pending.fetch_sub(frame.len(), Ordering::Relaxed);
        Some(frame)
    }

    /// Non-blocking [`Self::recv`] for tests.
    #[cfg(test)]
    pub fn try_recv(&mut self) -> Result<String, mpsc::error::TryRecvError> {
        let frame = self.rx.try_recv()?;
        if let Ok(mut at) = self.progress.lock() {
            *at = Instant::now();
        }
        self.pending.fetch_sub(frame.len(), Ordering::Relaxed);
        Ok(frame)
    }
}

/// What buffering one output chunk produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOut {
    /// The chunk is buffered and will be sent by the coalescer.
    Buffered,
    /// The consumer is too far behind: drop the buffer and send a snapshot.
    NeedSnapshot,
    /// A snapshot is already in flight and the consumer has not caught up:
    /// drop the chunk, the snapshot already covers it.
    Dropped,
}

/// Per-session, per-connection output coalescer.
pub struct SessionCoal {
    buf: Vec<u8>,
    hi_seq: u64,
    deadline: Option<Instant>,
    last_send: Option<Instant>,
    min_interval: Option<Duration>,
    awaiting_drain: bool,
}

impl SessionCoal {
    /// A coalescer for one attached session. `remote` enables the data
    /// channel's per-session frame rate cap.
    pub fn new(remote: bool) -> Self {
        Self {
            buf: Vec::new(),
            hi_seq: 0,
            deadline: None,
            last_send: None,
            min_interval: remote.then_some(REMOTE_MIN_INTERVAL),
            awaiting_drain: false,
        }
    }

    /// Buffer one output chunk (or report that a snapshot is needed).
    pub fn push_out(&mut self, tx: &ConnTx, seq: u64, chunk: &[u8]) -> PushOut {
        let now = Instant::now();
        if self.awaiting_drain {
            if tx.pending() > MAX_BYTES / 2 {
                return PushOut::Dropped;
            }
            self.awaiting_drain = false;
        }
        if tx.behind().is_some_and(|behind| behind > STALE) {
            self.reset();
            return PushOut::NeedSnapshot;
        }
        self.append(now, seq, chunk);
        if tx.pending().saturating_add(wire_bytes(self.buf.len())) > MAX_BYTES {
            self.reset();
            return PushOut::NeedSnapshot;
        }
        if self.buf.len() >= COALESCE_MAX {
            self.deadline = Some(now);
        }
        PushOut::Buffered
    }

    fn append(&mut self, now: Instant, seq: u64, chunk: &[u8]) {
        if self.buf.is_empty() {
            self.deadline = Some(now + COALESCE);
        }
        self.buf.extend_from_slice(chunk);
        self.hi_seq = seq;
    }

    /// When the forwarder should wake to flush, if it holds buffered output.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.buf.is_empty() {
            return None;
        }
        let mut at = self.deadline?;
        if let (Some(min), Some(last)) = (self.min_interval, self.last_send) {
            let rate_at = last + min;
            if rate_at > at {
                at = rate_at;
            }
        }
        Some(at)
    }

    /// Take the buffered bytes when a flush is due and the rate allows it.
    /// The buffer keeps its capacity for reuse, releasing it only when it is
    /// idle and above [`SHRINK_AT`].
    pub fn take_flush(&mut self) -> Option<(u64, Vec<u8>)> {
        if self.buf.is_empty() || !self.flush_ready() {
            return None;
        }
        let seq = self.hi_seq;
        let data = self.buf.clone();
        self.buf.clear();
        self.deadline = None;
        self.last_send = Some(Instant::now());
        if self.buf.capacity() > SHRINK_AT {
            self.buf = Vec::new();
        }
        Some((seq, data))
    }

    fn flush_ready(&self) -> bool {
        let now = Instant::now();
        if self.deadline.is_some_and(|deadline| now < deadline) {
            return false;
        }
        if let (Some(min), Some(last)) = (self.min_interval, self.last_send) {
            if now.duration_since(last) < min {
                return false;
            }
        }
        true
    }

    /// Record that a fresh snapshot was just queued: drop pending output and
    /// stay quiet until the consumer has drained the snapshot.
    pub fn mark_snapshot_sent(&mut self) {
        self.reset();
        self.awaiting_drain = true;
        self.last_send = Some(Instant::now());
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.deadline = None;
        if self.buf.capacity() > SHRINK_AT {
            self.buf = Vec::new();
        }
    }
}
