//! A relay-only WebRTC pair stays connected over a TURN server that enforces
//! the RFC 8656 §12 channel rule and answers the first CHANNEL-BIND with a
//! stale nonce — the shape the live relay showed on 2026-11-02, where upstream
//! rtc-turn 0.21.1 answered a `438 Stale Nonce` by freeing the channel number,
//! re-bound the peer on a fresh number and was rejected with `400` forever,
//! dropping the ICE consent traffic until the pair died in seconds.
//!
//! The fake server here does what `turn.cloudflare.com` did: the first
//! CHANNEL-BIND of an allocation meets `438` (with a fresh nonce), and after
//! that RFC 8656 §12 applies — a CHANNEL-BIND for a peer that already has a
//! different channel number, or for a number already bound to a different
//! peer, is answered `400` and counted. With the patched client the pair must
//! connect, exchange data-channel messages for 60 simulated seconds, and leave
//! the server with zero 400s and one channel number per peer.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use rtc_turn::proto::chandata::ChannelData;
use rtc_turn::proto::channum::ChannelNumber;
use rtc_turn::proto::data::Data;
use rtc_turn::proto::lifetime::Lifetime;
use rtc_turn::proto::peeraddr::PeerAddress;
use rtc_turn::proto::relayaddr::RelayedAddress;
use stun::attributes::*;
use stun::error_code::{ErrorCodeAttribute, CODE_BAD_REQUEST, CODE_STALE_NONCE, CODE_UNAUTHORIZED};
use stun::integrity::MessageIntegrity;
use stun::message::*;
use stun::textattrs::{Nonce, Realm};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::timeout;
use webrtc::data_channel::{
    DataChannel, DataChannelEvent, RTCDataChannelInit, RTCDataChannelState,
};
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfigurationBuilder,
    RTCIceCandidateInit, RTCIceGatheringState, RTCIceServer, RTCIceTransportPolicy,
    RTCPeerConnectionIceEvent, RTCPeerConnectionState, RTCSessionDescription,
};

/// TURN credentials the fake server accepts and the peers present.
const USER: &str = "canopy-turn-user";
const PASS: &str = "canopy-turn-pass";
const REALM: &str = "canopy.turn.test";
/// The label of the one data channel the pair opens.
const LABEL: &str = "turn-channel";
/// How many simulated seconds the pair must keep exchanging messages.
const SIMULATED_SECONDS: u64 = 60;
/// Virtual-time budget for the whole connect (gather, DTLS, channel open).
const CONNECT_WAIT: Duration = Duration::from_secs(120);
/// Virtual-time budget for waiting for one expected message.
const MESSAGE_WAIT: Duration = Duration::from_secs(30);
/// Virtual-time budget for a data channel to open.
const OPEN_WAIT: Duration = Duration::from_secs(30);

type CallbackFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

/// What the fake server counted while the test ran.
#[derive(Default)]
struct TurnStats {
    /// CHANNEL-BIND requests answered `400` under the RFC 8656 §12 rule.
    bind_400s: AtomicUsize,
    /// Every channel number a client sent a CHANNEL-BIND for, by client address.
    bound_numbers: Mutex<HashMap<SocketAddr, HashSet<u16>>>,
}

/// One allocation: the relayed socket the server owns, plus the channel state
/// the RFC rule is applied to.
struct Alloc {
    /// The client's control address the allocation belongs to.
    client: SocketAddr,
    /// The relayed socket; data the client sends emerges from here.
    relayed: Arc<UdpSocket>,
    /// CHANNEL-NUMBER → peer address, as bound.
    channels: Mutex<HashMap<u16, SocketAddr>>,
    /// The reverse map, so a peer's number is found when its data arrives.
    peer_numbers: Mutex<HashMap<SocketAddr, u16>>,
    /// Peer IPs a `CreatePermission` permitted.
    perms: Mutex<HashSet<IpAddr>>,
    /// The first CHANNEL-BIND of this allocation meets a stale nonce.
    bound_once: AtomicBool,
}

