//! Reconnect with the stored device key after any drop.
//!
//! Pair once, connect, drop the link — a dead relay socket or a dropped
//! device peer — then reconnect from the same stored key: the reconnect
//! answers with the same device row, never a new one, and never `busy`
//! while a slot is genuinely free. Each test breaks when its production
//! liveness check is skipped: the relay idle timeout or the serve watchdog.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::remote::connection::{
    spawn_with_watchdog, DevicePeers, PeerSession, WatchdogConfig, MAX_CHANNEL_FRAME,
};
use crate::remote::devices::{device_id, RemoteDevices};
use crate::remote::hostkey::{verify_p1363, HostKey};
use crate::remote::pairing::{derive_k_pair, mac_init, normalize_code, PairingState};
use crate::remote::peer;
use crate::remote::peer::device_side::{DeviceEvent, DeviceSide};
use crate::remote::relay::{
    connect_ws, serve_socket_with_idle_timeout, websocket_url, ConnectionRegistry, HostHeaders,
    RelayRuntime, SignalTx,
};
use crate::remote::signaling::{self, offer_message, verify_offer, Refusal};
use crate::remote::trust::Grant;
use crate::sessions::ws::SessionsState;
use crate::sessions::SessionHub;

const OWNER: &str = "octocat";
/// How long one answer may take to arrive on the signal channel.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// How long eviction may take to empty the peer table.
const EVICT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the first sends may wait for the data channel to open.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The daemon path under test: one registry, one peer table, one runtime
/// answering offers through `route`, one signal inbox collecting answers.
struct Harness {
    dir: tempfile::TempDir,
    db: Database,
    host_id: String,
    host_spki: Vec<u8>,
    devices: RemoteDevices,
    runtime: RelayRuntime,
    peers: DevicePeers,
    connections: ConnectionRegistry,
    sessions: Arc<SessionsState>,
    signal: SignalTx,
    inbox: mpsc::UnboundedReceiver<(String, Value)>,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("test.db")).expect("database");
        let mut config = CanopyConfig::default();
        config.remote.enabled = true;
        config.save_for_tests(dir.path()).expect("config");
        let host = HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key");
        let host_id = host.host_id();
        let host_spki = host.spki_der().to_vec();
        let connections = ConnectionRegistry::default();
        let peers = DevicePeers::default();
        let mut runtime = RelayRuntime::new(
            host,
            db.clone(),
            OWNER.to_string(),
            PairingState::new(dir.path()),
            connections.clone(),
        );
        let sessions = SessionsState::for_test(
            SessionHub::new(Arc::new(db.clone()), "test-host".to_string()),
            "test-token".to_string(),
        );
        runtime.set_test_harness(Arc::clone(&sessions), peers.clone());
        let devices = RemoteDevices::new(db.clone());
        let (signal, inbox) = mpsc::unbounded_channel();
        Self {
            dir,
            db,
            host_id,
            host_spki,
            devices,
            runtime,
            peers,
            connections,
            sessions,
            signal,
            inbox,
        }
    }

    fn data_dir(&self) -> &Path {
        self.dir.path()
    }

    /// Pair one device key straight into the registry; returns the key and
    /// its row id.
    fn pair(&self, scalar: u8) -> (HostKey, String) {
        let key = HostKey::from_scalar_bytes(&[scalar; 32]).expect("device key");
        let row = self
            .devices
            .pair(
                key.spki_der().to_vec(),
                "browser",
                OWNER,
                "2026-01-01T00:00:00Z",
            )
            .expect("pair");
        (key, row.id)
    }

    fn rows(&self) -> usize {
        self.devices.list().expect("list").len()
    }

    /// One offer body with a real SDP, signed by `key`, fresh now.
    fn offer(&self, key: &HostKey, id: &str, sdp: &str) -> Value {
        let ts = chrono::Utc::now().timestamp();
        let sig = b64(&key.sign(offer_message(&self.host_id, ts, sdp).as_bytes()));
        json!({
            "t": "offer",
            "device_id": id,
            "ts": ts.to_string(),
            "sdp": sdp,
            "sig": sig,
        })
    }

    /// Route one offer through the daemon and await its signalled reply:
    /// the `answer` or `connect_fail` addressed to `from`. Trickle from an
    /// earlier peer of a previous cycle is not our reply and is skipped.
    async fn route_and_answer(&mut self, body: &Value, from: &str) -> (String, Value) {
        assert_eq!(
            self.runtime
                .route(body, chrono::Utc::now().timestamp(), from, &self.signal),
            None,
            "offers answer over the signal channel, never inline"
        );
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        loop {
            assert!(Instant::now() < deadline, "a reply in time");
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (to, reply) = tokio::time::timeout(remaining, self.inbox.recv())
                .await
                .expect("a reply in time")
                .expect("the signal channel stays open");
            if to == from && (reply["t"] == "answer" || reply["t"] == "connect_fail") {
                return (to, reply);
            }
        }
    }
}

