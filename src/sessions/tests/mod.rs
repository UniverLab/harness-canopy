//! Session hub tests, by area: auth, roundtrip, snapshot, resize/kill,
//! bad frames. The hub runs in-process; live children are `sh`/`cat`/`true`,
//! never a canopy binary.

mod auth;
mod badframe;
mod resize_kill;
mod roundtrip;
mod snapshot;

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt as _, StreamExt as _};

use crate::db::Database;
use crate::sessions::ws::SessionsState;
use crate::sessions::{SessionHub, SessionKind, SpawnRequest};

/// Type of an established test WebSocket stream (plain `ws://`, no TLS).
type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// A scratch database that lives as long as the returned temp dir.
fn test_db() -> (Arc<Database>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = Database::new(&dir.path().join("test.db")).expect("test database");
    (Arc::new(db), dir)
}

fn test_hub(db: &Arc<Database>) -> SessionHub {
    SessionHub::with_broadcast_capacity(Arc::clone(db), "test-host".to_string(), 256)
}

/// Serve `state` on 127.0.0.1 with an ephemeral port. The server task dies
/// with the test process; tests abort the handle when done.
async fn serve_state(
    state: Arc<SessionsState>,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new()
        .route(
            "/sessions/ws",
            axum::routing::get(crate::sessions::ws::sessions_ws_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test server");
    let addr = listener.local_addr().expect("test server addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, handle)
}

/// A handshake request for the test server: start from the client-request
/// conversion (which generates the `Sec-WebSocket-Key` tungstenite requires
/// on a hand-built request) and add the bearer token when given.
fn ws_request(
    addr: std::net::SocketAddr,
    token: Option<&str>,
) -> tokio_tungstenite::tungstenite::http::Request<()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
    let mut request = format!("ws://{addr}/sessions/ws")
        .into_client_request()
        .expect("test ws request");
    if let Some(token) = token {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("auth header"),
        );
    }
    request
}

/// Connect a test client, with or without the bearer token.
async fn connect_client(addr: std::net::SocketAddr, token: Option<&str>) -> WsStream {
    let (stream, _) = tokio_tungstenite::connect_async(ws_request(addr, token))
        .await
        .expect("test ws connect");
    stream
}

/// Connect without asserting success, for tests expecting a 401.
async fn connect_raw(
    addr: std::net::SocketAddr,
    token: Option<&str>,
) -> Result<WsStream, Box<tokio_tungstenite::tungstenite::Error>> {
    tokio_tungstenite::connect_async(ws_request(addr, token))
        .await
        .map(|(stream, _)| stream)
        .map_err(Box::new)
}

async fn send_json(stream: &mut WsStream, value: &serde_json::Value) {
    stream
        .send(tokio_tungstenite::tungstenite::Message::Text(
            value.to_string().into(),
        ))
        .await
        .expect("send test frame");
}

/// Receive the next JSON text frame, failing the test on timeout.
async fn recv_json(stream: &mut WsStream, what: &str) -> serde_json::Value {
    try_recv_json(stream, Duration::from_secs(10))
        .await
        .unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// Receive the next JSON text frame within `within`, or `None` on timeout.
async fn try_recv_json(stream: &mut WsStream, within: Duration) -> Option<serde_json::Value> {
    tokio::time::timeout(within, async {
        loop {
            match stream.next().await {
                Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                    return serde_json::from_str(&text).expect("valid JSON frame");
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => panic!("ws error: {e}"),
                None => panic!("ws closed"),
            }
        }
    })
    .await
    .ok()
}

fn encode_b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn decode_b64(s: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.as_bytes())
        .expect("valid base64")
}

/// Spawn a request on `hub`, killing the session when the guard drops so no
/// test ever leaves a live child behind.
struct KillGuard {
    hub: SessionHub,
    id: String,
}

impl KillGuard {
    fn spawn(
        hub: &SessionHub,
        req: &SpawnRequest,
    ) -> (std::sync::Arc<crate::sessions::HubSession>, Self) {
        let session = hub.spawn(req).expect("test spawn");
        let guard = Self {
            hub: hub.clone(),
            id: session.id.clone(),
        };
        (session, guard)
    }

    fn id(&self) -> &str {
        &self.id
    }
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        let _ = self.hub.kill(&self.id);
    }
}

fn terminal_spawn(cli: &str, workdir: &str) -> SpawnRequest {
    SpawnRequest {
        kind: SessionKind::Terminal,
        cli: cli.to_string(),
        model: None,
        workdir: workdir.to_string(),
        name: None,
        prompt: None,
    }
}

fn agent_spawn(cli: &str, workdir: &str) -> SpawnRequest {
    SpawnRequest {
        kind: SessionKind::Agent,
        cli: cli.to_string(),
        model: None,
        workdir: workdir.to_string(),
        name: None,
        prompt: None,
    }
}
