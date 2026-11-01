//! Live device connections, keyed by `device_id`.
//!
//! Moved out of [`super::relay`] so the relay client stays under the file
//! size gate; re-exported there, so `relay::ConnectionRegistry` keeps working.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

/// Live device connections, keyed by `device_id`.
#[derive(Clone, Default)]
pub struct ConnectionRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<CancellationToken>>>>,
}

impl ConnectionRegistry {
    /// Register a live connection for `device_id`; cancelling the returned
    /// token closes it.
    pub fn register(&self, device_id: &str) -> Arc<CancellationToken> {
        let token = Arc::new(CancellationToken::new());
        if let Ok(mut map) = self.inner.lock() {
            map.insert(device_id.to_string(), Arc::clone(&token));
        }
        token
    }

    /// Close one device's live connection, if any.
    pub fn close(&self, device_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            if let Some(token) = map.remove(device_id) {
                token.cancel();
            }
        }
    }

    /// Forget one peer's registration. A newer connection of the same device
    /// already replaced it, so only the exact token may be removed: the
    /// reconnect must survive the old peer's cleanup.
    pub fn release(&self, device_id: &str, token: &Arc<CancellationToken>) {
        if let Ok(mut map) = self.inner.lock() {
            if map
                .get(device_id)
                .is_some_and(|current| Arc::ptr_eq(current, token))
            {
                map.remove(device_id);
            }
        }
    }
}