/// A minimal TURN server: enough of RFC 8656 for a relay-only ICE pair, plus
/// the §12 rule the test is about. It signs nothing (the client checks no
/// response integrity) and verifies every authenticated request against the
/// long-term key `USER`:`REALM`:`PASS` derives.
struct FakeTurn {
    control: Arc<UdpSocket>,
    allocs: Mutex<HashMap<SocketAddr, Arc<Alloc>>>,
    nonce: Mutex<u32>,
    key: Vec<u8>,
    provider: Arc<dyn crypto::RTCCryptoProvider>,
    stats: Arc<TurnStats>,
}

impl FakeTurn {
    /// Starts the server and returns its control address and stats.
    async fn spawn() -> Result<(SocketAddr, Arc<TurnStats>)> {
        let provider = crypto::default_provider().context("a built-in crypto provider")?;
        let control = Arc::new(UdpSocket::bind("127.0.0.1:0").await?);
        let key = MessageIntegrity::long_term_integrity_key(
            USER.to_owned(),
            REALM.to_owned(),
            PASS.to_owned(),
            provider.crypto(),
        )
        .context("derive the long-term key")?;
        let stats = Arc::new(TurnStats::default());
        let server = Arc::new(Self {
            control: control.clone(),
            allocs: Mutex::new(HashMap::new()),
            nonce: Mutex::new(0),
            key,
            provider,
            stats: Arc::clone(&stats),
        });
        tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                let _ = server.control_loop().await;
            }
        });
        Ok((control.local_addr()?, stats))
    }

    /// A fresh nonce; the server rotates it on every 401 and 438.
    fn rotate_nonce(&self) -> String {
        let mut nonce = self.nonce.lock().unwrap();
        *nonce += 1;
        format!("nonce-{}", nonce)
    }

    async fn control_loop(self: Arc<Self>) -> Result<()> {
        let mut buf = vec![0u8; 65536];
        loop {
            let (n, from) = self.control.recv_from(&mut buf).await?;
            let packet = &buf[..n];
            if ChannelData::is_channel_data(packet) {
                self.client_to_peer(from, packet).await;
            } else if let Ok(mut msg) = decode_message(packet) {
                if msg.typ.class == CLASS_INDICATION && msg.typ.method == METHOD_SEND {
                    self.send_indication(from, &msg).await;
                } else {
                    Self::handle_request(&self, from, &mut msg).await;
                }
            }
        }
    }

    /// Forwards client data out of the allocation's relayed address: what the
    /// client sent on a bound channel goes to the peer it bound.
    async fn client_to_peer(&self, client: SocketAddr, packet: &[u8]) {
        let alloc = {
            let allocs = self.allocs.lock().unwrap();
            allocs.get(&client).cloned()
        };
        let Some(alloc) = alloc else { return };
        let mut cd = ChannelData {
            raw: packet.to_vec(),
            ..Default::default()
        };
        if cd.decode().is_err() {
            return;
        }
        let peer = {
            let channels = alloc.channels.lock().unwrap();
            channels.get(&cd.number.0).copied()
        };
        if let Some(peer) = peer {
            let _ = alloc.relayed.send_to(&cd.data, peer).await;
        }
    }

    async fn handle_request(self: &Arc<Self>, from: SocketAddr, msg: &mut Message) {
        if msg.typ.class != CLASS_REQUEST {
            return;
        }
        match msg.typ.method {
            METHOD_ALLOCATE => self.allocate(from, msg).await,
            METHOD_CREATE_PERMISSION => self.create_permission(from, msg).await,
            METHOD_CHANNEL_BIND => self.channel_bind(from, msg).await,
            METHOD_REFRESH => {
                if !self.authenticated(msg) {
                    self.unauthorized(from, msg).await;
                    return;
                }
                let raw = Self::encode_response(
                    msg.transaction_id,
                    METHOD_REFRESH,
                    CLASS_SUCCESS_RESPONSE,
                    vec![Box::new(Lifetime(Duration::from_secs(600)))],
                );
                self.send(from, &raw).await;
            }
            _ => {}
        }
    }

    /// Whether the request carries a valid MESSAGE-INTEGRITY for the shared
    /// long-term credentials.
    fn authenticated(&self, msg: &mut Message) -> bool {
        MessageIntegrity::check(msg, &self.key, self.provider.crypto()).is_ok()
    }

    /// The 401 that starts the long-term-credential exchange.
    async fn unauthorized(&self, from: SocketAddr, msg: &Message) {
        let nonce = self.rotate_nonce();
        let raw = Self::encode_response(
            msg.transaction_id,
            msg.typ.method,
            CLASS_ERROR_RESPONSE,
            vec![
                Box::new(ErrorCodeAttribute {
                    code: CODE_UNAUTHORIZED,
                    reason: b"Unauthorized".to_vec(),
                }),
                Box::new(Nonce::new(ATTR_NONCE, nonce)),
                Box::new(Realm::new(ATTR_REALM, REALM.to_owned())),
            ],
        );
        self.send(from, &raw).await;
    }

    async fn allocate(self: &Arc<Self>, from: SocketAddr, msg: &mut Message) {
        if !self.authenticated(msg) {
            self.unauthorized(from, msg).await;
            return;
        }
        // One allocation per client: a re-Allocate reuses the relayed address.
        let existing = {
            let allocs = self.allocs.lock().unwrap();
            allocs
                .get(&from)
                .map(|alloc| alloc.relayed.local_addr().expect("a bound relayed socket"))
        };
        if let Some(relayed) = existing {
            self.answer_allocated(from, msg, relayed).await;
            return;
        }
        let Ok(relayed) = UdpSocket::bind("127.0.0.1:0").await else {
            return;
        };
        let Ok(relayed_addr) = relayed.local_addr() else {
            return;
        };
        let alloc = Arc::new(Alloc {
            client: from,
            relayed: Arc::new(relayed),
            channels: Mutex::new(HashMap::new()),
            peer_numbers: Mutex::new(HashMap::new()),
            perms: Mutex::new(HashSet::new()),
            bound_once: AtomicBool::new(false),
        });
        self.allocs.lock().unwrap().insert(from, Arc::clone(&alloc));
        tokio::spawn(Self::relayed_loop(Arc::clone(self), alloc));
        self.answer_allocated(from, msg, relayed_addr).await;
    }

    async fn answer_allocated(&self, from: SocketAddr, msg: &Message, relayed: SocketAddr) {
        let raw = Self::encode_response(
            msg.transaction_id,
            METHOD_ALLOCATE,
            CLASS_SUCCESS_RESPONSE,
            vec![
                Box::new(RelayedAddress {
                    ip: relayed.ip(),
                    port: relayed.port(),
                }),
                Box::new(Lifetime(Duration::from_secs(600))),
            ],
        );
        self.send(from, &raw).await;
    }

    async fn create_permission(&self, from: SocketAddr, msg: &mut Message) {
        if !self.authenticated(msg) {
            self.unauthorized(from, msg).await;
            return;
        }
        let alloc = {
            let allocs = self.allocs.lock().unwrap();
            allocs.get(&from).cloned()
        };
        let Some(alloc) = alloc else { return };
        let mut peer = PeerAddress::default();
        if peer.get_from(msg).is_ok() {
            alloc.perms.lock().unwrap().insert(peer.ip);
        }
        let raw = Self::encode_response(
            msg.transaction_id,
            METHOD_CREATE_PERMISSION,
            CLASS_SUCCESS_RESPONSE,
            vec![],
        );
        self.send(from, &raw).await;
    }

    /// Relays a Send indication: unauthenticated, fire-and-forget data out of
    /// the allocation's relayed address towards its peer.
    async fn send_indication(&self, from: SocketAddr, msg: &Message) {
        let mut peer = PeerAddress::default();
        let mut data = Data::default();
        if peer.get_from(msg).is_err() || data.get_from(msg).is_err() {
            return;
        }
        let alloc = {
            let allocs = self.allocs.lock().unwrap();
            allocs.get(&from).cloned()
        };
        if let Some(alloc) = alloc {
            let peer = SocketAddr::new(peer.ip, peer.port);
            let _ = alloc.relayed.send_to(&data.0, peer).await;
        }
    }

    async fn channel_bind(&self, from: SocketAddr, msg: &mut Message) {
        if !self.authenticated(msg) {
            self.unauthorized(from, msg).await;
            return;
        }
        let alloc = {
            let allocs = self.allocs.lock().unwrap();
            allocs.get(&from).cloned()
        };
        let Some(alloc) = alloc else { return };

        let mut number = ChannelNumber::default();
        let mut peer = PeerAddress::default();
        if number.get_from(msg).is_err() || peer.get_from(msg).is_err() {
            return;
        }
        let peer = SocketAddr::new(peer.ip, peer.port);
        self.stats
            .bound_numbers
            .lock()
            .unwrap()
            .entry(from)
            .or_default()
            .insert(number.0);

        // The twist the live relay showed on 2026-11-02: the first CHANNEL-BIND
        // of an allocation is answered `438 Stale Nonce` — but the channel is
        // bound anyway, so the server now holds number↔peer while the client
        // believes the bind failed. A client that frees the number on the 438
        // then re-binds the peer on a fresh one and is rejected with 400
        // forever; a client that retries the same number (the patch) finds the
        // server already agrees with it.
        if !alloc.bound_once.swap(true, Ordering::SeqCst) {
            alloc.channels.lock().unwrap().insert(number.0, peer);
            alloc.peer_numbers.lock().unwrap().insert(peer, number.0);
            let nonce = self.rotate_nonce();
            let raw = Self::encode_response(
                msg.transaction_id,
                METHOD_CHANNEL_BIND,
                CLASS_ERROR_RESPONSE,
                vec![
                    Box::new(ErrorCodeAttribute {
                        code: CODE_STALE_NONCE,
                        reason: b"Stale Nonce".to_vec(),
                    }),
                    Box::new(Nonce::new(ATTR_NONCE, nonce)),
                    Box::new(Realm::new(ATTR_REALM, REALM.to_owned())),
                ],
            );
            self.send(from, &raw).await;
            return;
        }

        // RFC 8656 §12: a peer that already has a channel, or a number already
        // bound to a peer, is never bound twice — answer 400.
        let conflict = {
            let channels = alloc.channels.lock().unwrap();
            let peer_numbers = alloc.peer_numbers.lock().unwrap();
            peer_numbers
                .get(&peer)
                .is_some_and(|bound| *bound != number.0)
                || channels.get(&number.0).is_some_and(|bound| *bound != peer)
        };
        if conflict {
            self.stats.bind_400s.fetch_add(1, Ordering::SeqCst);
            let raw = Self::encode_response(
                msg.transaction_id,
                METHOD_CHANNEL_BIND,
                CLASS_ERROR_RESPONSE,
                vec![Box::new(ErrorCodeAttribute {
                    code: CODE_BAD_REQUEST,
                    reason: Vec::new(),
                })],
            );
            self.send(from, &raw).await;
        } else {
            alloc.channels.lock().unwrap().insert(number.0, peer);
            alloc.peer_numbers.lock().unwrap().insert(peer, number.0);
            let raw = Self::encode_response(
                msg.transaction_id,
                METHOD_CHANNEL_BIND,
                CLASS_SUCCESS_RESPONSE,
                vec![],
            );
            self.send(from, &raw).await;
        }
    }

    /// Reads one allocation's relayed socket: data arriving from a peer goes to
    /// the client as `ChannelData` on the bound number, or as a `Data`
    /// indication when only a permission covers the peer.
    async fn relayed_loop(self: Arc<Self>, alloc: Arc<Alloc>) -> Result<()> {
        let mut buf = vec![0u8; 65536];
        loop {
            let (n, from) = alloc.relayed.recv_from(&mut buf).await?;
            let data = &buf[..n];
            let number = {
                let peer_numbers = alloc.peer_numbers.lock().unwrap();
                peer_numbers.get(&from).copied()
            };
            if let Some(number) = number {
                let mut cd = ChannelData {
                    number: ChannelNumber(number),
                    data: data.to_vec(),
                    ..Default::default()
                };
                cd.encode();
                self.control.send_to(&cd.raw, alloc.client).await?;
            } else {
                let permitted = {
                    let perms = alloc.perms.lock().unwrap();
                    perms.contains(&from.ip())
                };
                if !permitted {
                    continue;
                }
                let raw = {
                    let mut msg = Message::new();
                    let setters: Vec<Box<dyn Setter>> = vec![
                        Box::new(TransactionId::new()),
                        Box::new(MessageType::new(METHOD_DATA, CLASS_INDICATION)),
                        Box::new(PeerAddress {
                            ip: from.ip(),
                            port: from.port(),
                        }),
                        Box::new(Data(data.to_vec())),
                    ];
                    if msg.build(&setters).is_err() {
                        continue;
                    }
                    std::mem::take(&mut msg.raw)
                };
                self.control.send_to(&raw, alloc.client).await?;
            }
        }
    }

    /// Encodes one response message; empty when the attributes do not build.
    /// Sync on purpose: `dyn Setter` is not `Send`, so the encoding must be
    /// finished before anything awaits.
    fn encode_response(
        tid: TransactionId,
        method: Method,
        class: MessageClass,
        attrs: Vec<Box<dyn Setter>>,
    ) -> Vec<u8> {
        let mut setters: Vec<Box<dyn Setter>> =
            vec![Box::new(tid), Box::new(MessageType::new(method, class))];
        setters.extend(attrs);
        let mut msg = Message::new();
        if msg.build(&setters).is_err() {
            return Vec::new();
        }
        std::mem::take(&mut msg.raw)
    }

    /// Sends an encoded datagram on the control socket; an empty one is the
    /// encode-failure marker and is dropped.
    async fn send(&self, to: SocketAddr, raw: &[u8]) {
        if raw.is_empty() {
            return;
        }
        let _ = self.control.send_to(raw, to).await;
    }
}

