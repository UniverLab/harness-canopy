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
use crate::remote::devices::RemoteDevices;
use crate::remote::hostkey::HostKey;
use crate::remote::pairing::PairingState;

/// `daemon_state` key carrying the last terminal relay error.
pub const REMOTE_ERROR_KEY: &str = "remote_error";
/// `daemon_state` key holding device ids revoked by another process.
pub const REMOTE_REVOKE_KEY: &str = "remote_revoke";
/// Longest reconnect delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(60);

/// A connected relay socket.
pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

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
    inner: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl ConnectionRegistry {
    /// Register a live connection for `device_id`; cancelling the returned
    /// token closes it. Used by the connection layer the next spec adds.
    #[allow(dead_code)]
    pub fn register(&self, device_id: &str) -> CancellationToken {
        let token = CancellationToken::new();
        if let Ok(mut map) = self.inner.lock() {
            map.insert(device_id.to_string(), token.clone());
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
}

/// Everything one relay connection needs to answer frames.
pub struct RelayRuntime {
    host: HostKey,
    pairing: PairingState,
    devices: RemoteDevices,
    connections: ConnectionRegistry,
    github_login: String,
    db: Database,
}

impl RelayRuntime {
    /// Build the runtime with the pairing state the CLI wrote.
    pub fn new(host: HostKey, db: Database, github_login: String, pairing: PairingState) -> Self {
        Self {
            host,
            pairing,
            devices: RemoteDevices::new(db.clone()),
            connections: ConnectionRegistry::default(),
            github_login,
            db,
        }
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
        for id in value.split(',').filter(|id| !id.is_empty()) {
            self.connections.close(id);
        }
        let _ = self.db.set_state(REMOTE_REVOKE_KEY, "");
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
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            message = socket.next() => {
                let Some(message) = message else { return Ok(()) };
                let message = message.context("relay socket read failed")?;
                if !handle_message(&mut socket, &message, &runtime).await? {
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
) -> Result<bool> {
    match message {
        Message::Text(text) => answer_text(socket, text.as_str(), runtime).await,
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
    let reply = runtime.answer(body, chrono::Utc::now().timestamp());
    let outbound = json!({"to": from, "body": reply});
    socket
        .send(Message::Text(outbound.to_string().into()))
        .await?;
    Ok(true)
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
pub fn spawn(data_dir: std::path::PathBuf, db: Database) {
    tokio::spawn(async move { supervise(data_dir, db).await });
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
fn next_action(data_dir: &Path, db: &Database, rejected_token: Option<&str>) -> Next {
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
    let runtime = Arc::new(RelayRuntime::new(
        host,
        db.clone(),
        identity.login.clone(),
        PairingState::new(data_dir),
    ));
    Next::Connect {
        ws_url: websocket_url(&config.remote.relay_url),
        headers,
        runtime,
        token: identity.token,
    }
}

/// How one connection attempt ended, for the supervisor loop.
enum After {
    /// The socket served and closed: connect again immediately.
    Served,
    /// HTTP 401: record it and never retry this token.
    Rejected(String),
    /// Any other failure: wait, then count an attempt.
    Retry(Duration),
}

/// Connect once and serve the socket until it closes, classifying the
/// outcome for the supervisor loop.
async fn connect_and_serve(
    ws_url: &str,
    headers: &HostHeaders,
    runtime: Arc<RelayRuntime>,
    token: String,
    attempt: u32,
    data_dir: &Path,
) -> After {
    match connect_ws(ws_url, headers).await {
        Ok(socket) => {
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

/// The supervisor loop, exposed so tests can drive one iteration. It
/// re-reads config and identity on every iteration, so `canopy remote
/// on|off` takes effect without a restart.
pub async fn supervise(data_dir: std::path::PathBuf, db: Database) {
    let mut attempt = 0u32;
    let mut rejected_token: Option<String> = None;
    loop {
        match next_action(&data_dir, &db, rejected_token.as_deref()) {
            Next::Wait(wait) => {
                apply_wait(&wait, &mut rejected_token, &mut attempt);
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
                let after =
                    connect_and_serve(&ws_url, &headers, runtime, token, attempt, &data_dir).await;
                match after {
                    After::Served => {
                        let _ = db.set_state(REMOTE_ERROR_KEY, "");
                        attempt = 0;
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