/// One device-side offer. Boxed so the test futures stay small: the raw
/// offer future tops the linter's size budget.
async fn device_offer() -> (String, DeviceSide) {
    Box::pin(DeviceSide::offer())
        .await
        .expect("the device offer")
}

/// Occupy one peer slot with a never-opened zombie: the host answered, the
/// device never completed the handshake — no ICE, no `Open`, no `Closed`,
/// nothing for a serve loop without a watchdog to end on.
async fn spawn_zombie(harness: &Harness, device_id: &str, from: &str, grace: Duration) {
    let (offer_sdp, device) = device_offer().await;
    let (_answer_sdp, host_peer) = Box::pin(peer::Peer::answer_offer(Vec::new(), &offer_sdp))
        .await
        .expect("the host answer");
    drop(device);
    let token = harness.connections.register(device_id);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    assert!(harness
        .peers
        .register(device_id, from, token.clone(), control_tx));
    let (signal_tx, _signal_rx) = mpsc::unbounded_channel();
    spawn_with_watchdog(
        PeerSession {
            peer: host_peer,
            device_id: device_id.to_string(),
            state: Arc::clone(&harness.sessions),
            peers: harness.peers.clone(),
            connections: harness.connections.clone(),
            token,
            control: control_rx,
            signal: signal_tx,
            from: from.to_string(),
            grant: Grant::All,
            max_frame: MAX_CHANNEL_FRAME,
        },
        WatchdogConfig {
            tick: Duration::from_millis(100),
            disconnect_grace: grace,
            open_grace: grace,
        },
    );
}

/// Wait until no peer slot is occupied; fails when a slot leaks.
async fn wait_for_eviction(peers: &DevicePeers, what: &str) {
    let deadline = Instant::now() + EVICT_TIMEOUT;
    while !peers.live_ids().is_empty() {
        assert!(
            Instant::now() < deadline,
            "{what}: the slot never freed without a bye"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn test_headers(host_id: &str) -> HostHeaders {
    HostHeaders {
        token: "test-token".to_string(),
        host_id: host_id.to_string(),
        label: "test-host".to_string(),
    }
}

/// The half-open shape: the relay holds the socket and goes silent — no
/// reads, no Pongs, no Close.
async fn silent_upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|socket: WebSocket| async move {
        let _held = socket;
        tokio::time::sleep(Duration::from_secs(300)).await;
    })
}

/// A healthy relay: every Ping gets its Pong.
async fn healthy_upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|mut socket: WebSocket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            if let AxumMessage::Ping(payload) = message {
                let _ = socket.send(AxumMessage::Pong(payload)).await;
            }
        }
    })
}