fn decode_message(packet: &[u8]) -> Result<Message> {
    let mut msg = Message::new();
    msg.raw = packet.to_vec();
    msg.decode()?;
    Ok(msg)
}

/// The state one test peer's callbacks report.
struct PeerHandler {
    candidates: mpsc::Sender<String>,
    gathered: mpsc::Sender<()>,
    /// The channel the remote peer opened, once it arrives.
    channel: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
    incoming: mpsc::Sender<Vec<u8>>,
    state: Arc<Mutex<RTCPeerConnectionState>>,
}

impl PeerHandler {
    /// Forwards every inbound frame of `channel` into `incoming`.
    fn pump(channel: Arc<dyn DataChannel>, incoming: mpsc::Sender<Vec<u8>>) {
        tokio::spawn(async move {
            while let Some(event) = channel.poll().await {
                match event {
                    DataChannelEvent::OnMessage(message) => {
                        if incoming.send(message.data.to_vec()).await.is_err() {
                            return;
                        }
                    }
                    DataChannelEvent::OnClose => return,
                    _ => {}
                }
            }
        });
    }
}

impl PeerConnectionEventHandler for PeerHandler {
    fn on_ice_candidate<'a, 'b>(&'a self, event: RTCPeerConnectionIceEvent) -> CallbackFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            if let Ok(init) = event.candidate.to_json() {
                if !init.candidate.is_empty() {
                    let _ = self.candidates.send(init.candidate).await;
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
            *self.state.lock().unwrap() = state;
        })
    }

    fn on_data_channel<'a, 'b>(&'a self, data_channel: Arc<dyn DataChannel>) -> CallbackFuture<'b>
    where
        'a: 'b,
        Self: 'b,
    {
        Box::pin(async move {
            *self.channel.lock().unwrap() = Some(Arc::clone(&data_channel));
            Self::pump(data_channel, self.incoming.clone());
        })
    }
}

