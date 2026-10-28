use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use serde_json::{json, Value};
use tokio::sync::{oneshot, Mutex};

use super::*;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::remote::devices::device_id;
use crate::remote::hostkey::HostKey;
use crate::remote::pairing::{derive_k_pair, mac_init, normalize_code};
use crate::remote::signaling::MAX_DEVICE_PEERS;

fn test_db() -> Database {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("database");
    std::mem::forget(dir);
    db
}

fn test_sessions(db: &Database) -> Arc<crate::sessions::ws::SessionsState> {
    let hub = crate::sessions::SessionHub::new(Arc::new(db.clone()), "test-host".to_string());
    crate::sessions::ws::SessionsState::for_test(hub, "test-token".to_string())
}

fn headers() -> HostHeaders {
    HostHeaders {
        token: "gho_test_token".to_string(),
        host_id: "76805a63a6980c18".to_string(),
        label: "test-host".to_string(),
    }
}

#[derive(Clone)]
struct PairingRelay {
    body: Value,
    sender: Arc<Mutex<Option<oneshot::Sender<Value>>>>,
}

async fn pairing_upgrade(
    State(state): State<PairingRelay>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| pairing_socket(socket, state))
}

async fn pairing_socket(mut socket: WebSocket, state: PairingRelay) {
    let envelope = json!({"from": "d:1", "body": state.body});
    if socket
        .send(AxumMessage::Text(envelope.to_string().into()))
        .await
        .is_err()
    {
        return;
    }
    if let Some(Ok(AxumMessage::Text(text))) = socket.recv().await {
        if let Ok(reply) = serde_json::from_str::<Value>(&text) {
            if let Some(sender) = state.sender.lock().await.take() {
                let _ = sender.send(reply);
            }
        }
    }
}

async fn start_pairing_relay(body: Value) -> (String, oneshot::Receiver<Value>) {
    let (sender, receiver) = oneshot::channel();
    let state = PairingRelay {
        body,
        sender: Arc::new(Mutex::new(Some(sender))),
    };
    let app = Router::new()
        .route("/ws/host", get(pairing_upgrade))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}"), receiver)
}

#[tokio::test]
async fn fake_relay_carries_a_full_pairing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let mut config = CanopyConfig::default();
    config.remote.enabled = true;
    config.save_for_tests(dir.path()).expect("config");
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    let device = HostKey::from_scalar_bytes(&[0x22; 32]).expect("device key");
    let state = crate::remote::pairing::PairingState::new(dir.path());
    let created = state
        .create(&host.host_id(), chrono::Utc::now().timestamp())
        .expect("code");
    let key = derive_k_pair(&normalize_code(&created.code), &host.host_id());
    let nonce_b = base64::engine::general_purpose::STANDARD.encode((0u8..16).collect::<Vec<_>>());
    let device_spki = device.spki_b64();
    let body = json!({
        "t": "pair_init",
        "device_spki": device_spki,
        "device_label": "test browser",
        "nonce_b": nonce_b,
        "mac": base64::engine::general_purpose::STANDARD
            .encode(mac_init(&key, &device.spki_b64(), &nonce_b)),
    });
    let (url, reply) = start_pairing_relay(body).await;
    let socket = connect_ws(&websocket_url(&url), &headers())
        .await
        .expect("connect");
    let runtime = Arc::new(RelayRuntime::new(
        host,
        db.clone(),
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    ));
    let serving = tokio::spawn(serve_socket(
        socket,
        Arc::clone(&runtime),
        dir.path().to_path_buf(),
    ));
    let reply = tokio::time::timeout(Duration::from_secs(5), reply)
        .await
        .expect("relay reply timed out")
        .expect("relay reply");
    assert_eq!(reply["to"], "d:1");
    assert_eq!(reply["body"]["t"], "pair_ok");
    let stored = crate::remote::devices::RemoteDevices::new(db)
        .list()
        .expect("devices");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, device_id(device.spki_der()));
    serving.abort();
}