async fn start_relay(silent: bool) -> String {
    let app = if silent {
        Router::new().route("/ws/host", get(silent_upgrade))
    } else {
        Router::new().route("/ws/host", get(healthy_upgrade))
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the fake relay");
    let addr = listener.local_addr().expect("fake relay addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

async fn serve_against(
    harness: &Harness,
    url: &str,
    idle_timeout: Duration,
) -> tokio::task::JoinHandle<anyhow::Result<()>> {
    let socket = connect_ws(&websocket_url(url), &test_headers(&harness.host_id))
        .await
        .expect("connect");
    let runtime = Arc::new(RelayRuntime::new(
        HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key"),
        harness.db.clone(),
        OWNER.to_string(),
        PairingState::new(harness.data_dir()),
        harness.connections.clone(),
    ));
    tokio::spawn(serve_socket_with_idle_timeout(
        socket,
        runtime,
        harness.data_dir().to_path_buf(),
        idle_timeout,
    ))
}

/// Pump trickle both ways until the device hears `hello`: the answer arrives
/// on the signal channel first, then host and device candidates cross until
/// the channel opens.
async fn pump_to_hello(harness: &mut Harness, device: &DeviceSide, from: &str) {
    let answer = loop {
        let (to, body) = tokio::time::timeout(ANSWER_TIMEOUT, harness.inbox.recv())
            .await
            .expect("a reply in time")
            .expect("the signal channel stays open");
        assert_eq!(to, from);
        if body["t"] == "answer" {
            break body;
        }
        if let Some(candidate) = signaling::ice_candidate(&body) {
            device
                .add_candidate(candidate)
                .await
                .expect("host candidate");
        }
    };
    device
        .accept(answer["sdp"].as_str().expect("answer sdp"))
        .await
        .expect("the device accepts the answer");
    let deadline = Instant::now() + OPEN_TIMEOUT;
    loop {
        assert!(Instant::now() < deadline, "hello never arrived");
        tokio::select! {
            event = device.next() => match event {
                Some(DeviceEvent::Candidate(candidate)) => {
                    let now = chrono::Utc::now().timestamp();
                    harness.runtime.route(
                        &signaling::ice_body(&candidate),
                        now,
                        from,
                        &harness.signal,
                    );
                }
                Some(DeviceEvent::Frame(bytes)) => {
                    let text = String::from_utf8(bytes).expect("frames are UTF-8");
                    let frame: Value = serde_json::from_str(&text).expect("frames are JSON");
                    if frame["t"] == "hello" {
                        return;
                    }
                }
                _ => panic!("the device ended before hello"),
            },
            signalled = harness.inbox.recv() => {
                let (_, body) = signalled.expect("the signal channel stays open");
                if let Some(candidate) = signaling::ice_candidate(&body) {
                    device.add_candidate(candidate).await.expect("host candidate");
                }
            }
        }
    }
}

/// A dead relay socket is noticed without the toggle dance, and the same
/// stored key reconnects with the same row. Breaks when the idle timeout is
/// skipped: the silent relay never ends the socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_kill_drop_reconnects_with_the_same_row() {
    let mut harness = Harness::new();
    let (key, id) = harness.pair(0x22);

    // Connect: a real offer through the daemon route answers, signed by the
    // host key.
    let (offer_sdp, device) = device_offer().await;
    let body = harness.offer(&key, &id, &offer_sdp);
    let offer_sig = body["sig"].as_str().expect("offer sig").to_string();
    let (to, answer) = harness.route_and_answer(&body, "d:1").await;
    assert_eq!(to, "d:1");
    assert_eq!(answer["t"], "answer");
    let answer_sdp = answer["sdp"].as_str().expect("answer sdp");
    let answer_sig: Vec<u8> = base64::engine::general_purpose::STANDARD
        .decode(answer["sig"].as_str().expect("answer sig"))
        .expect("sig base64");
    let expected = signaling::answer_message(&id, &offer_sig, answer_sdp);
    assert!(
        verify_p1363(&harness.host_spki, expected.as_bytes(), &answer_sig),
        "the answer must verify with the host key"
    );
    drop(device);

    // Relay kill, half-open shape: the daemon must notice on its own and
    // return, so the supervisor reconnects.
    let url = start_relay(true).await;
    let serving = serve_against(&harness, &url, Duration::from_secs(2)).await;
    let ended = tokio::time::timeout(Duration::from_secs(10), serving)
        .await
        .expect("the dead socket is noticed in time")
        .expect("the serve task");
    assert!(
        ended.is_err(),
        "a silent relay must end the socket with an error so the supervisor reconnects"
    );

    // A relay that answers Pongs keeps the socket up past the idle timeout:
    // no false churn on a healthy link.
    let url = start_relay(false).await;
    let serving = serve_against(&harness, &url, Duration::from_secs(2)).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(
        !serving.is_finished(),
        "a ponging relay must not trip the idle timeout"
    );
    serving.abort();

    // Reconnect from the same stored key with a fresh timestamp: answers,
    // same row, no new row.
    let (offer_sdp, device) = device_offer().await;
    let body = harness.offer(&key, &id, &offer_sdp);
    let (to, answer) = harness.route_and_answer(&body, "d:2").await;
    assert_eq!(to, "d:2");
    assert_eq!(answer["t"], "answer");
    let rows = harness.devices.list().expect("list");
    assert_eq!(rows.len(), 1, "the reconnect adds no row");
    assert_eq!(rows[0].id, id, "the reconnect keeps its row");
    harness.peers.close(&id);
    drop(device);
}

/// A client-side drop without a bye frees the slot, and the same stored key
/// reconnects with the same row. Breaks when the watchdog is skipped: the
/// never-opened peer holds its slot forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn client_drop_frees_the_slot_for_the_same_key() {
    let mut harness = Harness::new();
    let (key, id) = harness.pair(0x22);
    spawn_zombie(&harness, &id, "d:1", Duration::from_secs(1)).await;
    assert!(harness.peers.live_ids().contains(&id));

    // The dropped device never completed its handshake; the slot frees
    // without any bye.
    wait_for_eviction(&harness.peers, "the dropped peer").await;

    // Reconnect from the same stored key: answers, same row, no new row.
    let (offer_sdp, device) = device_offer().await;
    let body = harness.offer(&key, &id, &offer_sdp);
    let (to, answer) = harness.route_and_answer(&body, "d:2").await;
    assert_eq!(to, "d:2");
    assert_eq!(answer["t"], "answer");
    assert_eq!(harness.rows(), 1, "the reconnect adds no row");
    harness.peers.close(&id);
    drop(device);
}

/// Four dead peers fill every slot; the watchdog evicts them and the fifth
/// offer answers instead of `busy`. Breaks when the watchdog is skipped:
/// the fifth offer stays `busy` forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dead_peer_eviction_frees_busy_slots() {
    let mut harness = Harness::new();
    let mut ids = Vec::new();
    for scalar in 0x22u8..=0x26 {
        let (_, id) = harness.pair(scalar);
        ids.push(id);
    }
    tokio::join!(
        spawn_zombie(&harness, &ids[0], "d:0", Duration::from_secs(3)),
        spawn_zombie(&harness, &ids[1], "d:1", Duration::from_secs(3)),
        spawn_zombie(&harness, &ids[2], "d:2", Duration::from_secs(3)),
        spawn_zombie(&harness, &ids[3], "d:3", Duration::from_secs(3)),
    );
    assert_eq!(harness.peers.len(), 4, "all four slots are occupied");

    // The fifth verified offer while full: `busy`, before any peer is built.
    let fifth = HostKey::from_scalar_bytes(&[0x26; 32]).expect("fifth device key");
    let (offer_sdp, device) = device_offer().await;
    drop(device);
    let body = harness.offer(&fifth, &ids[4], &offer_sdp);
    let (to, refusal) = harness.route_and_answer(&body, "d:5").await;
    assert_eq!(to, "d:5");
    assert_eq!(refusal, json!({"t": "connect_fail", "reason": "busy"}));

    // The watchdog evicts the zombies; the same device retries fresh and
    // answers.
    wait_for_eviction(&harness.peers, "the four zombies").await;
    let (offer_sdp, device) = device_offer().await;
    let body = harness.offer(&fifth, &ids[4], &offer_sdp);
    let (to, answer) = harness.route_and_answer(&body, "d:6").await;
    assert_eq!(to, "d:6");
    assert_eq!(answer["t"], "answer", "eviction must free the busy slot");
    assert_eq!(harness.rows(), 5);
    harness.peers.close(&ids[4]);
    drop(device);
}