/// One end of the pair: the connection plus the queues its callbacks feed.
struct TestPeer {
    pc: Arc<dyn PeerConnection>,
    /// Sender the data-channel pump uses; kept so the offerer can attach the
    /// channel it created itself.
    incoming_tx: mpsc::Sender<Vec<u8>>,
    incoming: mpsc::Receiver<Vec<u8>>,
    state: Arc<Mutex<RTCPeerConnectionState>>,
    accepted: Arc<Mutex<Option<Arc<dyn DataChannel>>>>,
}

/// Builds one relay-only peer pointed at the fake TURN server.
async fn build_peer(
    turn: SocketAddr,
) -> Result<(TestPeer, mpsc::Receiver<String>, mpsc::Receiver<()>)> {
    let (candidates_tx, candidates) = mpsc::channel(64);
    let (gathered_tx, gathered) = mpsc::channel(1);
    let (incoming_tx, incoming) = mpsc::channel(64);
    let accepted = Arc::new(Mutex::new(None));
    let state = Arc::new(Mutex::new(RTCPeerConnectionState::New));
    let handler = Arc::new(PeerHandler {
        candidates: candidates_tx,
        gathered: gathered_tx,
        channel: Arc::clone(&accepted),
        incoming: incoming_tx.clone(),
        state: Arc::clone(&state),
    });
    let configuration = RTCConfigurationBuilder::new()
        .with_ice_servers(vec![RTCIceServer {
            urls: vec![format!("turn:{turn}?transport=udp")],
            username: USER.to_owned(),
            credential: PASS.to_owned(),
        }])
        .with_ice_transport_policy(RTCIceTransportPolicy::Relay)
        .build();
    let pc: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(configuration)
            .with_handler(handler)
            .with_udp_addrs(vec!["127.0.0.1:0"])
            .build()
            .await
            .context("build the peer connection")?,
    );
    Ok((
        TestPeer {
            pc,
            incoming_tx,
            incoming,
            state,
            accepted,
        },
        candidates,
        gathered,
    ))
}

