//! The daemon's WebSocket client to the relay.
//!
//! When `[remote] enabled = true` and an identity is stored, the supervisor
//! keeps a socket to `<relay_url>/ws/host` with the GitHub bearer token and
//! this host's id and label. Frames are JSON envelopes: relay → host
//! `{from: "d:<conn>", body}` and host → relay `{to: "d:<conn>", body}`.
//! Reconnects back off from one second to one minute with ±20% jitter; an
//! HTTP 401 stops and records `github token rejected: run canopy remote
//! login` instead of retrying.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use futures::SinkExt as _;
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use crate::application::ports::StateRepository as _;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::remote::auth;
use crate::remote::connection::{self, Control, DevicePeers, PeerSession};
use crate::remote::devices::RemoteDevices;
use crate::remote::hostkey::HostKey;
use crate::remote::live::{RelayPhase, RemoteLive};
use crate::remote::pairing::PairingState;
use crate::remote::peer;
use crate::remote::signaling::{self, Refusal};
use crate::sessions::ws::SessionsState;

/// `daemon_state` key carrying the last terminal relay error.
pub const REMOTE_ERROR_KEY: &str = "remote_error";
/// `daemon_state` key holding device ids revoked by another process.
pub const REMOTE_REVOKE_KEY: &str = "remote_revoke";
/// Longest reconnect delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A connected relay socket.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Outbound relay bodies: `(to, body)`. One connection's task pushes answers
/// and trickled candidates here; the socket loop writes them.
pub type SignalTx = tokio::sync::mpsc::UnboundedSender<(String, Value)>;

/// Headers every host socket carries.
#[derive(Debug, Clone)]
pub struct HostHeaders {
    /// GitHub OAuth token (never logged).
    pub token: String,
    /// This host's `host_id`.
    pub host_id: String,
    /// Human label or hostname.
    pub label: String,
}

/// Why a connection attempt ended.
#[derive(Debug)]
pub enum ConnectError {
    /// HTTP 401: the GitHub token was rejected; do not retry it.
    Unauthorized,
    /// Anything else that may succeed on a later attempt.
    Transient(String),
}

/// `<relay_url>/pair#<code>`.
pub fn pair_link(relay_url: &str, code: &str) -> String {
    format!("{}/pair#{}", relay_url.trim_end_matches('/'), code)
}

/// Turn the configured HTTP relay URL into the host WebSocket URL.
pub fn websocket_url(relay_url: &str) -> String {
    let base = relay_url.trim_end_matches('/');
    if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}/ws/host")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}/ws/host")
    } else {
        format!("{base}/ws/host")
    }
}

/// Reconnect delay for `attempt` (0-based): 1 s doubling to 60 s, ±20%.
pub fn backoff_delay(attempt: u32) -> Duration {
    let exponent = attempt.min(6);
    let base = (1u64 << exponent).min(MAX_BACKOFF.as_secs());
    let millis = base * 1000;
    let low = millis * 80 / 100;
    let spread = millis * 40 / 100;
    let jitter = if spread == 0 {
        0
    } else {
        use rand::RngExt as _;
        rand::rng().random_range(0..=spread)
    };
    Duration::from_millis(low + jitter)
}

/// Build the handshake request with the identity and host headers.
pub fn request_for(
    url: &str,
    headers: &HostHeaders,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request> {
    let mut request = url
        .into_client_request()
        .with_context(|| "invalid relay websocket URL")?;
    let values = request.headers_mut();
    values.insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", headers.token))
            .context("invalid GitHub token header")?,
    );
    values.insert(
        "X-Canopy-Host-Id",
        HeaderValue::from_str(&headers.host_id).context("invalid host id header")?,
    );
    values.insert(
        "X-Canopy-Host-Label",
        HeaderValue::from_str(&headers.label).context("invalid host label header")?,
    );
    Ok(request)
}

/// Connect once, classifying an HTTP 401 apart from a transient failure.
pub async fn connect_ws(url: &str, headers: &HostHeaders) -> Result<WsStream, ConnectError> {
    let request = request_for(url, headers).map_err(|e| ConnectError::Transient(e.to_string()))?;
    match tokio_tungstenite::connect_async(request).await {
        Ok((stream, _response)) => Ok(stream),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) if response.status() == 401 => {
            Err(ConnectError::Unauthorized)
        }
        Err(error) => Err(ConnectError::Transient(error.to_string())),
    }
}

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