/// A fresh identity pairs over the real pairing path and runs the full
/// pair -> connect -> drop -> reconnect cycle green, the same path a phone
/// uses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn fresh_identity_runs_the_full_cycle() {
    let mut harness = Harness::new();
    let device_key = HostKey::from_scalar_bytes(&[0x33; 32]).expect("device key");
    let pairing = PairingState::new(harness.data_dir());
    let now = chrono::Utc::now().timestamp();
    let created = pairing.create(&harness.host_id, now).expect("a code");
    let code_key = derive_k_pair(&normalize_code(&created.code), &harness.host_id);
    let nonce_b = b64(&(0u8..16).collect::<Vec<_>>());
    let host = HostKey::from_scalar_bytes(&[0x11; 32]).expect("host key");
    let init = json!({
        "t": "pair_init",
        "device_spki": device_key.spki_b64(),
        "device_label": "phone",
        "nonce_b": nonce_b,
        "mac": b64(&mac_init(&code_key, &device_key.spki_b64(), &nonce_b)),
    });
    let reply = pairing
        .respond_with_nonce(&init, &host, &harness.devices, OWNER, now + 1, [0x33; 16])
        .expect("respond");
    assert_eq!(reply["t"], "pair_ok");
    let id = device_id(device_key.spki_der());
    assert_eq!(harness.rows(), 1);

    // Connect: the channel opens and the device hears `hello`.
    let (offer_sdp, device) = device_offer().await;
    let body = harness.offer(&device_key, &id, &offer_sdp);
    assert_eq!(
        harness.runtime.route(
            &body,
            chrono::Utc::now().timestamp(),
            "d:1",
            &harness.signal
        ),
        None
    );
    pump_to_hello(&mut harness, &device, "d:1").await;

    // Drop the link client-side, without a bye.
    drop(device);

    // Reconnect from the same stored key: answers, same row, no new row.
    let (offer_sdp, device) = device_offer().await;
    let body = harness.offer(&device_key, &id, &offer_sdp);
    let (to, answer) = harness.route_and_answer(&body, "d:2").await;
    assert_eq!(to, "d:2");
    assert_eq!(answer["t"], "answer");
    let rows = harness.devices.list().expect("list");
    assert_eq!(rows.len(), 1, "the reconnect adds no row");
    assert_eq!(rows[0].id, id, "the reconnect keeps its row");
    harness.peers.close(&id);
    drop(device);
}