#[tokio::test]
async fn http_401_stops_without_reconnecting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let mut config = CanopyConfig::default();
    config.remote.enabled = true;
    let counter = Arc::new(AtomicUsize::new(0));
    let requests = Arc::clone(&counter);
    let app = Router::new().fallback(move || {
        let counter = Arc::clone(&requests);
        async move {
            counter.fetch_add(1, Ordering::SeqCst);
            axum::http::StatusCode::UNAUTHORIZED
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    config.remote.relay_url = format!("http://{addr}");
    config.save_for_tests(dir.path()).expect("config");
    crate::remote::auth::save_identity(
        dir.path(),
        &crate::remote::auth::GitHubIdentity {
            token: "gho_rejected".to_string(),
            login: "octocat".to_string(),
            id: 1,
        },
    )
    .expect("identity");
    let running = tokio::spawn(supervise(
        dir.path().to_path_buf(),
        db.clone(),
        test_sessions(&db),
    ));
    // Longer than the slowest first backoff (1200 ms): a 401 misclassified
    // as transient would have retried by now.
    tokio::time::sleep(Duration::from_millis(2500)).await;
    running.abort();
    assert_eq!(counter.load(Ordering::SeqCst), 1, "must not retry a 401");
    let recorded = crate::application::ports::StateRepository::get_state(&db, REMOTE_ERROR_KEY)
        .expect("state")
        .unwrap_or_default();
    assert_eq!(recorded, "github token rejected: run canopy remote login");
}

async fn hold_open_upgrade(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(|mut socket: WebSocket| async move {
        // Hold the socket open until the host goes away; the host side is
        // what the test watches.
        while socket.recv().await.is_some() {}
    })
}

#[tokio::test]
async fn remote_off_ends_a_live_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let mut config = CanopyConfig::default();
    config.remote.enabled = true;
    config.save_for_tests(dir.path()).expect("config");
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    let runtime = Arc::new(RelayRuntime::new(
        host,
        db,
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    ));

    let app = Router::new().route("/ws/host", get(hold_open_upgrade));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let socket = connect_ws(&websocket_url(&format!("http://{addr}")), &headers())
        .await
        .expect("connect");
    let serving = tokio::spawn(serve_socket(
        socket,
        Arc::clone(&runtime),
        dir.path().to_path_buf(),
    ));

    // The socket stays up while remote is enabled.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !serving.is_finished(),
        "a live socket must stay up while remote is enabled"
    );

    // `canopy remote off` rewrites the file; the next 1 s tick ends the socket.
    config.remote.enabled = false;
    config.save_for_tests(dir.path()).expect("config off");
    tokio::time::timeout(Duration::from_secs(5), serving)
        .await
        .expect("off must end the live socket within seconds")
        .expect("serve task")
        .expect("clean shutdown");
}

#[tokio::test]
async fn revoke_deletes_the_row_and_closes_a_live_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    let runtime = RelayRuntime::new(
        host,
        db.clone(),
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    let devices = crate::remote::devices::RemoteDevices::new(db.clone());
    let spki = HostKey::from_scalar_bytes(&[0x22; 32])
        .expect("device")
        .spki_der()
        .to_vec();
    let id = device_id(&spki);
    devices
        .pair(spki, "laptop", "octocat", "2026-01-01T00:00:00Z")
        .expect("pair");
    let token = runtime.connections().register(&id);
    assert!(!token.is_cancelled());

    // A live fake connection: it serves until its registry token is
    // cancelled, then reports the close.
    let (closed_tx, closed_rx) = oneshot::channel();
    let live_token = token.clone();
    let connection = tokio::spawn(async move {
        tokio::select! {
            _ = live_token.cancelled() => {}
            _ = tokio::time::sleep(Duration::from_secs(30)) => panic!("still open"),
        }
        let _ = closed_tx.send(());
    });
    assert!(runtime.revoke(&id).expect("revoke"));
    tokio::time::timeout(Duration::from_secs(5), closed_rx)
        .await
        .expect("revoke must close the live connection at once")
        .expect("close signal");
    connection.abort();
    assert!(token.is_cancelled());
    assert!(!devices.is_paired(&id).expect("is_paired"));

    // The CLI revokes from another process: the queue is drained by the
    // live runtime and closes that device's connection too.
    let other = HostKey::from_scalar_bytes(&[0x33; 32])
        .expect("other device")
        .spki_der()
        .to_vec();
    let other_id = device_id(&other);
    devices
        .pair(other, "phone", "octocat", "2026-01-01T00:00:00Z")
        .expect("pair other");
    let other_token = runtime.connections().register(&other_id);
    queue_revocation(&db, &other_id);
    runtime.drain_remote_revocations();
    assert!(other_token.is_cancelled());
    let queue = crate::application::ports::StateRepository::get_state(&db, REMOTE_REVOKE_KEY)
        .expect("queue state");
    assert!(
        queue.as_deref().unwrap_or_default().is_empty(),
        "drained ids must leave the queue, got {queue:?}"
    );
}

