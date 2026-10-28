//! Loopback tests for the device-peer bridge: two real `webrtc` peers in one
//! process, one playing the paired device (d4eb8558 scalar 0x22), one serving
//! the hub through [`super::PeerSession`]. The relay only carries signaling,
//! so the two peers hand their SDPs to each other directly here.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::remote::bridge::{CHUNK_BYTES, MAX_FRAME_BYTES};
use crate::remote::devices::device_id;
use crate::remote::hostkey::HostKey;
use crate::remote::peer;
use crate::remote::signaling::{self, MAX_DEVICE_PEERS};
use crate::sessions::ws::SessionsState;
use crate::sessions::{HubSession, SessionHub, SessionKind};

/// Device id the d4eb8558 device key (scalar 0x22) derives.
const DEVICE_ID: &str = "bfcba2b742a807bb";
/// Relay peer id the host addresses its trickle candidates to.
const FROM: &str = "d:loopback";
/// What the `cat` sessions echo back through their PTY.
const ECHO: &[u8] = b"loopback-echo\n";
/// The needle the echo is matched with: the PTY translates the line's
/// newline to CRLF on the way back, so the full line would never match.
/// (The local round-trip test uses a newline-less needle for the same reason.)
const ECHO_NEEDLE: &[u8] = b"loopback-echo";
/// How long the first sends may wait for the data channel to open.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// How long one inbound frame may take to arrive.
const FRAME_TIMEOUT: Duration = Duration::from_secs(20);
/// How long to wait for `snapshot_more` after a `snapshot`: continuations
/// travel back-to-back with it, so a quiet window means it was complete.
const CONTINUATION_TIMEOUT: Duration = Duration::from_secs(2);
/// Quiet window draining the echo's straggler chunks after the needle matched.
const ECHO_DRAIN_TIMEOUT: Duration = Duration::from_secs(1);

/// What the device side of the loopback reports.
enum DeviceEvent {
    /// One inbound data-channel frame, raw and parsed.
    Frame(Frame),
    /// The channel, or the whole connection, is gone.
    Closed,
}

/// One inbound frame; kept raw so tests can assert its size.
struct Frame {
    raw: String,
    json: Value,
}

impl Frame {
    /// Parse one inbound text frame.
    fn parse(bytes: Vec<u8>) -> Self {
        let raw = String::from_utf8(bytes).expect("frames are UTF-8 JSON");
        let json = serde_json::from_str(&raw).expect("frames are JSON");
        Self { raw, json }
    }
}

/// The fake relay's `/turn`: records the bearer token, answers no ICE servers.
#[derive(Clone)]
struct TurnState {
    auth: Arc<Mutex<Option<String>>>,
}

async fn turn_handler(State(state): State<TurnState>, headers: HeaderMap) -> impl IntoResponse {
    let seen = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    *state.auth.lock().expect("auth lock") = seen;
    (
        axum::http::StatusCode::OK,
        axum::Json(json!({"iceServers": []})),
    )
}

/// Start the fake relay on an ephemeral port; `(url, seen authorization)`.
async fn start_turn() -> (String, Arc<Mutex<Option<String>>>) {
    let auth = Arc::new(Mutex::new(None));
    let state = TurnState {
        auth: Arc::clone(&auth),
    };
    let app = Router::new()
        .route("/turn", get(turn_handler))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake relay");
    let addr = listener.local_addr().expect("fake relay addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), auth)
}

fn encode(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn decode(frame: &Value) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(frame["data"].as_str().expect("base64 data"))
        .expect("valid base64")
}

/// A spawned session, killed when the guard drops so no test leaves a child.
struct KillGuard {
    hub: SessionHub,
    id: String,
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        let _ = self.hub.kill(&self.id);
    }
}

/// Spawn a `cat` session of `kind`; `cat` echoes whatever reaches its PTY.
fn spawn_session(
    hub: &SessionHub,
    kind: SessionKind,
    workdir: &Path,
) -> (Arc<HubSession>, KillGuard) {
    let request = crate::sessions::SpawnRequest {
        kind,
        cli: "cat".to_string(),
        model: None,
        workdir: workdir.to_string_lossy().to_string(),
        name: None,
        prompt: None,
        interactive_args: None,
        fallback_args: None,
        model_flag: None,
        id: None,
        seed_id: None,
        accent: None,
    };
    let session = hub.spawn(&request).expect("test spawn");
    let guard = KillGuard {
        hub: hub.clone(),
        id: session.id.clone(),
    };
    (session, guard)
}

