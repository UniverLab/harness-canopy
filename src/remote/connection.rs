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

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::sessions::protocol::{error_frame, hello_frame, ERR_REVOKED};
use crate::sessions::ws::SessionsState;
use crate::sessions::HubEvent;

use super::bridge::RemoteBridge;
use super::peer::{LinkState, Peer, PeerEvent};
use super::relay::{ConnectionRegistry, SignalTx};
use super::signaling::{self, MAX_DEVICE_PEERS};
use super::trust::Grant;

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
    /// Devices whose peer must hear `error{code:"revoked"}` before its
    /// channel closes. Marked by [`Self::close_revoked`], taken by the serve
    /// loop, forgotten by [`Self::remove`] — always under the `inner` lock
    /// first, so a mark never leaks into a later generation.
    revoked: Arc<Mutex<HashSet<String>>>,
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

    /// Close one device's peer now (a disconnect or a `bye`).
    pub fn close(&self, device_id: &str) {
        if let Ok(mut map) = self.inner.lock() {
            if let Some(handle) = map.remove(device_id) {
                handle.token.cancel();
            }
        }
    }

    /// Mark one device revoked and cancel its peer now, keeping the entry
    /// until the serve loop's cleanup removes it: the loop takes the mark
    /// and sends `error{code:"revoked"}` before the channel closes. No mark
    /// when the device has no live peer — there is no open channel to hear
    /// it, and nothing must leak into its next connection.
    ///
    /// Callers cancel through here first: the mark lands under the same
    /// locks, so a peer woken by the cancel always finds it. Cancelling the
    /// shared registry token beforehand would let the serve loop take (and
    /// miss) before the mark lands.
    pub fn close_revoked(&self, device_id: &str) {
        if let (Ok(map), Ok(mut revoked)) = (self.inner.lock(), self.revoked.lock()) {
            if let Some(handle) = map.get(device_id) {
                revoked.insert(device_id.to_string());
                handle.token.cancel();
            }
        }
    }

    /// Take one device's revoked mark, if set. The serve loop calls this as
    /// it ends, so only the revoked generation hears the error.
    pub fn take_revoked(&self, device_id: &str) -> bool {
        self.revoked
            .lock()
            .map(|mut revoked| revoked.remove(device_id))
            .unwrap_or(false)
    }

    /// Forget a peer that ended on its own — unless the device already
    /// reconnected and a newer handle took the slot. A revoked mark set
    /// after this peer passed its own check is forgotten with it, so the
    /// next generation of the same device starts unmarked.
    pub fn remove(&self, device_id: &str, token: &Arc<CancellationToken>) {
        if let (Ok(mut map), Ok(mut revoked)) = (self.inner.lock(), self.revoked.lock()) {
            if map
                .get(device_id)
                .is_some_and(|handle| Arc::ptr_eq(&handle.token, token))
            {
                map.remove(device_id);
                revoked.remove(device_id);
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
    /// What this peer's login may see, resolved when its offer verified.
    pub grant: Grant,
    /// Largest outbound frame for this peer, from its offer SDP (see
    /// [`peer_frame_cap`]): [`MAX_CHANNEL_FRAME`] for a desktop, less for a
    /// peer that advertised less.
    pub max_frame: usize,
}

/// Receive cap when the offer carries no usable `a=max-message-size`
/// (RFC 8841 §6: absent means 64 KiB).
const DEFAULT_REMOTE_MAX_FRAME: usize = 64 * 1024;

/// How long a revoked peer's farewell frame gets to leave before the
/// connection closes.
const REVOKED_FAREWELL_WAIT: Duration = Duration::from_millis(250);

/// The largest frame a peer accepts, from its offer SDP: the minimum of
/// every `a=max-message-size` it advertises (a zero advertises no limit and
/// is skipped), or the 64 KiB RFC default when it advertises nothing usable.
/// Never above [`MAX_CHANNEL_FRAME`]: the const stays the ceiling.
///
/// Why the SDP and not the SCTP association's negotiated max: the
/// association reconciles the remote advertisement with this stack's own
/// 64 KiB ceiling, so its minimum would cap a desktop peer that accepts the
/// full const. Our sends are bounded by what the remote receives — its own
/// advertisement — and Chrome advertises the full 256 KiB.
pub(crate) fn peer_frame_cap(offer_sdp: &str) -> usize {
    let mut cap = DEFAULT_REMOTE_MAX_FRAME;
    let mut seen = false;
    for line in offer_sdp.lines() {
        let Some(value) = line.trim().strip_prefix("a=max-message-size:") else {
            continue;
        };
        let Ok(advertised) = value.trim().parse::<usize>() else {
            continue;
        };
        if advertised == 0 {
            continue;
        }
        cap = if seen {
            cap.min(advertised)
        } else {
            advertised
        };
        seen = true;
    }
    MAX_CHANNEL_FRAME.min(cap)
}

/// Largest frame the data channel carries for a peer that negotiated at
/// least this much. Chrome advertises `a=max-message-size:262144` and
/// closes the channel on a bigger message, so one oversized answer used to
/// end the whole peer; a peer that negotiated less gets its own smaller cap
/// instead (see [`PeerSession::max_frame`).
pub(crate) const MAX_CHANNEL_FRAME: usize = 256 * 1024;

/// The frame to send for `frame`: itself when it fits `max_frame`,
/// otherwise an `error` answering the same `req` so the device stops
/// waiting for it.
fn fitting_frame(frame: String, max_frame: usize) -> String {
    if frame.len() <= max_frame {
        return frame;
    }
    let parsed: Option<serde_json::Value> = serde_json::from_str(&frame).ok();
    let field = |key: &str| {
        parsed
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    };
    let kind = field("t").unwrap_or_else(|| "?".to_string());
    tracing::warn!(
        "remote: dropping a {} byte `{kind}` frame over the {max_frame} byte channel limit",
        frame.len()
    );
    error_frame(
        field("req").as_deref(),
        "frame_too_large",
        &format!("the host's `{kind}` answer is too large for the data channel"),
    )
}

/// Timing for the serve watchdog: how often liveness is re-checked and how
/// long a sick link may linger. Production runs minutes; the reconnect
/// tests pass seconds.
#[derive(Debug, Clone, Copy)]
pub struct WatchdogConfig {
    /// How often the serve loop re-checks transport liveness.
    pub tick: Duration,
    /// How long `Disconnected` may last before the peer ends.
    pub disconnect_grace: Duration,
    /// How long a peer that never opened its channel may live.
    pub open_grace: Duration,
}

impl WatchdogConfig {
    /// The production timing: a 5 s check with 60 s graces.
    const fn production() -> Self {
        Self {
            tick: Duration::from_secs(5),
            disconnect_grace: Duration::from_secs(60),
            open_grace: Duration::from_secs(60),
        }
    }
}

/// Serve one established peer: relay its ICE, bridge its data channel, and
/// stop as soon as the channel closes or the device is revoked.
pub fn spawn(session: PeerSession) {
    spawn_with_config(session, WatchdogConfig::production());
}

fn spawn_with_config(mut session: PeerSession, config: WatchdogConfig) {
    tokio::spawn(async move {
        serve(&mut session, config).await;
    });
}

/// Test hook: serve one peer with shortened watchdog timing, so the
/// reconnect tests watch eviction in seconds instead of a minute.
#[cfg(test)]
pub fn spawn_with_watchdog(session: PeerSession, config: WatchdogConfig) {
    spawn_with_config(session, config);
}

async fn serve(session: &mut PeerSession, config: WatchdogConfig) {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let mut bridge = RemoteBridge::with_grant(session.state.clone(), session.grant.clone());
    let mut events = session.state.hub.subscribe_events();
    // Like the local socket, the first frame the device sees is `hello`; any
    // hub event produced before the channel exists is dropped (the device
    // resynchronises with `list`), never reordered around the greeting.
    let mut greeted = false;
    let started = Instant::now();
    let mut disconnected_since: Option<Instant> = None;
    let mut watchdog = tokio::time::interval(config.tick);
    // Every arm resolves to why the peer ends, if it does; one break point
    // below keeps the loop readable as arms are added.
    let ended = loop {
        let reason: Option<&'static str> = tokio::select! {
            _ = session.token.cancelled() => Some("revoked or closed by the device"),
            message = session.control.recv() => on_control(session, message).await.err(),
            frame = out_rx.recv() => on_out_frame(session, frame, greeted).await.err(),
            event = session.peer.next() => {
                on_peer_event(session, event, &mut greeted, &mut bridge, &out_tx)
                    .await
                    .err()
            }
            hub_event = events.recv() => {
                on_hub_event(&mut bridge, &out_tx, greeted, hub_event).err()
            }
            _ = watchdog.tick() => {
                watchdog_check(
                    session.peer.link_state(),
                    &mut disconnected_since,
                    greeted,
                    started,
                    config,
                )
            }
        };
        if let Some(ended) = reason {
            break ended;
        }
    };
    tracing::info!("remote: device {} disconnected: {ended}", session.device_id);
    send_revoked_farewell(session).await;
    bridge.abort();
    session.peer.close().await;
    session.peers.remove(&session.device_id, &session.token);
    session
        .connections
        .release(&session.device_id, &session.token);
}

/// One watchdog pass over the transport: `Some` reason ends the peer so its
/// slot frees through the existing `remove`, `None` keeps serving. A failed
/// or closed link ends now; a disconnected one gets its grace, then ends;
/// a peer whose channel never opened gets its own grace, then ends. Pure
/// over the reported link state so the reconnect tests pin every arm; the
/// integration zombies only ever hold `Up` links.
pub(crate) fn watchdog_check(
    link: LinkState,
    disconnected_since: &mut Option<Instant>,
    greeted: bool,
    started: Instant,
    config: WatchdogConfig,
) -> Option<&'static str> {
    match link {
        LinkState::Down => Some("the transport failed or closed"),
        LinkState::Disconnected => {
            let since = disconnected_since.get_or_insert_with(Instant::now);
            if since.elapsed() > config.disconnect_grace {
                Some("the transport stayed disconnected past its grace")
            } else {
                None
            }
        }
        LinkState::Up => {
            *disconnected_since = None;
            if !greeted && started.elapsed() > config.open_grace {
                Some("the channel never opened past its grace")
            } else {
                None
            }
        }
    }
}