/// An old peer's cleanup must not close the same device's newer registration.
#[test]
fn an_old_peers_release_leaves_the_reconnect_live() {
    let registry = ConnectionRegistry::default();
    let old = registry.register("device-a");
    let new = registry.register("device-a");
    registry.release("device-a", &old);
    assert!(
        !new.is_cancelled(),
        "the reconnect must survive the old cleanup"
    );
    registry.close("device-a");
    assert!(new.is_cancelled(), "the current registration still closes");
}

#[test]
fn live_connections_survive_a_reconnect() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    // One supervisor, two sockets: both runtimes share the supervisor's
    // registry, so a revoke after a reconnect still finds the connection.
    let connections = ConnectionRegistry::default();
    let before = RelayRuntime::new(
        HostKey::load_or_create(dir.path()).expect("host key"),
        db.clone(),
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        connections.clone(),
    );
    let after = RelayRuntime::new(
        host,
        db.clone(),
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        connections,
    );
    let id = device_id(
        HostKey::from_scalar_bytes(&[0x22; 32])
            .expect("device")
            .spki_der(),
    );
    let token = before.connections().register(&id);
    queue_revocation(&db, &id);
    after.drain_remote_revocations();
    assert!(token.is_cancelled());
}

#[test]
fn backoff_starts_at_one_second_and_caps_at_sixty() {
    for attempt in 0..8 {
        let delay = backoff_delay(attempt);
        let base = (1u64 << attempt.min(6)).min(60) as f64;
        let millis = delay.as_millis() as f64;
        assert!(
            millis >= base * 800.0 && millis <= base * 1200.0,
            "attempt {attempt}: {millis} ms outside ±20% of {base} s"
        );
    }
}

#[test]
fn pair_link_and_websocket_url_are_built_from_the_relay_url() {
    assert_eq!(
        pair_link("https://remote.example/", "7K3QH-M9P2X"),
        "https://remote.example/pair#7K3QH-M9P2X"
    );
    assert_eq!(
        websocket_url("https://remote.example"),
        "wss://remote.example/ws/host"
    );
    assert_eq!(
        websocket_url("http://127.0.0.1:9000"),
        "ws://127.0.0.1:9000/ws/host"
    );
}

#[tokio::test]
async fn connect_ws_classifies_401_separately_from_transient_failures() {
    // A real 401 handshake answer.
    let app = Router::new().fallback(|| async { axum::http::StatusCode::UNAUTHORIZED });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let rejected = connect_ws(&websocket_url(&format!("http://{addr}")), &headers())
        .await
        .expect_err("the server answered 401");
    assert!(matches!(rejected, ConnectError::Unauthorized));

    // A refused socket is transient: it may work on a later attempt.
    let dead = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let dead_addr = dead.local_addr().expect("addr");
    drop(dead);
    let refused = connect_ws(&websocket_url(&format!("http://{dead_addr}")), &headers())
        .await
        .expect_err("the port is closed");
    assert!(matches!(refused, ConnectError::Transient(_)));
}

/// One offer body signed by `key` over the vector SDP, fresh at `ts`.
fn signed_offer(key: &HostKey, host_id: &str, ts: i64) -> Value {
    let sdp = "v=0\r\no=- 1 2 IN IP4 127.0.0.1\r\n";
    let message = crate::remote::signaling::offer_message(host_id, ts, sdp);
    let sig = base64::engine::general_purpose::STANDARD.encode(key.sign(message.as_bytes()));
    json!({
        "t": "offer", "device_id": device_id(key.spki_der()),
        "ts": ts, "sdp": sdp, "sig": sig,
    })
}

