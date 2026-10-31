//! Serving one connected relay socket.
//!
//! Frames are JSON envelopes; the [`super::relay::RelayRuntime`] answers
//! them. A half-open socket reads nothing forever, so the loop Pings every
//! second and declares the socket dead past the idle timeout, letting the
//! supervisor reconnect it without the relay toggle dance.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use futures::SinkExt as _;
use serde_json::{json, Value};
use tokio_tungstenite::tungstenite::Message;

use super::relay::{RelayRuntime, SignalTx, WsStream};
use crate::domain::canopy_config::CanopyConfig;

/// Seconds without any inbound frame before the socket is declared dead and
/// the supervisor reconnects it.
const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// Serve one connected socket until it closes, fails, or `remote.enabled`
/// is switched off (`canopy remote off` takes effect on the next tick).
pub async fn serve_socket(
    socket: WsStream,
    runtime: Arc<RelayRuntime>,
    data_dir: std::path::PathBuf,
) -> Result<()> {
    serve_socket_with_idle_timeout(socket, runtime, data_dir, RELAY_IDLE_TIMEOUT).await
}

/// Serve one socket with an explicit idle timeout: production passes
/// [`RELAY_IDLE_TIMEOUT`], the reconnect tests shorten it so a silent relay
/// is noticed in seconds instead of a minute.
pub async fn serve_socket_with_idle_timeout(
    mut socket: WsStream,
    runtime: Arc<RelayRuntime>,
    data_dir: std::path::PathBuf,
    idle_timeout: Duration,
) -> Result<()> {
    use futures::StreamExt as _;
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::unbounded_channel::<(String, Value)>();
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    // A half-open socket reads nothing forever; the Ping below plus this
    // clock notice it without the relay toggle dance.
    let mut last_inbound = Instant::now();
    loop {
        tokio::select! {
            message = socket.next() => {
                let Some(message) = message else { return Ok(()) };
                let message = message.context("relay socket read failed")?;
                last_inbound = Instant::now();
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
                if last_inbound.elapsed() > idle_timeout {
                    bail!("relay socket idle past its timeout");
                }
                // End to end through proxies, where a TCP keepalive would
                // only prove the next hop is alive.
                socket
                    .send(Message::Ping(Vec::new().into()))
                    .await
                    .context("relay ping failed")?;
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
        // The relay's answer to the tick Ping, like any other unexpected
        // frame: the serve loop already counted it as inbound, so a healthy
        // link never idles out.
        Message::Pong(_) | Message::Binary(_) | Message::Frame(_) => Ok(true),
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