/// Handle one relay-routed control message; `Err` carries why the peer ends.
async fn on_control(session: &PeerSession, message: Option<Control>) -> Result<(), &'static str> {
    match message {
        Some(Control::Ice(candidate)) => {
            let _ = session.peer.add_remote_candidate(&candidate).await;
            Ok(())
        }
        None => Err("replaced by a newer connection"),
    }
}

/// Forward one queued outbound frame; `Err` carries why the peer ends.
async fn on_out_frame(
    session: &PeerSession,
    frame: Option<String>,
    greeted: bool,
) -> Result<(), &'static str> {
    let Some(frame) = frame else {
        return Ok(());
    };
    if !greeted {
        return Ok(());
    }
    session
        .peer
        .send_text(&fitting_frame(frame, session.max_frame))
        .await
        .map_err(|_| "the data channel refused a write")
}

/// Handle one hub receive; `Err` carries why the peer ends.
fn on_hub_event(
    bridge: &mut RemoteBridge,
    out_tx: &mpsc::UnboundedSender<String>,
    greeted: bool,
    hub_event: Result<HubEvent, tokio::sync::broadcast::error::RecvError>,
) -> Result<(), &'static str> {
    match hub_event {
        Ok(event) => {
            forward_hub_event(bridge, out_tx, greeted, event);
            Ok(())
        }
        Err(tokio::sync::broadcast::error::RecvError::Closed) => Err("the hub stopped"),
        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => Ok(()),
    }
}