/// One connected device peer plus the hub it is bridged to.
struct Loopback {
    device: Arc<peer::device_side::DeviceSide>,
    events: mpsc::UnboundedReceiver<DeviceEvent>,
    pending: Option<Frame>,
    peers: DevicePeers,
    connections: ConnectionRegistry,
    cat: Arc<HubSession>,
    terminal: Arc<HubSession>,
    /// The guards must outlive the setup: dropping one kills its session, and
    /// the resulting `SessionRemoved` would reach the peer before its channel
    /// opens and end the session.
    _kills: Vec<KillGuard>,
    _tasks: Vec<JoinHandle<()>>,
}

/// Wire a device peer to a hub-backed [`PeerSession`] over a real loopback
/// WebRTC connection: the fake relay's `/turn` supplies the ICE servers, the
/// device opens the session channel, the host answers and bridges it.
async fn connected_loopback() -> Loopback {
    // The hub: one agent session a device may use, one terminal it may not.
    let dir = tempfile::tempdir().expect("tempdir");
    let db =
        Arc::new(crate::db::Database::new(&dir.path().join("test.db")).expect("test database"));
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
    let workdir = tempfile::tempdir().expect("workdir");
    let (cat, kill_cat) = spawn_session(&hub, SessionKind::Agent, workdir.path());
    let (terminal, kill_terminal) = spawn_session(&hub, SessionKind::Terminal, workdir.path());
    let state = SessionsState::for_test(hub, "token".to_string());

    // The device identity: the d4eb8558 vector key derives the vector id.
    let vector_key = HostKey::from_scalar_bytes(&[0x22; 32]).expect("device key");
    assert_eq!(device_id(vector_key.spki_der()), DEVICE_ID);

    // The relay hands out no ICE servers; the host still fetches them per
    // connection, with its bearer token.
    let (relay_url, auth) = start_turn().await;
    let servers = peer::fetch_ice_servers(&relay_url, "gho_loopback")
        .await
        .expect("the /turn fetch");
    assert!(
        servers.is_empty(),
        "the fake relay hands out no ICE servers"
    );
    assert_eq!(
        auth.lock().expect("auth lock").as_deref(),
        Some("Bearer gho_loopback")
    );

    // Offer and answer over loopback; the device end is the test-facing
    // browser side, the host end is the production transport.
    let (offer_sdp, device) = peer::device_side::DeviceSide::offer()
        .await
        .expect("the device offer");
    let (answer_sdp, host_peer) = peer::Peer::answer_offer(servers, &offer_sdp)
        .await
        .expect("the host answer");
    let device = Arc::new(device);
    device
        .accept(&answer_sdp)
        .await
        .expect("the device accepts the answer");

    // Host bookkeeping, exactly like the relay's offer path.
    let peers = DevicePeers::default();
    let connections = ConnectionRegistry::default();
    let token = connections.register(DEVICE_ID);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    assert!(peers.register(DEVICE_ID, FROM, token.clone(), control_tx));
    let (signal_tx, mut signal_rx) = mpsc::unbounded_channel();

    // The device demultiplexes its peer: trickle to the host, frames here.
    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let demux = {
        let device = Arc::clone(&device);
        let peers = peers.clone();
        tokio::spawn(async move {
            while let Some(event) = device.next().await {
                match event {
                    peer::device_side::DeviceEvent::Candidate(candidate) => {
                        let _ = peers.route(FROM, Control::Ice(candidate));
                    }
                    peer::device_side::DeviceEvent::Frame(bytes) => {
                        let _ = events_tx.send(DeviceEvent::Frame(Frame::parse(bytes)));
                    }
                    peer::device_side::DeviceEvent::Closed => {
                        let _ = events_tx.send(DeviceEvent::Closed);
                        return;
                    }
                }
            }
        })
    };

    // The host's trickled candidates reach the device through its signal channel.
    let trickle = {
        let device = Arc::clone(&device);
        tokio::spawn(async move {
            while let Some((_from, body)) = signal_rx.recv().await {
                if let Some(candidate) = signaling::ice_candidate(&body) {
                    let _ = device.add_candidate(candidate).await;
                }
            }
        })
    };

    spawn(PeerSession {
        peer: host_peer,
        device_id: DEVICE_ID.to_string(),
        state,
        peers: peers.clone(),
        connections: connections.clone(),
        token,
        control: control_rx,
        signal: signal_tx,
        from: FROM.to_string(),
    });

    Loopback {
        device,
        events: events_rx,
        pending: None,
        peers,
        connections,
        cat,
        terminal,
        _kills: vec![kill_cat, kill_terminal],
        _tasks: vec![demux, trickle],
    }
}