/// Forwards one peer's candidates to the other, recording what was gathered so
/// the test can assert the relay-only policy held.
async fn forward_candidates(
    mut rx: mpsc::Receiver<String>,
    seen: Arc<Mutex<Vec<String>>>,
    pc: Arc<dyn PeerConnection>,
) {
    while let Some(candidate) = rx.recv().await {
        seen.lock().unwrap().push(candidate.clone());
        let init = RTCIceCandidateInit {
            candidate,
            ..Default::default()
        };
        let _ = pc.add_ice_candidate(init).await;
    }
}

/// Waits until `peer` reports Connected; fails fast on a terminal state.
async fn wait_connected(peer: &TestPeer, name: &str) -> Result<()> {
    timeout(CONNECT_WAIT, async {
        loop {
            let state = *peer.state.lock().unwrap();
            if state == RTCPeerConnectionState::Connected {
                return Ok(());
            }
            if matches!(
                state,
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed
            ) {
                return Err(anyhow::anyhow!(
                    "{name} reached {state:?} before connecting"
                ));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context(format!("{name} never connected"))?
}

/// Waits until the data channel is open.
async fn wait_open(channel: &Arc<dyn DataChannel>, name: &str) -> Result<()> {
    timeout(OPEN_WAIT, async {
        loop {
            if channel.ready_state().await? == RTCDataChannelState::Open {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context(format!("{name} never opened"))?
}

/// Waits for the channel the remote peer opened.
async fn wait_accepted(peer: &TestPeer, name: &str) -> Result<Arc<dyn DataChannel>> {
    timeout(OPEN_WAIT, async {
        loop {
            let channel = peer.accepted.lock().unwrap().clone();
            if let Some(channel) = channel {
                return Ok(channel);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .context(format!("{name} was never offered a channel"))?
}

/// Waits until one expected frame arrives, skipping anything else.
async fn recv_match(peer: &mut TestPeer, needle: &str) -> Result<()> {
    timeout(MESSAGE_WAIT, async {
        loop {
            let frame = peer
                .incoming
                .recv()
                .await
                .context("the data channel closed")?;
            if frame == needle.as_bytes() {
                return Ok(());
            }
        }
    })
    .await
    .context(format!("never received {needle}"))?
}

/// A relay-only pair survives a TURN server that enforces the RFC 8656 §12
/// channel rule and opens with a stale nonce: connected for 60 seconds of
/// soak, messages flowing both ways, zero 400s, one channel number per peer.
#[tokio::test]
async fn relay_pair_survives_rfc_8656_channel_rule() -> Result<()> {
    let (turn, stats) = FakeTurn::spawn().await?;
    let (mut a, a_candidates, mut a_gathered) = build_peer(turn).await?;
    let (mut b, b_candidates, mut b_gathered) = build_peer(turn).await?;

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    tokio::spawn(forward_candidates(
        a_candidates,
        Arc::clone(&seen),
        Arc::clone(&b.pc),
    ));
    tokio::spawn(forward_candidates(
        b_candidates,
        Arc::clone(&seen),
        Arc::clone(&a.pc),
    ));

    // A offers and opens the channel; B answers.
    let a_channel =
        a.pc.create_data_channel(
            LABEL,
            Some(RTCDataChannelInit {
                ordered: true,
                ..Default::default()
            }),
        )
        .await
        .context("create the data channel")?;
    PeerHandler::pump(Arc::clone(&a_channel), a.incoming_tx.clone());
    let offer: RTCSessionDescription = a.pc.create_offer(None).await.context("create the offer")?;
    a.pc.set_local_description(offer.clone())
        .await
        .context("set the local offer")?;
    b.pc.set_remote_description(offer)
        .await
        .context("apply the offer")?;
    let answer =
        b.pc.create_answer(None)
            .await
            .context("create the answer")?;
    b.pc.set_local_description(answer.clone())
        .await
        .context("set the local answer")?;
    a.pc.set_remote_description(answer)
        .await
        .context("apply the answer")?;

    // Gathering completes once the allocations exist.
    let _ = timeout(CONNECT_WAIT, a_gathered.recv()).await;
    let _ = timeout(CONNECT_WAIT, b_gathered.recv()).await;

    wait_connected(&a, "A").await?;
    wait_connected(&b, "B").await?;
    wait_open(&a_channel, "A's channel").await?;
    let b_channel = wait_accepted(&b, "B").await?;
    wait_open(&b_channel, "B's channel").await?;

    // One message each way before the clock starts.
    a_channel.send_text("hello-0").await?;
    recv_match(&mut b, "hello-0").await?;
    b_channel.send_text("ack-0").await?;
    recv_match(&mut a, "ack-0").await?;

    // 60 simulated seconds of ping-pong; consent keeps flowing the whole time.
    for i in 1..=SIMULATED_SECONDS {
        let ping = format!("ping-{i}");
        let pong = format!("pong-{i}");
        a_channel.send_text(&ping).await?;
        recv_match(&mut b, &ping).await?;
        b_channel.send_text(&pong).await?;
        recv_match(&mut a, &pong).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    assert_eq!(
        RTCPeerConnectionState::Connected,
        *a.state.lock().unwrap(),
        "A stays connected"
    );
    assert_eq!(
        RTCPeerConnectionState::Connected,
        *b.state.lock().unwrap(),
        "B stays connected"
    );

    // The rule held: the server never answered 400, and each peer used exactly
    // one channel number for the other.
    assert_eq!(
        0,
        stats.bind_400s.load(Ordering::SeqCst),
        "the server never answered 400"
    );
    let numbers = stats.bound_numbers.lock().unwrap();
    assert_eq!(2, numbers.len(), "both peers bound a channel");
    for (client, bound) in numbers.iter() {
        assert_eq!(
            1,
            bound.len(),
            "{client} used exactly one channel number, used {bound:?}"
        );
    }

    // And everything the policy gathered was a relay candidate.
    let candidates = seen.lock().unwrap();
    assert!(!candidates.is_empty(), "the pair gathered candidates");
    for candidate in candidates.iter() {
        assert!(
            candidate.contains(" typ relay"),
            "the relay-only policy gathered {candidate}"
        );
    }

    Ok(())
}