/// Forward one hub event to the bridge. Held until the greeting is
/// queued: nothing may overtake `hello`, and nothing is attached yet to
/// miss a removal. Pre-greeting shelf events still feed the hidden set, so
/// a terminal file that lands during the handshake hides its later removal.
fn forward_hub_event(
    bridge: &mut RemoteBridge,
    out_tx: &mpsc::UnboundedSender<String>,
    greeted: bool,
    event: HubEvent,
) {
    if greeted {
        bridge.handle_event(event, out_tx);
    } else {
        bridge.note_hidden(&event);
    }
}

/// A revoke lands as a cancelled token; the device hears why before the
/// channel closes, so its app can say the device was removed.
async fn send_revoked_farewell(session: &PeerSession) {
    if session.peers.take_revoked(&session.device_id) {
        let _ = session
            .peer
            .send_text(&error_frame(None, ERR_REVOKED, "this device was revoked"))
            .await;
        // The farewell gets a moment to leave: shutting the connection down
        // in the same instant drops it from the SCTP queue on some stacks.
        tokio::time::sleep(REVOKED_FAREWELL_WAIT).await;
    }
}

/// Handle one transport event; `Err` carries why the peer ends.
async fn on_peer_event(
    session: &PeerSession,
    event: Option<PeerEvent>,
    greeted: &mut bool,
    bridge: &mut RemoteBridge,
    out_tx: &mpsc::UnboundedSender<String>,
) -> Result<(), &'static str> {
    match event {
        Some(PeerEvent::Open) => {
            if !*greeted {
                *greeted = true;
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
                bridge.handle_text(&text, out_tx).await;
            }
        }
        Some(PeerEvent::Closed) | None => return Err("the channel or the connection closed"),
    }
    Ok(())
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