/// Everything one relay connection needs to answer frames.
pub struct RelayRuntime {
    host: Arc<HostKey>,
    pairing: PairingState,
    devices: RemoteDevices,
    connections: ConnectionRegistry,
    github_login: String,
    db: Database,
    sessions: Option<Arc<SessionsState>>,
    relay_url: String,
    bearer: String,
    peers: DevicePeers,
}

impl RelayRuntime {
    /// Build the runtime with the pairing state the CLI wrote. The live
    /// connections stay with the supervisor across reconnects, so every
    /// runtime of one daemon shares the one registry it is given.
    pub fn new(
        host: HostKey,
        db: Database,
        github_login: String,
        pairing: PairingState,
        connections: ConnectionRegistry,
    ) -> Self {
        Self {
            host: Arc::new(host),
            pairing,
            devices: RemoteDevices::new(db.clone()),
            connections,
            github_login,
            db,
            sessions: None,
            relay_url: String::new(),
            bearer: String::new(),
            peers: DevicePeers::default(),
        }
    }

    /// Route one body: `pair_init` answers inline, signalling bodies spawn or
    /// reach a live peer, anything else is `unknown_type`.
    pub fn route(&self, body: &Value, now: i64, from: &str, signal: &SignalTx) -> Option<Value> {
        match body.get("t").and_then(Value::as_str) {
            Some("pair_init") => Some(self.answer(body, now)),
            Some("offer") => {
                self.start_offer(body, now, from, signal);
                None
            }
            Some("ice") => {
                if let Some(candidate) = signaling::ice_candidate(body) {
                    // The contract's `ice{candidate}` carries no device id;
                    // the relay envelope's `from` is the connection.
                    self.peers.route(from, Control::Ice(candidate.to_string()));
                }
                None
            }
            Some("bye") => {
                // The contract's `bye` carries no fields: it closes the
                // sender's own peer. A body `device_id` is never honored —
                // otherwise any paired device could close another one.
                self.peers.close_from(from);
                None
            }
            _ => Some(json!({"t": "error", "reason": "unknown_type"})),
        }
    }

    /// Verify an offer, refuse it, or build the answer off the socket loop.
    fn start_offer(&self, body: &Value, now: i64, from: &str, signal: &SignalTx) {
        let host_id = self.host.host_id();
        let offer = match signaling::verify_offer(&self.devices, &host_id, body, now) {
            Ok(offer) => offer,
            Err(refusal) => {
                let _ = signal.send((from.to_string(), signaling::connect_fail(refusal)));
                return;
            }
        };
        let Some(sessions) = self.sessions.clone() else {
            let _ = signal.send((from.to_string(), signaling::connect_fail(Refusal::Busy)));
            return;
        };
        let device_id = offer.device.id.clone();
        let token = self.connections.register(&device_id);
        let (control_tx, control_rx) = tokio::sync::mpsc::unbounded_channel();
        if !self
            .peers
            .register(&device_id, from, Arc::clone(&token), control_tx)
        {
            self.connections.release(&device_id, &token);
            let _ = signal.send((from.to_string(), signaling::connect_fail(Refusal::Busy)));
            return;
        }
        let host = Arc::clone(&self.host);
        let devices = self.devices.clone();
        let peers = self.peers.clone();
        let connections = self.connections.clone();
        let relay_url = self.relay_url.clone();
        let bearer = self.bearer.clone();
        let signal = signal.clone();
        let from = from.to_string();
        tokio::spawn(async move {
            let servers = match peer::fetch_ice_servers(&relay_url, &bearer).await {
                Ok(servers) => servers,
                Err(error) => {
                    tracing::warn!("remote: no ICE servers from the relay: {error:#}");
                    Vec::new()
                }
            };
            let answered = peer::Peer::answer_offer(servers, &offer.sdp).await;
            let Ok((sdp, peer)) = answered else {
                peers.remove(&device_id, &token);
                connections.release(&device_id, &token);
                let _ = signal.send((from, signaling::connect_fail(Refusal::BadSignature)));
                return;
            };
            let answer = signaling::answer_body(&host, &device_id, &offer.sig_b64, &sdp);
            let _ = signal.send((from.clone(), answer));
            let _ = devices.touch(&device_id);
            connection::spawn(PeerSession {
                peer,
                device_id,
                state: sessions,
                peers,
                connections,
                token,
                control: control_rx,
                signal,
                from,
            });
        });
    }

