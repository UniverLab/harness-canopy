//! WebRTC transport for one paired device; the only module importing `webrtc`.
//!
//! The relay only carries signaling: the host and the device then speak over a
//! direct `SCTP` data channel, authenticated end to end by the DTLS fingerprint
//! inside the signed SDP. This module owns that half of the protocol:
//!
//! - [`fetch_ice_servers`] asks the relay for a short-lived TURN credential.
//! - [`Peer::answer_offer`] is the host side: a device's offer goes in, the
//!   answer SDP comes out, and the accepted channel arrives through
//!   [`Peer::next`] as [`PeerEvent::Channel`].
//! - [`Peer::offer`] is the device side, used by tests to drive a real peer.
//!
//! Only the channel labelled `canopy-session-v1` is accepted; a device that
//! opens anything else has it closed without a reply.
//!
//! The connection's own callbacks cannot await the consumer, so they push into
//! a bounded queue with `try_send` and drop on overflow: a slow reader must
//! never stall the `webrtc` driver.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;
use tokio::sync::{mpsc, Mutex};
use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceGatheringState, RTCIceServer, RTCPeerConnectionIceEvent,
    RTCPeerConnectionState, RTCSessionDescription,
};

/// The one data channel label this transport accepts.
pub const CHANNEL_LABEL: &str = "canopy-session-v1";

/// How long to wait for ICE gathering before answering with what we have.
const GATHERING_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the relay's TURN credential response.
const RELAY_TIMEOUT: Duration = Duration::from_secs(10);

/// How often to re-check for a local description while ICE gathers.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Depth of the queue between the `webrtc` callbacks and [`Peer::next`].
const EVENT_QUEUE: usize = 64;

/// One STUN/TURN server as the relay hands it out.
#[derive(Debug, Clone)]
pub struct IceServer {
    /// `stun:`/`turn:`/`turns:` URLs, in preference order.
    pub urls: Vec<String>,
    /// TURN username, absent for STUN servers.
    pub username: Option<String>,
    /// TURN credential, absent for STUN servers.
    pub credential: Option<String>,
}

impl IceServer {
    /// Converts to the `webrtc` form, which spells absent credentials as empty.
    fn into_webrtc(self) -> RTCIceServer {
        RTCIceServer {
            urls: self.urls,
            username: self.username.unwrap_or_default(),
            credential: self.credential.unwrap_or_default(),
        }
    }
}

/// `{ "iceServers": [ { "urls": [...], "username": "...", "credential": "..." } ] }`
#[derive(Debug, Deserialize)]
struct TurnResponse {
    #[serde(rename = "iceServers")]
    ice_servers: Vec<IceServerBody>,
}

/// One entry of [`TurnResponse`]; `urls` is either a list or a single string.
#[derive(Debug, Deserialize)]
struct IceServerBody {
    #[serde(default)]
    urls: Option<Urls>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    credential: Option<String>,
}

/// Both spellings the WebRTC specification allows for `urls`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Urls {
    /// `"urls": ["turn:...", "stun:..."]`
    Many(Vec<String>),
    /// `"urls": "turn:..."`
    One(String),
}

impl Urls {
    /// Flattens either spelling into a list.
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::Many(urls) => urls,
            Self::One(url) => vec![url],
        }
    }
}

/// Asks the relay for ICE servers: `GET <relay_url>/turn` with the host token.
///
/// A non-200 response is an error rather than an empty list, because an empty
/// list would silently downgrade the session to host candidates and only fail
/// later, at connection time.
///
/// # Errors
///
/// Returns an error when the request fails, the status is not 200, or the body
/// is not the expected JSON shape.
pub async fn fetch_ice_servers(relay_url: &str, token: &str) -> Result<Vec<IceServer>> {
    let url = format!("{}/turn", relay_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(RELAY_TIMEOUT)
        .build()
        .context("build the relay HTTP client")?;
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(anyhow!("GET {url} returned HTTP {}", response.status()));
    }
    let body: TurnResponse = response
        .json()
        .await
        .with_context(|| format!("decode the ICE servers in the GET {url} response"))?;
    Ok(body
        .ice_servers
        .into_iter()
        .filter_map(|server| {
            let urls = server.urls.map(Urls::into_vec).unwrap_or_default();
            if urls.is_empty() {
                return None;
            }
            Some(IceServer {
                urls,
                username: server.username,
                credential: server.credential,
            })
        })
        .collect())
}