#[tokio::test]
async fn the_fifth_concurrent_offer_is_answered_busy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    let host_id = host.host_id();
    let devices = crate::remote::devices::RemoteDevices::new(db.clone());
    for scalar in 0x22u8..=0x26 {
        devices
            .pair(
                HostKey::from_scalar_bytes(&[scalar; 32])
                    .expect("device key")
                    .spki_der()
                    .to_vec(),
                "browser",
                "octocat",
                "2026-01-01T00:00:00Z",
            )
            .expect("pair a device");
    }
    let mut runtime = RelayRuntime::new(
        host,
        db.clone(),
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    runtime.sessions = Some(test_sessions(&db));
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    let ts = chrono::Utc::now().timestamp();

    // Four verified offers register four peers; no await between them, so the
    // spawned answer builds cannot finish first.
    for index in 0..MAX_DEVICE_PEERS {
        let key = HostKey::from_scalar_bytes(&[0x22u8 + index as u8; 32]).expect("device key");
        let body = signed_offer(&key, &host_id, ts);
        assert_eq!(
            runtime.route(&body, ts, &format!("d:{index}"), &signal),
            None
        );
    }
    assert_eq!(runtime.peers.len(), MAX_DEVICE_PEERS);

    // The fifth verified offer is refused `busy` before its peer is built.
    let fifth = HostKey::from_scalar_bytes(&[0x26; 32]).expect("fifth device key");
    assert_eq!(
        runtime.route(&signed_offer(&fifth, &host_id, ts), ts, "d:5", &signal),
        None
    );
    let (to, refusal) = inbox
        .try_recv()
        .expect("the fifth offer is refused at once");
    assert_eq!(to, "d:5");
    assert_eq!(refusal, json!({"t": "connect_fail", "reason": "busy"}));
}

/// The contract's `ice{candidate}` and `bye` bodies carry no device id: the
/// relay envelope's `from` is the only address, so routing must not need one.
#[tokio::test]
async fn ice_and_bye_are_routed_by_the_relay_connection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let mut runtime = RelayRuntime::new(
        HostKey::load_or_create(dir.path()).expect("host key"),
        db,
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    let peers = crate::remote::connection::DevicePeers::default();
    let token = Arc::new(tokio_util::sync::CancellationToken::new());
    let (control, mut inbox) = tokio::sync::mpsc::unbounded_channel();
    assert!(peers.register("bfcba2b742a807bb", "d:7", Arc::clone(&token), control));
    runtime.peers = peers;
    let (signal, _signal_rx) = tokio::sync::mpsc::unbounded_channel();
    let now = chrono::Utc::now().timestamp();

    let ice = json!({
        "t": "ice",
        "candidate": "candidate:1 1 UDP 1694498815 127.0.0.1 5000 typ host",
    });
    assert_eq!(runtime.route(&ice, now, "d:7", &signal), None);
    match inbox.try_recv() {
        Ok(crate::remote::connection::Control::Ice(candidate)) => {
            assert!(candidate.contains("candidate:1"), "candidate: {candidate}");
        }
        other => panic!("the ice body must reach the peer: {other:?}"),
    }

    // `bye` closes that peer; no device id in the body.
    assert_eq!(
        runtime.route(&json!({"t": "bye"}), now, "d:7", &signal),
        None
    );
    assert!(token.is_cancelled(), "bye must close the peer");
}