/// The refusal order stays `unknown_device`, then `stale`, then
/// `bad_signature`, and three drop/reconnect cycles add no row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refusals_keep_their_order_and_rows_stay_stable() {
    let mut harness = Harness::new();
    let (key, id) = harness.pair(0x22);
    let now = chrono::Utc::now().timestamp();
    let sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n";

    // Unknown first: even with a stale timestamp and a garbage signature,
    // a missing id refuses `unknown_device`.
    let no_id = json!({"t": "offer", "ts": now - 10_000, "sdp": sdp, "sig": "AA=="});
    assert_eq!(
        verify_offer(&harness.devices, &harness.host_id, &no_id, now).err(),
        Some(Refusal::UnknownDevice)
    );
    let unknown = json!({
        "t": "offer",
        "device_id": "0000000000000000",
        "ts": now - 10_000,
        "sdp": sdp,
        "sig": "AA==",
    });
    assert_eq!(
        verify_offer(&harness.devices, &harness.host_id, &unknown, now).err(),
        Some(Refusal::UnknownDevice)
    );
    // Stale before `bad_signature`: an expired timestamp with a garbage
    // signature refuses `stale`, not `bad_signature`.
    let stale = json!({
        "t": "offer",
        "device_id": id,
        "ts": now - 10_000,
        "sdp": sdp,
        "sig": "AA==",
    });
    assert_eq!(
        verify_offer(&harness.devices, &harness.host_id, &stale, now).err(),
        Some(Refusal::Stale)
    );
    // A fresh timestamp with a bad signature refuses `bad_signature`.
    let bad = json!({"t": "offer", "device_id": id, "ts": now, "sdp": sdp, "sig": "AA=="});
    assert_eq!(
        verify_offer(&harness.devices, &harness.host_id, &bad, now).err(),
        Some(Refusal::BadSignature)
    );
    // Control: a valid fresh offer verifies.
    let ts = chrono::Utc::now().timestamp();
    let sig = b64(&key.sign(offer_message(&harness.host_id, ts, sdp).as_bytes()));
    let good = json!({"t": "offer", "device_id": id, "ts": ts, "sdp": sdp, "sig": sig});
    verify_offer(&harness.devices, &harness.host_id, &good, ts).expect("a valid offer verifies");

    // Three drop/reconnect cycles add no row.
    for cycle in 0..3 {
        let from = format!("d:{cycle}");
        let (offer_sdp, device) = device_offer().await;
        let body = harness.offer(&key, &id, &offer_sdp);
        let (to, answer) = harness.route_and_answer(&body, &from).await;
        assert_eq!(to, from);
        assert_eq!(answer["t"], "answer", "cycle {cycle} answers");
        drop(device);
        let rows = harness.devices.list().expect("list");
        assert_eq!(rows.len(), 1, "cycle {cycle} adds no row");
        assert_eq!(rows[0].id, id, "cycle {cycle} keeps its row");
    }
    harness.peers.close(&id);
}