/// What a [`Peer`] reports as the session progresses.
#[derive(Debug)]
pub enum PeerEvent {
    /// A local ICE candidate to forward to the other side verbatim.
    LocalCandidate(String),
    /// Inbound bytes from the accepted data channel.
    Channel(Vec<u8>),
    /// The accepted channel, or the whole connection, is gone.
    Closed,
}

/// Boxed future returned by the callbacks of [`PeerConnectionEventHandler`].
///
/// `webrtc` declares those callbacks through `async-trait`, and this crate does
/// not depend on it, so the implementations below spell the desugared
/// signature out instead of using the attribute macro.
type CallbackFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// The `webrtc` event handler, forwarding every event into the [`Peer`] queue.
struct Handler {
    /// Queue shared with [`Peer::next`].
    events: mpsc::Sender<PeerEvent>,
    /// Slot holding the accepted data channel, shared with [`Peer`].
    channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    /// Fires once, when ICE gathering completes.
    gathered: mpsc::Sender<()>,
}

impl PeerConnectionEventHandler for Handler {
    fn on_ice_candidate<'a, 'b>(&'a self, event: RTCPeerConnectionIceEvent) -> CallbackFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            // `webrtc` reports a parsed candidate here; the SDP attribute string
            // the other side has to add verbatim comes from its `to_json()`.
            let candidate = match event.candidate.to_json() {
                Ok(init) => init.candidate,
                Err(error) => {
                    tracing::debug!("dropping an unsignallable ICE candidate: {error}");
                    return;
                }
            };
            // The end-of-candidates signal arrives as an empty string; there is
            // nothing for the other side to add in that case.
            if candidate.is_empty() {
                return;
            }
            let _ = self.events.try_send(PeerEvent::LocalCandidate(candidate));
        })
    }

    fn on_ice_gathering_state_change<'a, 'b>(
        &'a self,
        state: RTCIceGatheringState,
    ) -> CallbackFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            if state == RTCIceGatheringState::Complete {
                let _ = self.gathered.try_send(());
            }
        })
    }

    fn on_connection_state_change<'a, 'b>(
        &'a self,
        state: RTCPeerConnectionState,
    ) -> CallbackFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            // `Disconnected` is often transient and recovers on its own; only
            // the terminal states end the session.
            if matches!(
                state,
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
            ) {
                let _ = self.events.try_send(PeerEvent::Closed);
            }
        })
    }

    fn on_data_channel<'a, 'b>(&'a self, data_channel: Arc<dyn DataChannel>) -> CallbackFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            let label = data_channel.label().await.unwrap_or_default();
            if label != CHANNEL_LABEL {
                // A device may open whatever it likes; anything but the session
                // channel is closed unanswered.
                let _ = data_channel.close().await;
                return;
            }
            attach_channel(&self.channel, &self.events, data_channel).await;
        })
    }
}

/// Publishes an accepted channel and starts forwarding its inbound messages.
async fn attach_channel(
    slot: &Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    events: &mpsc::Sender<PeerEvent>,
    data_channel: Arc<dyn DataChannel>,
) {
    *slot.lock().await = Some(data_channel.clone());
    spawn_pump(data_channel, slot.clone(), events.clone());
}

/// Drains a data channel into `events` until it closes.
fn spawn_pump(
    data_channel: Arc<dyn DataChannel>,
    slot: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    events: mpsc::Sender<PeerEvent>,
) {
    tokio::spawn(async move {
        while let Some(event) = data_channel.poll().await {
            match event {
                DataChannelEvent::OnMessage(message) => {
                    if events
                        .send(PeerEvent::Channel(message.data.to_vec()))
                        .await
                        .is_err()
                    {
                        // Nobody is reading any more; the peer is being dropped.
                        return;
                    }
                }
                DataChannelEvent::OnClose => break,
                _ => {}
            }
        }
        // Only the channel that is still in the slot reports the closure: a
        // replaced channel must not end the session that replaced it.
        let was_current = {
            let mut guard = slot.lock().await;
            let current = guard
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &data_channel));
            if current {
                *guard = None;
            }
            current
        };
        if was_current {
            let _ = events.send(PeerEvent::Closed).await;
        }
    });
}

/// One end of a paired session.
pub struct Peer {
    /// The connection that owns the sockets and the DTLS transport.
    pc: Arc<dyn PeerConnection>,
    /// Events the callbacks and the channel pump push into. Behind a mutex
    /// so `next` can borrow `&self` and run beside `send_text`.
    events: tokio::sync::Mutex<mpsc::Receiver<PeerEvent>>,
    /// The accepted (host) or locally created (device) channel, once there is one.
    channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
}