    /// One protocol reply for one relay body.
    pub fn answer(&self, body: &Value, now: i64) -> Value {
        match body.get("t").and_then(Value::as_str) {
            Some("pair_init") => self
                .pairing
                .respond(body, &self.host, &self.devices, &self.github_login, now)
                .unwrap_or_else(|_| pair_fail("bad_code")),
            _ => json!({"t": "error", "reason": "unknown_type"}),
        }
    }

    /// Revoke a device: delete the row and close its live connection at once.
    #[allow(dead_code)]
    pub fn revoke(&self, device_id: &str) -> Result<bool> {
        let removed = self.devices.revoke(device_id)?;
        self.connections.close(device_id);
        self.peers.close(device_id);
        Ok(removed)
    }

    /// Live connections, for tests and status surfaces.
    #[cfg(test)]
    pub fn connections(&self) -> &ConnectionRegistry {
        &self.connections
    }

    /// Close every connection whose device was revoked by another process
    /// (the CLI writes ids to `remote_revoke`).
    pub fn drain_remote_revocations(&self) {
        let Ok(Some(value)) = self.db.get_state(REMOTE_REVOKE_KEY) else {
            return;
        };
        let drained: Vec<&str> = value.split(',').filter(|id| !id.is_empty()).collect();
        for id in &drained {
            self.connections.close(id);
        }
        // A revoke that lands while we close must survive: keep whatever we
        // did not just drain instead of blanking the key.
        let current = self
            .db
            .get_state(REMOTE_REVOKE_KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        let remaining: Vec<&str> = current
            .split(',')
            .filter(|id| !id.is_empty() && !drained.contains(id))
            .collect();
        let _ = self.db.set_state(REMOTE_REVOKE_KEY, &remaining.join(","));
    }
}

fn pair_fail(reason: &str) -> Value {
    json!({"t": "pair_fail", "reason": reason})
}

/// Serve one connected socket until it closes, fails, or `remote.enabled`
/// is switched off (`canopy remote off` takes effect on the next tick).
pub async fn serve_socket(
    mut socket: WsStream,
    runtime: Arc<RelayRuntime>,
    data_dir: std::path::PathBuf,
) -> Result<()> {
    use futures::StreamExt as _;
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            message = socket.next() => {
                let Some(message) = message else { return Ok(()) };
                let message = message.context("relay socket read failed")?;
                if !handle_message(&mut socket, &message, &runtime, &signal_tx).await? {
                    return Ok(());
                }
            }
            Some((to, body)) = signal_rx.recv() => {
                if !send_body(&mut socket, &to, &body).await? {
                    return Ok(());
                }
            }
            _ = ticks.tick() => {
                runtime.drain_remote_revocations();
                if !remote_enabled(&data_dir) {
                    return Ok(());
                }
            }
        }
    }
}

/// Re-read `[remote] enabled` from the config file.
fn remote_enabled(data_dir: &Path) -> bool {
    CanopyConfig::load(data_dir).remote.enabled
}

/// Handle one frame. `Ok(false)` means the socket should close.
async fn handle_message(
    socket: &mut WsStream,
    message: &Message,
    runtime: &Arc<RelayRuntime>,
    signal: &SignalTx,
) -> Result<bool> {
    match message {
        Message::Text(text) => answer_text(socket, text.as_str(), runtime, signal).await,
        Message::Ping(payload) => {
            socket.send(Message::Pong(payload.clone())).await?;
            Ok(true)
        }
        Message::Close(_) => Ok(false),
        _ => Ok(true),
    }
}

