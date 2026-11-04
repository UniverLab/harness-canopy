//! The hub's handle on the session shelf: `wire` once at daemon startup,
//! `reap` on every session removal. It lives here — not in `hub.rs`, which
//! is at the file-size limit — with the hub keeping only the field and two
//! short calls.

use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;

use super::hub::{HubEvent, SessionHub};
use super::shelf::ShelfStore;

/// The shelf attached to a hub, if daemon wiring attached one. Absent in
/// the stdio server (no hub there) and in tests that never touch the shelf.
#[derive(Default)]
pub struct ShelfLink {
    store: Mutex<Option<Arc<ShelfStore>>>,
}

impl ShelfLink {
    /// Attach the shelf: its `shelf_added` / `shelf_removed` events ride
    /// the hub bus, and `kill` reaps it through [`Self::reap`].
    pub fn wire(&self, events: broadcast::Sender<HubEvent>, shelf: Arc<ShelfStore>) {
        shelf.attach_bus(events);
        *self.store.lock().unwrap_or_else(|e| e.into_inner()) = Some(shelf);
    }

    /// Run the shelf's lifecycle checks for a session that just left and
    /// broadcast every reaped file as the hub's `shelf_removed`. A hub with
    /// no shelf attached is a no-op.
    pub fn reap_and_emit(&self, hub: &SessionHub) {
        let store = self.store.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(store) = store {
            for file_id in store.maintenance() {
                hub.emit(HubEvent::ShelfRemoved { file_id });
            }
        }
    }
}