impl Peer {
    /// Host side: answers a device's offer and returns the answer SDP.
    ///
    /// The peer never creates a data channel of its own; it waits for the
    /// device's `canopy-session-v1` channel and reports it through
    /// [`next`](Self::next).
    ///
    /// # Errors
    ///
    /// Returns an error when the offer SDP does not parse, when the connection
    /// cannot be built, or when negotiation fails.
    pub async fn answer_offer(servers: Vec<IceServer>, offer_sdp: &str) -> Result<(String, Peer)> {
        let wiring = Wiring::build(servers).await?;
        let offer =
            RTCSessionDescription::offer(offer_sdp.to_string()).context("parse the offer SDP")?;
        wiring
            .pc
            .set_remote_description(offer)
            .await
            .context("set the remote offer")?;
        let answer = wiring
            .pc
            .create_answer(None)
            .await
            .context("create the answer")?;
        wiring
            .pc
            .set_local_description(answer)
            .await
            .context("set the local answer")?;
        answer_with_gathered_sdp(wiring).await
    }

    /// Device side: opens the `label` data channel and returns the offer SDP.
    ///
    /// The channel is ordered and reliable, matching what the host accepts.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection cannot be built or when the offer
    /// cannot be created and applied.
    #[allow(dead_code)]
    pub async fn offer(servers: Vec<IceServer>, label: &str) -> Result<(String, Peer)> {
        let wiring = Wiring::build(servers).await?;
        let options = RTCDataChannelInit {
            ordered: true,
            ..Default::default()
        };
        let data_channel = wiring
            .pc
            .create_data_channel(label, Some(options))
            .await
            .context("create the data channel")?;
        // The device owns the channel it opened, so its pump is started here
        // rather than from `on_data_channel`, which only sees remote channels.
        attach_channel(&wiring.channel, &wiring.events_tx, data_channel).await;
        let offer = wiring
            .pc
            .create_offer(None)
            .await
            .context("create the offer")?;
        wiring
            .pc
            .set_local_description(offer)
            .await
            .context("set the local offer")?;
        answer_with_gathered_sdp(wiring).await
    }

    /// Device side: applies the host's answer SDP, completing the handshake.
    ///
    /// A browser device does this itself; a peer built by [`offer`](Self::offer)
    /// needs it, so it is test-facing like `offer` is.
    ///
    /// # Errors
    ///
    /// Returns an error when the answer SDP does not parse or the connection
    /// rejects it as the remote description.
    #[allow(dead_code)]
    pub async fn accept_answer(&self, answer_sdp: &str) -> Result<()> {
        let answer = RTCSessionDescription::answer(answer_sdp.to_string())
            .context("parse the answer SDP")?;
        self.pc
            .set_remote_description(answer)
            .await
            .context("set the remote answer")
    }

    /// Next event from the connection, or `None` once it has been dropped.
    pub async fn next(&self) -> Option<PeerEvent> {
        self.events.lock().await.recv().await
    }

    /// Adds a candidate received from the other side.
    ///
    /// An empty candidate is the end-of-candidates marker and is ignored.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection rejects the candidate.
    pub async fn add_remote_candidate(&self, candidate: &str) -> Result<()> {
        if candidate.trim().is_empty() {
            return Ok(());
        }
        let init = RTCIceCandidateInit {
            candidate: candidate.to_string(),
            ..Default::default()
        };
        self.pc
            .add_ice_candidate(init)
            .await
            .context("add a remote ICE candidate")
    }

    /// Sends one text frame on the accepted channel.
    ///
    /// # Errors
    ///
    /// Returns an error when no channel is accepted yet, or the channel is no
    /// longer open.
    pub async fn send_text(&self, text: &str) -> Result<()> {
        let data_channel = self.channel.lock().await.clone();
        let data_channel = data_channel.ok_or_else(|| anyhow!("the data channel is not open"))?;
        data_channel
            .send_text(text)
            .await
            .context("send on the data channel")
    }

    /// Closes the channel and the connection; safe to call more than once.
    pub async fn close(&self) {
        let data_channel = self.channel.lock().await.clone();
        if let Some(data_channel) = data_channel {
            let _ = data_channel.close().await;
        }
        let _ = self.pc.close().await;
    }
}