impl Loopback {
    /// Send one client frame; the first sends wait for the channel to open.
    async fn send(&self, body: Value) {
        let text = body.to_string();
        let deadline = Instant::now() + OPEN_TIMEOUT;
        loop {
            match self.device.send_text(&text).await {
                Ok(()) => return,
                Err(error) if Instant::now() < deadline => {
                    tracing::debug!("the data channel is not open yet: {error:#}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => panic!("could not send over the data channel: {error:#}"),
            }
        }
    }

    /// The next inbound frame, failing the test when the peer ends first.
    async fn recv(&mut self, what: &str) -> Frame {
        if let Some(frame) = self.pending.take() {
            return frame;
        }
        let next = tokio::time::timeout(FRAME_TIMEOUT, self.events.recv()).await;
        match next {
            Ok(Some(DeviceEvent::Frame(frame))) => frame,
            Ok(Some(DeviceEvent::Closed)) => panic!("the channel closed while waiting for {what}"),
            Ok(None) => panic!("the device peer ended while waiting for {what}"),
            Err(_) => panic!("timed out waiting for {what}"),
        }
    }

    /// Consume a `snapshot` and its `snapshot_more` continuations; the seq.
    ///
    /// A snapshot that fits in one frame has no continuations at all, so the
    /// tail waits only a quiet window: anything that arrives must either
    /// continue the snapshot or belong to whatever comes next.
    async fn recv_snapshot(&mut self, what: &str) -> u64 {
        let frame = self.recv(what).await;
        assert_eq!(
            frame.json["t"], "snapshot",
            "{what}: expected a snapshot, got {}",
            frame.raw
        );
        assert_eq!(
            frame.json["id"], self.cat.id,
            "{what}: the snapshot must be the cat session's"
        );
        let seq = frame.json["seq"].as_u64().expect("snapshot seq");
        loop {
            let next = tokio::time::timeout(CONTINUATION_TIMEOUT, self.events.recv()).await;
            let next = match next {
                Ok(Some(DeviceEvent::Frame(frame))) => frame,
                Ok(Some(DeviceEvent::Closed)) => {
                    panic!("the channel closed while waiting for {what}'s continuations")
                }
                Ok(None) => {
                    panic!("the device peer ended while waiting for {what}'s continuations")
                }
                Err(_) => return seq,
            };
            if next.json["t"] != "snapshot_more" {
                self.pending = Some(next);
                return seq;
            }
            assert!(
                next.raw.len() <= MAX_FRAME_BYTES,
                "frame of {} bytes exceeds {MAX_FRAME_BYTES}",
                next.raw.len()
            );
            if next.json["last"].as_bool().expect("the last flag") {
                return seq;
            }
        }
    }
}

/// The next frame must be an error; return it for the caller's asserts.
async fn expect_error(loopback: &mut Loopback, what: &str) -> Value {
    let frame = loopback.recv(what).await;
    assert_eq!(
        frame.json["t"], "error",
        "{what}: expected an error, got {}",
        frame.raw
    );
    frame.json
}

/// One `out` frame of the echo: this session, size-bounded, seq in order.
fn assert_echo_out(frame: &Frame, id: &str, expected_seq: u64) {
    assert_eq!(
        frame.json["t"], "out",
        "only out frames expected here: {}",
        frame.raw
    );
    assert_eq!(frame.json["id"].as_str(), Some(id));
    assert!(
        frame.raw.len() <= MAX_FRAME_BYTES,
        "frame of {} bytes exceeds {MAX_FRAME_BYTES}",
        frame.raw.len()
    );
    assert_eq!(
        frame.json["seq"].as_u64(),
        Some(expected_seq),
        "out seqs continue the snapshot's without a gap"
    );
}

/// Send one line through the attached `cat` session and collect its echo:
/// every `out` frame continues the snapshot's seq, none exceeds the limit.
///
/// A trailing drain collects the echo's straggler chunks — the PTY echo and
/// `cat`'s own output can arrive as separate chunks — so nothing leaks into
/// the next step.
async fn echo_roundtrip(loopback: &mut Loopback, snapshot_seq: u64) {
    loopback
        .send(json!({"t": "input", "id": loopback.cat.id, "data": encode(ECHO)}))
        .await;
    let id = loopback.cat.id.clone();
    let mut seen: Vec<u8> = Vec::new();
    let mut expected_seq = snapshot_seq + 1;
    while !seen
        .windows(ECHO_NEEDLE.len())
        .any(|window| window == ECHO_NEEDLE)
    {
        let frame = loopback.recv("the echoed input").await;
        assert_echo_out(&frame, &id, expected_seq);
        expected_seq += 1;
        seen.extend(decode(&frame.json));
    }
    loop {
        let next = tokio::time::timeout(ECHO_DRAIN_TIMEOUT, loopback.events.recv()).await;
        match next {
            Ok(Some(DeviceEvent::Frame(frame))) => {
                assert_echo_out(&frame, &id, expected_seq);
                expected_seq += 1;
            }
            Ok(Some(DeviceEvent::Closed)) => {
                panic!("the channel closed while draining the echo")
            }
            Ok(None) => panic!("the device peer ended while draining the echo"),
            Err(_) => return,
        }
    }
}

/// The remote restrictions of FR3, driven through the data channel: terminal
/// sessions are refused everywhere, `resize` always, `spawn` unless `agent`.
async fn assert_restrictions(loopback: &mut Loopback) {
    loopback
        .send(json!({"t": "attach", "id": loopback.terminal.id}))
        .await;
    assert_eq!(
        expect_error(loopback, "attach on a terminal").await["code"],
        "forbidden"
    );
    loopback
        .send(json!({"t": "input", "id": loopback.terminal.id, "data": encode(b"x")}))
        .await;
    assert_eq!(
        expect_error(loopback, "input on a terminal").await["code"],
        "forbidden"
    );
    loopback
        .send(json!({"t": "prompt", "req": "p1", "id": loopback.terminal.id, "text": "hello"}))
        .await;
    let error = expect_error(loopback, "prompt on a terminal").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "p1");
    loopback
        .send(json!({"t": "kill", "req": "k1", "id": loopback.terminal.id}))
        .await;
    let error = expect_error(loopback, "kill on a terminal").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "k1");
    // `resize` is refused even for the sessions a device may use.
    loopback
        .send(json!({"t": "resize", "id": loopback.cat.id, "cols": 120, "rows": 40}))
        .await;
    assert_eq!(expect_error(loopback, "resize").await["code"], "forbidden");
    // `spawn` accepts only kind `agent`; a terminal is refused before spawning.
    loopback
        .send(
            json!({"t": "spawn", "req": "s1", "kind": "terminal", "cli": "cat", "workdir": "/tmp"}),
        )
        .await;
    let error = expect_error(loopback, "spawn a terminal").await;
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["req"], "s1");
    loopback
        .send(json!({"t": "spawn", "req": "s2", "kind": "banana", "cli": "cat", "workdir": "/tmp"}))
        .await;
    assert_eq!(
        expect_error(loopback, "spawn a bad kind").await["code"],
        "bad_request"
    );
}