/// A `bye` body `device_id` is never honored: only the sender's own peer
/// closes, so one paired device cannot knock another one off.
#[test]
fn bye_naming_another_device_does_not_close_it() {
    let peers = crate::remote::connection::DevicePeers::default();
    let token_a = Arc::new(tokio_util::sync::CancellationToken::new());
    let (control_a, _rx_a) = tokio::sync::mpsc::unbounded_channel();
    assert!(peers.register("device-a", "d:a", Arc::clone(&token_a), control_a));
    let token_b = Arc::new(tokio_util::sync::CancellationToken::new());
    let (control_b, _rx_b) = tokio::sync::mpsc::unbounded_channel();
    assert!(peers.register("device-b", "d:b", Arc::clone(&token_b), control_b));

    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let mut runtime = RelayRuntime::new(
        HostKey::load_or_create(dir.path()).expect("host key"),
        db,
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    runtime.peers = peers;
    let (signal, _signal_rx) = tokio::sync::mpsc::unbounded_channel();
    let now = chrono::Utc::now().timestamp();

    // From b's connection, naming a: only b closes.
    assert_eq!(
        runtime.route(
            &json!({"t": "bye", "device_id": "device-a"}),
            now,
            "d:b",
            &signal
        ),
        None
    );
    assert!(!token_a.is_cancelled(), "a's peer must survive b's bye");
    assert!(token_b.is_cancelled(), "bye closes the sender's own peer");
}

/// A verified offer over the real signaling path gets a signed answer and
/// refreshes `last_seen`: the offer half of FR2/FR4 through `route`, with a
/// live SDP (the vector stub is not an SDP any peer could parse) signed with
/// the contract's string `ts`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verified_offer_gets_a_signed_answer_and_updates_last_seen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = test_db();
    let host = HostKey::load_or_create(dir.path()).expect("host key");
    let host_id = host.host_id();
    let host_spki = host.spki_der().to_vec();
    let devices = crate::remote::devices::RemoteDevices::new(db.clone());
    let device_key = HostKey::from_scalar_bytes(&[0x22; 32]).expect("device key");
    let id = device_id(device_key.spki_der());
    devices
        .pair(
            device_key.spki_der().to_vec(),
            "browser",
            "octocat",
            "2026-01-01T00:00:00Z",
        )
        .expect("pair the device");
    assert!(
        devices
            .get(&id)
            .expect("get")
            .expect("row")
            .last_seen
            .is_none(),
        "unseen before the offer"
    );

    let mut runtime = RelayRuntime::new(
        host,
        db.clone(),
        "octocat".to_string(),
        crate::remote::pairing::PairingState::new(dir.path()),
        ConnectionRegistry::default(),
    );
    runtime.sessions = Some(test_sessions(&db));
    // `relay_url` stays empty: the `/turn` fetch fails and the peer falls
    // back to host candidates, which is all loopback needs.
    let (signal, mut inbox) = tokio::sync::mpsc::unbounded_channel();

    let (offer_sdp, device) = crate::remote::peer::device_side::DeviceSide::offer()
        .await
        .expect("the device offer");
    let ts = chrono::Utc::now().timestamp();
    let message = format!("canopy-remote/v1/offer\n{host_id}\n{ts}\n{offer_sdp}");
    let sig = base64::engine::general_purpose::STANDARD.encode(device_key.sign(message.as_bytes()));
    let body = json!({
        "t": "offer", "device_id": id,
        "ts": ts.to_string(), "sdp": offer_sdp, "sig": sig,
    });
    assert_eq!(runtime.route(&body, ts, "d:9", &signal), None);

    let (to, answer) = tokio::time::timeout(Duration::from_secs(20), inbox.recv())
        .await
        .expect("the answer arrives in time")
        .expect("the answer");
    assert_eq!(to, "d:9");
    assert_eq!(answer["t"], "answer");
    let answer_sdp = answer["sdp"].as_str().expect("answer sdp");
    let answer_sig: Vec<u8> = base64::engine::general_purpose::STANDARD
        .decode(answer["sig"].as_str().expect("answer sig"))
        .expect("sig base64");
    assert_eq!(answer_sig.len(), 64, "P1363, not DER");
    let expected = crate::remote::signaling::answer_message(&id, &sig, answer_sdp);
    assert!(
        crate::remote::hostkey::verify_p1363(&host_spki, expected.as_bytes(), &answer_sig),
        "the answer must verify with the host key"
    );

    assert!(
        devices
            .get(&id)
            .expect("get")
            .expect("row")
            .last_seen
            .is_some(),
        "success refreshes last_seen"
    );

    runtime.peers.close(&id);
    drop(device);
}