/// The pieces [`Peer`] is assembled from, kept separate so ICE gathering can
/// borrow them before the `Peer` exists.
struct Wiring {
    /// The connection.
    pc: Arc<dyn PeerConnection>,
    /// Receiver handed to the finished [`Peer`].
    events: mpsc::Receiver<PeerEvent>,
    /// Sender clone used to start channel pumps after the handler owns one.
    events_tx: mpsc::Sender<PeerEvent>,
    /// Channel slot shared with the handler.
    channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    /// Fires when ICE gathering completes.
    gathered: mpsc::Receiver<()>,
}

impl Wiring {
    /// Builds the connection and its handler.
    async fn build(servers: Vec<IceServer>) -> Result<Self> {
        let (events_tx, events) = mpsc::channel(EVENT_QUEUE);
        let (gathered, gathered_rx) = mpsc::channel(1);
        let channel = Arc::new(Mutex::new(None));
        let handler = Arc::new(Handler {
            events: events_tx.clone(),
            channel: channel.clone(),
            gathered,
        });
        let configuration = RTCConfigurationBuilder::new()
            .with_ice_servers(servers.into_iter().map(IceServer::into_webrtc).collect())
            .build();
        // The wildcard binds every interface *except* loopback and link-local;
        // the explicit loopback socket is what lets a device on this host —
        // including the loopback tests — reach the peer.
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(configuration)
                .with_handler(handler)
                .with_udp_addrs(vec!["0.0.0.0:0", "127.0.0.1:0"])
                .build()
                .await
                .context("build the peer connection")?,
        );
        Ok(Self {
            pc,
            events,
            events_tx,
            channel,
            gathered: gathered_rx,
        })
    }

    /// Splits into the returned SDP and the finished [`Peer`].
    fn into_peer(self) -> Peer {
        Peer {
            pc: self.pc,
            events: tokio::sync::Mutex::new(self.events),
            channel: self.channel,
        }
    }
}

/// Waits for ICE gathering (bounded) and returns the local SDP plus the peer.
async fn answer_with_gathered_sdp(mut wiring: Wiring) -> Result<(String, Peer)> {
    // Bounded on purpose: a peer that never finishes gathering (a TURN server
    // that stays silent, say) must not hang the pairing, and the candidates
    // gathered so far still travel in the SDP.
    let _ = tokio::time::timeout(GATHERING_TIMEOUT, wiring.gathered.recv()).await;
    let sdp = local_sdp(&wiring.pc).await?;
    Ok((sdp, wiring.into_peer()))
}