/// Drive 100 KiB through the session's output in one hub chunk and check the
/// bridge re-chunks it into size-bounded, in-order frames with the source seq.
async fn assert_hundred_kib_intact(loopback: &mut Loopback) {
    let payload: Vec<u8> = (0..100 * 1024).map(|index| (index % 251) as u8).collect();
    loopback.cat.ingest_chunk(&payload);
    let expected_frames = payload.len().div_ceil(CHUNK_BYTES);
    let mut rebuilt: Vec<u8> = Vec::with_capacity(payload.len());
    let mut frames = 0usize;
    let mut payload_seq: Option<u64> = None;
    while frames < expected_frames {
        let frame = loopback.recv("a payload chunk").await;
        assert_eq!(
            frame.json["t"], "out",
            "only out frames expected here: {}",
            frame.raw
        );
        assert!(
            frame.raw.len() <= MAX_FRAME_BYTES,
            "frame of {} bytes exceeds {MAX_FRAME_BYTES}",
            frame.raw.len()
        );
        let chunk = decode(&frame.json);
        assert!(
            chunk.len() <= CHUNK_BYTES,
            "chunk of {} raw bytes exceeds {CHUNK_BYTES}",
            chunk.len()
        );
        let seq = frame.json["seq"].as_u64().expect("out seq");
        if payload_seq.is_none() {
            if chunk.len() < CHUNK_BYTES {
                // A trailing echo chunk that was sent before the payload.
                continue;
            }
            payload_seq = Some(seq);
        }
        assert_eq!(
            payload_seq,
            Some(seq),
            "every chunk keeps the seq of its source chunk"
        );
        rebuilt.extend_from_slice(&chunk);
        frames += 1;
    }
    assert_eq!(
        rebuilt, payload,
        "the 100 KiB output arrived intact and in order"
    );
}

