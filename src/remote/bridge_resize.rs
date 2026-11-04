//! The remote bridge's `resize` arm: only the host owner's own paired
//! device may size a session's PTY.
//!
//! A guest — any grant, full or narrow — answers `forbidden` for a session
//! it can see and `not_found` for one outside its grant (the scope's usual
//! rule, so a refusal never confirms an ungranted session exists). The
//! owner's sizes are clamped to [`COLS`] x [`ROWS`] and applied through
//! [`SessionHub::resize_session`], so every other attached client, the
//! local TUI included, hears `resized`. Resizes of one session closer than
//! [`MIN_GAP`] apart are coalesced: the latest one is applied when the gap
//! has passed.

use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::sessions::protocol::{error_frame, ERR_FORBIDDEN, ERR_NOT_FOUND};
use crate::sessions::ws::{no_such_session, OutTx};
use crate::sessions::SessionHub;

use super::RemoteBridge;

/// Columns a remote owner device may set.
pub(crate) const COLS: RangeInclusive<u16> = 20..=500;
/// Rows a remote owner device may set.
pub(crate) const ROWS: RangeInclusive<u16> = 5..=200;
/// Minimum gap between two applied resizes of one session from one device.
pub(crate) const MIN_GAP: Duration = Duration::from_millis(100);

/// One session's resize throttle for this device.
#[derive(Debug, Default)]
pub(crate) struct ResizeGate {
    /// When the last resize was applied.
    last: Option<Instant>,
    /// The newest size waiting for the gap to pass.
    pending: Option<(u16, u16)>,
    /// Whether a deferred apply is already scheduled.
    scheduled: bool,
}

/// Clamp a requested size into [`COLS`] x [`ROWS`].
pub(crate) fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (
        cols.clamp(*COLS.start(), *COLS.end()),
        rows.clamp(*ROWS.start(), *ROWS.end()),
    )
}

/// Apply `(cols, rows)` to session `id` if it is still live.
fn apply(hub: &SessionHub, id: &str, cols: u16, rows: u16) {
    if let Some(session) = hub.get(id) {
        hub.resize_session(&session, cols, rows);
    }
}

impl RemoteBridge {
    /// `resize{id,cols,rows}`: the owner device sizes the PTY; anyone else is
    /// refused. No answer frame on success — `resized` is the confirmation.
    pub(crate) fn handle_resize(&mut self, id: &str, cols: u16, rows: u16, out: &OutTx) {
        if !self.scope.granted(&self.state.hub, None, id, out) {
            return;
        }
        if !self.scope.is_owner() {
            let _ = out.send(error_frame(
                None,
                ERR_FORBIDDEN,
                &format!("resize is only available to the host owner's devices ('{id}')"),
            ));
            return;
        }
        if self.state.hub.get(id).is_none() {
            let _ = out.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
            return;
        }
        let (cols, rows) = clamp_size(cols, rows);
        let gate = Arc::clone(self.resize_gates.entry(id.to_string()).or_default());
        let Ok(mut state) = gate.lock() else {
            return;
        };
        let now = Instant::now();
        let due = state
            .last
            .map(|last| last + MIN_GAP)
            .filter(|due| *due > now);
        let Some(due) = due else {
            state.last = Some(now);
            state.pending = None;
            drop(state);
            apply(&self.state.hub, id, cols, rows);
            return;
        };
        state.pending = Some((cols, rows));
        if state.scheduled {
            return;
        }
        state.scheduled = true;
        drop(state);
        let hub = self.state.hub.clone();
        let id = id.to_string();
        let gate = Arc::clone(&gate);
        let task = tokio::spawn(async move {
            tokio::time::sleep_until(due.into()).await;
            let size = {
                let Ok(mut state) = gate.lock() else {
                    return;
                };
                state.scheduled = false;
                state.last = Some(Instant::now());
                state.pending.take()
            };
            if let Some((cols, rows)) = size {
                apply(&hub, &id, cols, rows);
            }
        });
        self.resize_tasks.retain(|task| !task.is_finished());
        self.resize_tasks.push(task);
    }
}

/// Shared per-session gates, keyed by session id.
pub(crate) type ResizeGates = std::collections::HashMap<String, Arc<Mutex<ResizeGate>>>;