/// Waits until the local description is available, bounded by
/// [`GATHERING_TIMEOUT`].
async fn local_sdp(pc: &Arc<dyn PeerConnection>) -> Result<String> {
    let wait = async {
        loop {
            if let Some(description) = pc.local_description().await {
                return description.sdp;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    };
    tokio::time::timeout(GATHERING_TIMEOUT, wait)
        .await
        .map_err(|_| anyhow!("no local description after ICE gathering"))
}

/// Test-facing device end of the handshake: what a browser does — open the
/// session channel, offer, apply the answer — built directly on `webrtc`.
///
/// [`Peer`]'s offer-side wiring is exercised through the host half in the
/// loopback tests; this end exists so two real peers can meet in one process
/// and the bridge can be driven end to end. `webrtc` stays imported here only.
#[cfg(test)]
pub mod device_side {
    use std::sync::Arc;

    use anyhow::{Context as _, Result};
    use webrtc::data_channel::{DataChannel, DataChannelEvent, RTCDataChannelInit};
    use webrtc::peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceCandidateInit,
        RTCIceGatheringState, RTCPeerConnectionState, RTCSessionDescription,
    };

    use super::{CallbackFuture, CHANNEL_LABEL, GATHERING_TIMEOUT, POLL_INTERVAL};

    /// One event from the device end.
    #[derive(Debug)]
    pub enum DeviceEvent {
        /// A local ICE candidate to hand to the host verbatim.
        Candidate(String),
        /// One inbound frame on the session channel.
        Frame(Vec<u8>),
        /// The channel, or the connection, is gone.
        Closed,
    }

    struct Handler {
        events: tokio::sync::mpsc::Sender<DeviceEvent>,
        gathered: tokio::sync::mpsc::Sender<()>,
    }

    impl PeerConnectionEventHandler for Handler {
        fn on_ice_candidate<'a, 'b>(
            &'a self,
            event: webrtc::peer_connection::RTCPeerConnectionIceEvent,
        ) -> CallbackFuture<'b>
        where
            'a: 'b,
            Self: 'b,
        {
            Box::pin(async move {
                if let Ok(init) = event.candidate.to_json() {
                    if !init.candidate.is_empty() {
                        let _ = self.events.try_send(DeviceEvent::Candidate(init.candidate));
                    }
                }
            })
        }

        fn on_ice_gathering_state_change<'a, 'b>(
            &'a self,
            state: RTCIceGatheringState,
        ) -> CallbackFuture<'b>
        where
            'a: 'b,
            Self: 'b,
        {
            Box::pin(async move {
                if state == RTCIceGatheringState::Complete {
                    let _ = self.gathered.try_send(());
                }
            })
        }

        fn on_connection_state_change<'a, 'b>(
            &'a self,
            state: RTCPeerConnectionState,
        ) -> CallbackFuture<'b>
        where
            'a: 'b,
            Self: 'b,
        {
            Box::pin(async move {
                if matches!(
                    state,
                    RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
                ) {
                    let _ = self.events.try_send(DeviceEvent::Closed);
                }
            })
        }
    }

    /// The device half of one session: connection, channel, event queue.
    pub struct DeviceSide {
        pc: Arc<dyn PeerConnection>,
        channel: Arc<dyn DataChannel>,
        events: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<DeviceEvent>>,
    }

    impl DeviceSide {
        /// Open the session channel, set the local offer, return the offer SDP.
        ///
        /// # Errors
        ///
        /// Returns an error when the connection or the channel cannot be built
        /// or the offer cannot be created and applied.
        pub async fn offer() -> Result<(String, Self)> {
            let (events_tx, events) = tokio::sync::mpsc::channel(64);
            let (gathered, mut gathered_rx) = tokio::sync::mpsc::channel(1);
            let pump_events = events_tx.clone();
            let handler = Arc::new(Handler {
                events: events_tx,
                gathered,
            });
            let pc: Arc<dyn PeerConnection> = Arc::new(
                PeerConnectionBuilder::new()
                    .with_handler(handler)
                    .with_udp_addrs(vec!["127.0.0.1:0"])
                    .build()
                    .await
                    .context("build the device peer connection")?,
            );
            let options = RTCDataChannelInit {
                ordered: true,
                ..Default::default()
            };
            let channel = pc
                .create_data_channel(CHANNEL_LABEL, Some(options))
                .await
                .context("create the session channel")?;
            let pump_channel = Arc::clone(&channel);
            tokio::spawn(async move {
                while let Some(event) = pump_channel.poll().await {
                    match event {
                        DataChannelEvent::OnMessage(message) => {
                            if pump_events
                                .send(DeviceEvent::Frame(message.data.to_vec()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                        DataChannelEvent::OnClose => break,
                        _ => {}
                    }
                }
                let _ = pump_events.send(DeviceEvent::Closed).await;
            });
            let offer = pc
                .create_offer(None)
                .await
                .context("create the device offer")?;
            pc.set_local_description(offer)
                .await
                .context("set the device local offer")?;
            let _ = tokio::time::timeout(GATHERING_TIMEOUT, gathered_rx.recv()).await;
            let sdp = tokio::time::timeout(GATHERING_TIMEOUT, async {
                loop {
                    if let Some(description) = pc.local_description().await {
                        return description.sdp;
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("no local offer after ICE gathering"))?;
            Ok((
                sdp,
                Self {
                    pc,
                    channel,
                    events: tokio::sync::Mutex::new(events),
                },
            ))
        }

        /// Apply the host's answer, completing the handshake.
        ///
        /// # Errors
        ///
        /// Returns an error when the answer does not parse or is rejected.
        pub async fn accept(&self, answer_sdp: &str) -> Result<()> {
            let answer = RTCSessionDescription::answer(answer_sdp.to_string())
                .context("parse the answer SDP")?;
            self.pc
                .set_remote_description(answer)
                .await
                .context("set the remote answer")
        }

        /// Next event from the device end, or `None` once it has ended.
        pub async fn next(&self) -> Option<DeviceEvent> {
            self.events.lock().await.recv().await
        }

        /// Add a host ICE candidate.
        ///
        /// # Errors
        ///
        /// Returns an error when the connection rejects the candidate.
        pub async fn add_candidate(&self, candidate: &str) -> Result<()> {
            let init = RTCIceCandidateInit {
                candidate: candidate.to_string(),
                ..Default::default()
            };
            self.pc
                .add_ice_candidate(init)
                .await
                .context("add a host ICE candidate")
        }

        /// Send one text frame on the session channel.
        ///
        /// # Errors
        ///
        /// Returns an error when the channel is no longer open.
        pub async fn send_text(&self, text: &str) -> Result<()> {
            self.channel
                .send_text(text)
                .await
                .context("send on the session channel")
        }
    }
}