/// A revoke — the same two calls [`RelayRuntime::revoke`] makes — closes the
/// peer, and the freed slot shows up in the live-peer registry.
///
/// [`RelayRuntime::revoke`]: crate::remote::relay::RelayRuntime
async fn close_by_revoking(loopback: &mut Loopback) {
    loopback.connections.close(DEVICE_ID);
    loopback.peers.close(DEVICE_ID);
    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match loopback.events.recv().await {
                Some(DeviceEvent::Closed) | None => return,
                // Trailing frames that were already in flight are fine.
                Some(DeviceEvent::Frame(_)) => {}
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "a revoked device's peer must close within a second"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while loopback.peers.len() > 0 {
        assert!(
            Instant::now() < deadline,
            "the peer slot must be released when the channel closes"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// End to end over a real data channel: list, attach, echo, the remote
/// restrictions, 100 KiB chunking, and revoke-closes-the-peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_paired_device_gets_the_session_hub_over_a_webrtc_data_channel() {
    let mut loopback = connected_loopback().await;

    // The device is greeted exactly like a local socket: hello, then list.
    let hello = loopback.recv("the hello").await;
    assert_eq!(hello.json["t"], "hello", "got {}", hello.raw);
    assert_eq!(hello.json["proto"], 1);

    // The listing the device sees: the agent session in, the terminal out.
    loopback.send(json!({"t": "list", "req": "l1"})).await;
    let frame = loopback.recv("the sessions answer").await;
    assert_eq!(frame.json["t"], "sessions");
    assert_eq!(frame.json["req"], "l1");
    let items = frame.json["items"].as_array().expect("items array");
    let ids: Vec<&str> = items
        .iter()
        .map(|item| item["id"].as_str().expect("item id"))
        .collect();
    assert!(
        ids.contains(&loopback.cat.id.as_str()),
        "the agent session is listed: {ids:?}"
    );
    assert!(
        !ids.contains(&loopback.terminal.id.as_str()),
        "terminal sessions are hidden from the list: {ids:?}"
    );
    let cat_item = items
        .iter()
        .find(|item| item["id"] == loopback.cat.id)
        .expect("the cat session item");
    assert_eq!(cat_item["kind"], "agent");

    // Attach the cat session: its snapshot arrives first, then the echo.
    loopback
        .send(json!({"t": "attach", "id": loopback.cat.id}))
        .await;
    let snapshot_seq = loopback.recv_snapshot("the cat snapshot").await;
    echo_roundtrip(&mut loopback, snapshot_seq).await;

    // The remote restrictions.
    assert_restrictions(&mut loopback).await;

    // Chunking over the real channel: 100 KiB intact, in order, size-bounded.
    assert_hundred_kib_intact(&mut loopback).await;

    // A revoke closes the peer and frees its slot.
    close_by_revoking(&mut loopback).await;
}

#[test]
fn the_fifth_concurrent_peer_is_refused_busy() {
    let peers = DevicePeers::default();
    // The receivers must stay alive, or routing fails for a closed channel.
    let mut kept: Vec<mpsc::UnboundedReceiver<Control>> = Vec::new();
    for index in 0..MAX_DEVICE_PEERS {
        let (control, rx) = mpsc::unbounded_channel();
        kept.push(rx);
        assert!(peers.register(
            &format!("device-{index}"),
            &format!("d:{index}"),
            Arc::new(CancellationToken::new()),
            control
        ));
    }
    let (control, rx) = mpsc::unbounded_channel();
    kept.push(rx);
    assert!(
        !peers.register(
            "device-5",
            "d:5",
            Arc::new(CancellationToken::new()),
            control
        ),
        "the fifth concurrent peer must be refused"
    );
    assert_eq!(peers.len(), MAX_DEVICE_PEERS);
    // `ice` bodies carry no device id: they route by their relay connection.
    assert!(peers.route("d:0", Control::Ice("candidate".to_string())));
    assert!(!peers.route("d:5", Control::Ice("candidate".to_string())));
    // A revoke frees the slot for the next device.
    peers.close("device-0");
    assert_eq!(peers.len(), MAX_DEVICE_PEERS - 1);
    let (control, rx) = mpsc::unbounded_channel();
    kept.push(rx);
    assert!(peers.register(
        "device-5",
        "d:5",
        Arc::new(CancellationToken::new()),
        control
    ));
}

/// A device that offers again before its old peer noticed keeps its new slot:
/// the old peer's cleanup must not remove the new handle.
#[test]
fn a_reconnected_device_keeps_its_new_slot_when_the_old_peer_cleans_up() {
    let peers = DevicePeers::default();
    let old = Arc::new(CancellationToken::new());
    let (old_control, mut old_rx) = mpsc::unbounded_channel();
    assert!(peers.register("device-a", "d:old", Arc::clone(&old), old_control));
    let new = Arc::new(CancellationToken::new());
    let (new_control, mut new_rx) = mpsc::unbounded_channel();
    assert!(peers.register("device-a", "d:new", Arc::clone(&new), new_control));
    assert_eq!(peers.len(), 1, "a reconnect reuses its own slot");

    // The replaced handle is dropped, so the old peer wakes and cleans up.
    assert!(matches!(
        old_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
    peers.remove("device-a", &old);
    assert_eq!(peers.len(), 1);
    assert!(
        peers.route("d:new", Control::Ice("candidate".to_string())),
        "the reconnected peer still answers on its own connection"
    );
    assert!(new_rx.try_recv().is_ok());
    assert!(!new.is_cancelled());
}

/// A frame that arrives before the device opens its channel is dropped, not
/// fatal: a hub event in the setup window must never end the peer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_frame_before_the_channel_opens_is_not_fatal() {
    let (offer, device) = peer::device_side::DeviceSide::offer()
        .await
        .expect("the device offer");
    let (_answer, host) = peer::Peer::answer_offer(Vec::new(), &offer)
        .await
        .expect("the host answer");
    // The device has not accepted the answer, so the host has no channel yet.
    assert!(
        host.send_text(r#"{"t":"session_added"}"#).await.is_ok(),
        "the setup window must not end the peer"
    );
    drop(device);
}

/// FR3: only the `canopy-session-v1` channel, ordered and reliable, is
/// accepted; a device that asks for another one has it closed unanswered.
/// (Reliability has no case: webrtc 0.21.1 reports both caps as `None` for
/// inbound channels, so an unreliable channel is indistinguishable on the
/// host side and the reliable half of the check cannot be exercised.)
async fn assert_channel_refused(label: &str, ordered: bool) {
    let (offer, device) = peer::device_side::DeviceSide::offer_channel(label, ordered)
        .await
        .expect("the device offer");
    let (answer, host) = peer::Peer::answer_offer(Vec::new(), &offer)
        .await
        .expect("the host answer");
    device.accept(&answer).await.expect("the device accepts");
    let closed = tokio::time::timeout(OPEN_TIMEOUT, async {
        loop {
            match device.next().await {
                Some(peer::device_side::DeviceEvent::Closed) | None => return,
                Some(_) => {}
            }
        }
    })
    .await;
    assert!(
        closed.is_ok(),
        "the host must close a channel {label:?} ordered={ordered}"
    );
    assert!(
        host.send_text("{}").await.is_ok(),
        "a refused channel never becomes the session channel"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_an_ordered_reliable_session_channel_is_accepted() {
    assert_channel_refused("some-other-channel", true).await;
    assert_channel_refused(peer::CHANNEL_LABEL, false).await;
}

/// One bridge answer through a channel: the remote-restriction half of the
/// loopback scenario, driven directly against the hub without a transport.
fn bridge_reply(state: &Arc<SessionsState>, frame: &Value) -> Vec<String> {
    let (out_tx, mut out_rx) = mpsc::unbounded_channel();
    let mut bridge = crate::remote::bridge::RemoteBridge::new(Arc::clone(state));
    bridge.handle_text(&frame.to_string(), &out_tx);
    drop(out_tx);
    let mut replies = Vec::new();
    while let Ok(reply) = out_rx.try_recv() {
        replies.push(reply);
    }
    replies
}

/// Assert one refusal frame with the `forbidden` code.
fn assert_forbidden(replies: &[String], req: Option<&str>) {
    assert_eq!(replies.len(), 1, "exactly one refusal, got {replies:?}");
    let error: Value = serde_json::from_str(&replies[0]).expect("error frame");
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "forbidden");
    match req {
        Some(req) => assert_eq!(error["req"], req),
        None => assert!(error.get("req").is_none(), "no req echoed: {error}"),
    }
}

/// The remote restrictions of FR3 without the transport: terminal sessions
/// are hidden from `list` and refused everywhere, `resize` always, `spawn`
/// unless `kind` is `agent` — the same rules the loopback test drives over
/// the data channel.
#[tokio::test]
async fn bridge_restrictions_hide_and_refuse_terminal_sessions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db =
        Arc::new(crate::db::Database::new(&dir.path().join("test.db")).expect("test database"));
    let hub = SessionHub::with_broadcast_capacity(db, "test-host".to_string(), 256);
    let workdir = tempfile::tempdir().expect("workdir");
    let (cat, _kill_cat) = spawn_session(&hub, SessionKind::Agent, workdir.path());
    let (terminal, _kill_terminal) = spawn_session(&hub, SessionKind::Terminal, workdir.path());
    let state = SessionsState::for_test(hub, "token".to_string());

    // The listing keeps the agent session and hides the terminal one.
    let replies = bridge_reply(&state, &json!({"t": "list", "req": "l1"}));
    assert_eq!(replies.len(), 1);
    let listed: Value = serde_json::from_str(&replies[0]).expect("sessions frame");
    assert_eq!(listed["t"], "sessions");
    assert_eq!(listed["req"], "l1");
    let items = listed["items"].as_array().expect("items array");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(ids.contains(&cat.id.as_str()), "agent listed: {ids:?}");
    assert!(
        !ids.contains(&terminal.id.as_str()),
        "terminal hidden: {ids:?}"
    );

    // attach/input/prompt/kill aimed at a terminal answer forbidden.
    assert_forbidden(
        &bridge_reply(&state, &json!({"t": "attach", "id": terminal.id})),
        None,
    );
    assert_forbidden(
        &bridge_reply(
            &state,
            &json!({"t": "input", "id": terminal.id, "data": encode(b"x")}),
        ),
        None,
    );
    assert_forbidden(
        &bridge_reply(
            &state,
            &json!({"t": "prompt", "req": "p1", "id": terminal.id, "text": "hi"}),
        ),
        Some("p1"),
    );
    assert_forbidden(
        &bridge_reply(
            &state,
            &json!({"t": "kill", "req": "k1", "id": terminal.id}),
        ),
        Some("k1"),
    );

    // resize is refused even for the session a device may use.
    assert_forbidden(
        &bridge_reply(
            &state,
            &json!({"t": "resize", "id": cat.id, "cols": 120, "rows": 40}),
        ),
        None,
    );

    // spawn accepts only kind agent; a terminal is refused before it spawns.
    assert_forbidden(
        &bridge_reply(
            &state,
            &json!({"t": "spawn", "req": "s1", "kind": "terminal", "cli": "cat", "workdir": "/tmp"}),
        ),
        Some("s1"),
    );
    let bad = bridge_reply(
        &state,
        &json!({"t": "spawn", "req": "s2", "kind": "banana", "cli": "cat", "workdir": "/tmp"}),
    );
    assert_eq!(bad.len(), 1);
    let error: Value = serde_json::from_str(&bad[0]).expect("error frame");
    assert_eq!(error["t"], "error");
    assert_eq!(error["code"], "bad_request");

    // The agent session answers attach with its snapshot.
    let replies = bridge_reply(&state, &json!({"t": "attach", "id": cat.id}));
    assert!(!replies.is_empty(), "attach answers at least a snapshot");
    let first: Value = serde_json::from_str(&replies[0]).expect("snapshot frame");
    assert_eq!(first["t"], "snapshot");
    assert_eq!(first["id"], cat.id);
}