async fn answer_text(
    socket: &mut WsStream,
    text: &str,
    runtime: &Arc<RelayRuntime>,
    signal: &SignalTx,
) -> Result<bool> {
    let Ok(envelope) = serde_json::from_str::<Value>(text) else {
        return Ok(true);
    };
    let (Some(from), Some(body)) = (
        envelope.get("from").and_then(Value::as_str),
        envelope.get("body"),
    ) else {
        return Ok(true);
    };
    if let Some(reply) = runtime.route(body, chrono::Utc::now().timestamp(), from, signal) {
        if !send_body(socket, from, &reply).await? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Write one `{to, body}` envelope; `false` when the socket is gone.
async fn send_body(socket: &mut WsStream, to: &str, body: &Value) -> Result<bool> {
    let outbound = json!({"to": to, "body": body});
    Ok(socket
        .send(Message::Text(outbound.to_string().into()))
        .await
        .is_ok())
}

/// Record the terminal 401 message and stop retrying that token.
pub fn record_token_rejected(db: &Database) {
    let message = "github token rejected: run canopy remote login";
    let _ = db.set_state(REMOTE_ERROR_KEY, message);
    tracing::error!("{message}");
}

/// Append a revoked device id for the daemon to pick up.
pub fn queue_revocation(db: &Database, device_id: &str) {
    let existing = db.get_state(REMOTE_REVOKE_KEY).ok().flatten();
    let mut ids: Vec<&str> = existing
        .as_deref()
        .unwrap_or_default()
        .split(',')
        .filter(|id| !id.is_empty())
        .collect();
    if !ids.contains(&device_id) {
        ids.push(device_id);
    }
    let _ = db.set_state(REMOTE_REVOKE_KEY, &ids.join(","));
}

/// Spawn the relay supervisor for the life of the daemon. It re-reads
/// config and identity on every iteration, so `canopy remote on|off` takes
/// effect without a restart.
pub fn spawn(data_dir: std::path::PathBuf, db: Database, sessions: Arc<SessionsState>) {
    let live = crate::remote::live::shared();
    tokio::spawn(async move { supervise(data_dir, db, sessions, live).await });
}

/// What keeps the relay from connecting this second.
enum Wait {
    /// No GitHub identity stored: forget any rejected token.
    SignedOut,
    /// `[remote] enabled = false`.
    Disabled,
    /// This token already got a 401; never retry it.
    Rejected,
}

/// One supervisor iteration's decision.
enum Next {
    /// Not ready to connect; check again in a second.
    Wait(Wait),
    /// The host key could not be loaded: back off, then retry.
    HostKeyError(String),
    /// Connect with these headers and serve the socket.
    Connect {
        /// The WebSocket URL built from `relay_url`.
        ws_url: String,
        /// Identity and host headers for the handshake.
        headers: HostHeaders,
        /// Everything one connection needs to answer frames.
        runtime: Arc<RelayRuntime>,
        /// The token this connection authenticates with.
        token: String,
    },
}

/// The cheap pre-connect checks — sign-in state, `remote.enabled`, the
/// rejected token and the host key — as one synchronous decision.
fn next_action(
    data_dir: &Path,
    db: &Database,
    rejected_token: Option<&str>,
    connections: &ConnectionRegistry,
    sessions: &Arc<SessionsState>,
    peers: &DevicePeers,
) -> Next {
    let Some(identity) = auth::load_identity(data_dir) else {
        return Next::Wait(Wait::SignedOut);
    };
    let config = CanopyConfig::load(data_dir);
    if !config.remote.enabled {
        return Next::Wait(Wait::Disabled);
    }
    if rejected_token == Some(identity.token.as_str()) {
        return Next::Wait(Wait::Rejected);
    }
    let host = match HostKey::load_or_create(data_dir) {
        Ok(host) => host,
        Err(error) => return Next::HostKeyError(error.to_string()),
    };
    let headers = HostHeaders {
        token: identity.token.clone(),
        host_id: host.host_id(),
        label: host_label(&config),
    };
    let mut runtime = RelayRuntime::new(
        host,
        db.clone(),
        identity.login.clone(),
        PairingState::new(data_dir),
        connections.clone(),
    );
    runtime.github_login = identity.login.clone();
    runtime.sessions = Some(Arc::clone(sessions));
    runtime.relay_url = config.remote.relay_url.clone();
    runtime.bearer = identity.token.clone();
    runtime.peers = peers.clone();
    let runtime = Arc::new(runtime);
    Next::Connect {
        ws_url: websocket_url(&config.remote.relay_url),
        headers,
        runtime,
        token: identity.token,
    }
}

/// How one connection attempt ended, for the supervisor loop.
enum After {
    /// The socket served and closed: pause a second, then connect again.
    Served,
    /// HTTP 401: record it and never retry this token.
    Rejected(String),
    /// Any other failure: wait, then count an attempt.
    Retry(Duration),
}

/// Connect once and serve the socket until it closes, classifying the
/// outcome for the supervisor loop. The socket comes up inside this call,
/// so `live` records `Connected` the moment the dial succeeds — the status
/// surfaces must not read `connecting` for the whole life of a healthy
/// connection.
async fn connect_and_serve(
    ws_url: &str,
    headers: &HostHeaders,
    runtime: Arc<RelayRuntime>,
    token: String,
    attempt: u32,
    data_dir: &Path,
    live: &RemoteLive,
) -> After {
    match connect_ws(ws_url, headers).await {
        Ok(socket) => {
            live.set_phase(RelayPhase::Connected);
            if let Err(error) = serve_socket(socket, runtime, data_dir.to_path_buf()).await {
                tracing::warn!("remote: relay connection ended: {error}");
            }
            After::Served
        }
        Err(ConnectError::Unauthorized) => After::Rejected(token),
        Err(ConnectError::Transient(reason)) => {
            tracing::warn!("remote: relay connection failed: {reason}");
            After::Retry(backoff_delay(attempt))
        }
    }
}

/// Fold one "wait" decision into the loop counters.
fn apply_wait(wait: &Wait, rejected_token: &mut Option<String>, attempt: &mut u32) {
    match wait {
        Wait::SignedOut => {
            *rejected_token = None;
            *attempt = 0;
        }
        Wait::Disabled => *attempt = 0,
        Wait::Rejected => {}
    }
}

/// The phase one wait decision leaves behind: a rejected token stays
/// rejected, anything else is off.
fn wait_phase(wait: &Wait) -> RelayPhase {
    match wait {
        Wait::Rejected => RelayPhase::TokenRejected,
        Wait::SignedOut | Wait::Disabled => RelayPhase::Off,
    }
}

/// The phase one connection attempt leaves behind: a served socket was
/// connected, a 401 rejects the token, anything else keeps connecting.
fn after_phase(after: &After) -> RelayPhase {
    match after {
        After::Served => RelayPhase::Connected,
        After::Rejected(_) => RelayPhase::TokenRejected,
        After::Retry(_) => RelayPhase::Connecting,
    }
}

/// The supervisor loop. It re-reads config and identity on every
/// iteration, so `canopy remote on|off` takes effect without a restart;
/// the live-connection registry outlives any one socket, so a revoke
/// still closes connections that were registered before a reconnect.
/// Every iteration records its phase on `live`, and the peers it serves
/// are `live`'s, so `remote_status` reads the same state.
pub async fn supervise(
    data_dir: std::path::PathBuf,
    db: Database,
    sessions: Arc<SessionsState>,
    live: Arc<RemoteLive>,
) {
    let mut attempt = 0u32;
    let mut rejected_token: Option<String> = None;
    let connections = ConnectionRegistry::default();
    let peers = live.peers().clone();
    loop {
        match next_action(
            &data_dir,
            &db,
            rejected_token.as_deref(),
            &connections,
            &sessions,
            &peers,
        ) {
            Next::Wait(wait) => {
                apply_wait(&wait, &mut rejected_token, &mut attempt);
                live.set_phase(wait_phase(&wait));
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Next::HostKeyError(error) => {
                tracing::warn!("remote: could not load the host key: {error}");
                tokio::time::sleep(backoff_delay(attempt)).await;
                attempt += 1;
            }
            Next::Connect {
                ws_url,
                headers,
                runtime,
                token,
            } => {
                live.set_phase(RelayPhase::Connecting);
                let after =
                    connect_and_serve(&ws_url, &headers, runtime, token, attempt, &data_dir, &live)
                        .await;
                live.set_phase(after_phase(&after));
                match after {
                    After::Served => {
                        let _ = db.set_state(REMOTE_ERROR_KEY, "");
                        attempt = 0;
                        // Bound the reconnect rate: a relay that accepts and
                        // instantly drops must not spin this loop.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    After::Rejected(token) => {
                        record_token_rejected(&db);
                        rejected_token = Some(token);
                    }
                    After::Retry(delay) => {
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "relay_tests.rs"]
mod tests;

fn host_label(config: &CanopyConfig) -> String {
    if !config.remote.host_label.trim().is_empty() {
        return config.remote.host_label.trim().to_string();
    }
    hostname()
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|name| name.trim().to_string())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "canopy-host".to_string())
}
