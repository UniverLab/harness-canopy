//! Live device peers: at most four, each revocable, each bridged to the hub.
//!
//! A peer registers here only after its offer verified and its answer was
//! built; the fifth offer gets `busy`. Every peer holds the
//! [`ConnectionRegistry`] token for its device, so a revoke (CLI or another
//! process) closes the peer within a second, and the data channel's close
//! detaches every session it attached.
//!
//! `ice` and `bye` bodies carry no device id: the relay envelope's `from` is
//! the address, so every handle remembers the connection id it answers to.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::sessions::protocol::hello_frame;
use crate::sessions::ws::SessionsState;

use super::bridge::RemoteBridge;
use super::peer::{Peer, PeerEvent};
use super::relay::{ConnectionRegistry, SignalTx};
use super::signaling::{self, MAX_DEVICE_PEERS};

/// What the relay routes into one live peer after the answer.
#[derive(Debug)]
pub enum Control {
    /// An `ice` candidate from the device.
    Ice(String),
}

struct PeerHandle {
    /// Relay connection id this peer answers `ice`/`bye` from.
    from: String,
    token: Arc<CancellationToken>,
    control: mpsc::UnboundedSender<Control>,
}

/// Live device peers, keyed by `device_id`.
#[derive(Clone, Default)]
pub struct DevicePeers {
    inner: Arc<Mutex<HashMap<String, PeerHandle>>>,
}

impl DevicePeers {
    /// How many peers are live. Status-surface API alongside [`Self::live_ids`].
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.lock().map(|map| map.len()).unwrap_or(0)
    }

    /// Whether no peer is live.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The ids of the devices with a live peer.
    pub fn live_ids(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Register one peer. `false` means [`MAX_DEVICE_PEERS`] other devices are
    /// already live (the caller answers `busy`). A device that reconnects
    /// replaces its own handle, so a reconnect never consumes a second slot.
    pub fn register(
        &self,
        device_id: &str,
        from: &str,
        token: Arc<CancellationToken>,
        control: mpsc::UnboundedSender<Control>,
    ) -> bool {
        let Ok(mut map) = self.inner.lock() else {
            return false;
        };
        if map.len() >= MAX_DEVICE_PEERS && !map.contains_key(device_id) {
            return false;
        }
        map.insert(
            device_id.to_string(),
            PeerHandle {
                from: from.to_string(),
                token,
                control,
            },
        );
        true
    }

    /// Route one control body to the peer that answers on relay connection
    /// `from`.
    pub fn route(&self, from: &str, control: Control) -> bool {
        let Ok(map) = self.inner.lock() else {
            return false;
        };
        map.values()
            .find(|handle| handle.from == from)
            .is_some_and(|handle| handle.control.send(control).is_ok())
    }

    /// Close the peer on relay connection `from` now (`bye`).
    pub fn close_from(&self, from: &str) {
        if let Ok(mut map) = self.inner.lock() {
            let device_id = map
                .iter()
                .find(|(_, handle)| handle.from == from)
                .map(|(device_id, _)| device_id.clone());
            if let Some(device_id) = device_id {
                if let Some(handle) = map.remove(&device_id) {
                    handle.token.cancel();
                }
            }
        }
    }

    /// Close one device's peer now (a revoke).
    pub fn close(&self, device_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            if let Some(handle) = map.remove(device_id) {
                handle.token.cancel();
            }
        }
    }

    /// Forget a peer that ended on its own — unless the device already
    /// reconnected and a newer handle took the slot.
    pub fn remove(&self, device_id: &str, token: &Arc<CancellationToken>) {
        if let Ok(mut map) = self.inner.lock() {
            if map
                .get(device_id)
                .is_some_and(|handle| Arc::ptr_eq(&handle.token, token))
            {
                map.remove(device_id);
            }
        }
    }
}

/// Everything one established peer needs to run.
pub struct PeerSession {
    /// The transport.
    pub peer: Peer,
    /// The device that signed the offer.
    pub device_id: String,
    /// Hub state the bridge answers from.
    pub state: Arc<SessionsState>,
    /// Live peers, so the slot is released when the channel closes.
    pub peers: DevicePeers,
    /// Registry the revoke token came from.
    pub connections: ConnectionRegistry,
    /// Cancelled when the device is revoked.
    pub token: Arc<CancellationToken>,
    /// `ice`/`bye` routed by the relay after the answer.
    pub control: mpsc::UnboundedReceiver<Control>,
    /// Where local ICE candidates go.
    pub signal: SignalTx,
    /// Relay peer id to address the candidates to.
    pub from: String,
}

/// Serve one established peer: relay its ICE, bridge its data channel, and
/// stop as soon as the channel closes or the device is revoked.
pub fn spawn(mut session: PeerSession) {
    tokio::spawn(async move {
        serve(&mut session).await;
    });
}

async fn serve(session: &mut PeerSession) {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let mut bridge = RemoteBridge::new(session.state.clone());
    let mut events = session.state.hub.subscribe_events();
    // Like the local socket, the first frame the device sees is `hello`; any
    // hub event produced before the channel exists is dropped (the device
    // resynchronises with `list`), never reordered around the greeting.
    let mut greeted = false;
    loop {
        tokio::select! {
            _ = session.token.cancelled() => break,
            message = session.control.recv() => match message {
                Some(Control::Ice(candidate)) => {
                    let _ = session.peer.add_remote_candidate(&candidate).await;
                }
                None => break,
            },
            frame = out_rx.recv() => {
                if let Some(frame) = frame {
                    if greeted && session.peer.send_text(&frame).await.is_err() {
                        break;
                    }
                }
            }
            event = session.peer.next() => match event {
                Some(PeerEvent::Open) => {
                    if !greeted {
                        greeted = true;
                        let _ = out_tx.send(hello_frame(session.state.hub.host_label()));
                    }
                }
                Some(PeerEvent::LocalCandidate(candidate)) => {
                    let _ = session
                        .signal
                        .send((session.from.clone(), signaling::ice_body(&candidate)));
                }
                Some(PeerEvent::Channel(bytes)) => {
                    if let Ok(text) = String::from_utf8(bytes) {
                        bridge.handle_text(&text, &out_tx);
                    }
                }
                Some(PeerEvent::Closed) | None => break,
            },
            hub_event = events.recv() => match hub_event {
                Ok(event) => {
                    // Held until the greeting is queued: nothing may overtake
                    // `hello`, and nothing is attached yet to miss a removal.
                    if greeted {
                        bridge.handle_event(event, &out_tx);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            },
        }
    }
    bridge.abort();
    session.peer.close().await;
    session.peers.remove(&session.device_id, &session.token);
    session
        .connections
        .release(&session.device_id, &session.token);
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
