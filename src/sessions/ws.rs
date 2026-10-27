//! `GET /sessions/ws`: the session protocol v1 WebSocket endpoint.
//!
//! The handshake requires `Authorization: Bearer <token>` (see [`super::auth`]);
//! a missing or wrong token gets HTTP 401 before the upgrade. Each connection
//! then speaks protocol v1: it is greeted with `hello`, may attach to several
//! sessions at once, and receives lifecycle events for the whole hub.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::FromRequestParts as _;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::db::Database;

use super::auth::{bearer_token, ensure_token, token_valid};
use super::protocol::{
    error_frame, exited_frame, hello_frame, ok_frame, out_frame, parse_client_frame,
    session_added_frame, session_removed_frame, sessions_frame, snapshot_frame, spawned_frame,
    ClientFrame, SessionItem, ERR_BAD_REQUEST, ERR_DELIVERY_FAILED, ERR_FORBIDDEN, ERR_INTERNAL,
    ERR_NOT_FOUND, ERR_SPAWN_FAILED,
};
use super::{
    HubEvent, HubSession, OutputReceiver, SessionHub, SessionKind, SessionStatus, SpawnRequest,
};

/// Shared state for the sessions endpoint: the hub plus the bearer token.
/// The token value is never logged.
pub struct SessionsState {
    pub hub: SessionHub,
    token: String,
}

impl SessionsState {
    /// Create the endpoint state for a daemon start: ensure the token file
    /// exists and build the hub. Fails when the token cannot be read or
    /// created — the daemon must never serve without auth.
    pub fn ensure(data_dir: &Path, db: Arc<Database>) -> anyhow::Result<Arc<Self>> {
        let token = ensure_token(data_dir)?;
        let hub = SessionHub::new(db, host_label());
        Ok(Arc::new(Self { hub, token }))
    }

    #[cfg(test)]
    pub fn for_test(hub: SessionHub, token: String) -> Arc<Self> {
        Arc::new(Self { hub, token })
    }
}

/// Machine hostname for `hello.host_label`.
fn host_label() -> String {
    #[cfg(unix)]
    if let Some(name) = unix_hostname() {
        return name;
    }
    "canopy".to_string()
}

#[cfg(unix)]
fn unix_hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) == 0 };
    if !ok {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = std::str::from_utf8(&buf[..len]).ok()?.trim().to_string();
    (!name.is_empty()).then_some(name)
}

/// The `/sessions/ws` route: 401 on a missing or wrong bearer token, before
/// the upgrade is ever attempted.
pub async fn sessions_ws_handler(
    State(state): State<Arc<SessionsState>>,
    req: Request<Body>,
) -> Response {
    if !request_authorized(req.headers(), &state.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let (mut parts, _body) = req.into_parts();
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    ws.on_upgrade(|socket| handle_connection(socket, state))
}

fn request_authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(bearer_token)
        .is_some_and(|provided| token_valid(provided, token))
}

/// Outbound JSON frames for one connection.
type OutTx = mpsc::UnboundedSender<String>;

/// Output forwarders of one connection, by attached session id.
type Attached = HashMap<String, JoinHandle<()>>;

async fn handle_connection(socket: WebSocket, state: Arc<SessionsState>) {
    let (mut sink, mut stream) = socket.split();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<String>();
    let send_task = tokio::spawn(async move {
        while let Some(text) = out_rx.recv().await {
            if sink.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    let _ = out_tx.send(hello_frame(state.hub.host_label()));
    let mut events = state.hub.subscribe_events();
    let mut attached: Attached = HashMap::new();
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Text(text)) => {
                        handle_client_text(&state, &out_tx, &mut attached, &text);
                    }
                    Ok(Message::Close(_)) => break,
                    Ok(_) => {
                        let _ = out_tx.send(error_frame(
                            None,
                            ERR_BAD_REQUEST,
                            "expected text frame",
                        ));
                    }
                    Err(_) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => handle_hub_event(&out_tx, &mut attached, event),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = out_tx.send(error_frame(
                            None,
                            ERR_INTERNAL,
                            "event buffer lagged; send list to resync",
                        ));
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    for task in attached.into_values() {
        task.abort();
    }
    send_task.abort();
}

fn handle_client_text(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    attached: &mut Attached,
    text: &str,
) {
    let frame = match parse_client_frame(text) {
        Ok(frame) => frame,
        Err(e) => {
            let _ = out_tx.send(error_frame(e.req.as_deref(), ERR_BAD_REQUEST, &e.message));
            return;
        }
    };
    match frame {
        ClientFrame::List { req } => handle_list(state, out_tx, &req),
        ClientFrame::Attach { id } => attach_session(state, out_tx, attached, id),
        ClientFrame::Detach { id } => detach_session(state, out_tx, attached, &id),
        ClientFrame::Input { id, data } => handle_input(state, out_tx, &id, &data),
        ClientFrame::Prompt {
            req,
            id,
            text: prompt,
        } => {
            handle_prompt(state, out_tx, req, &id, prompt);
        }
        ClientFrame::Resize { id, cols, rows } => handle_resize(state, out_tx, &id, cols, rows),
        ClientFrame::Spawn {
            req,
            kind,
            cli,
            model,
            workdir,
            name,
            prompt,
            ext,
        } => {
            let Some(kind) = SessionKind::parse(&kind) else {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_BAD_REQUEST,
                    "spawn: field 'kind' must be 'agent' or 'terminal'",
                ));
                return;
            };
            handle_spawn(
                state,
                out_tx,
                req,
                SpawnRequest {
                    kind,
                    cli,
                    model,
                    workdir,
                    name,
                    prompt,
                    interactive_args: ext.interactive_args,
                    fallback_args: ext.fallback_args,
                    model_flag: ext.model_flag,
                    id: ext.id,
                    seed_id: ext.seed_id,
                    accent: ext.accent,
                },
            );
        }
        ClientFrame::Kill { req, id } => handle_kill(state, out_tx, req, id),
    }
}

fn handle_list(state: &Arc<SessionsState>, out_tx: &OutTx, req: &str) {
    let mut items: Vec<SessionItem> = state.hub.list().iter().map(|s| s.item()).collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let _ = out_tx.send(sessions_frame(req, &items));
}

fn no_such_session(id: &str) -> String {
    format!("no such session '{id}'")
}

/// Attach: send one snapshot, then forward `out` frames after it. The
/// snapshot is queued before the forwarder starts so it always arrives
/// first; chunks in between wait in the broadcast receiver.
fn attach_session(state: &Arc<SessionsState>, out_tx: &OutTx, attached: &mut Attached, id: String) {
    let Some(session) = state.hub.get(&id) else {
        let _ = out_tx.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(&id)));
        return;
    };
    let Some((snapshot, rx)) = session.attach() else {
        let _ = out_tx.send(error_frame(None, ERR_INTERNAL, "session unavailable"));
        return;
    };
    if let Some(prev) = attached.remove(&id) {
        prev.abort();
    }
    let _ = out_tx.send(snapshot_frame(
        &id,
        snapshot.cols,
        snapshot.rows,
        snapshot.seq,
        &snapshot.data,
    ));
    let task = spawn_forwarder(session, snapshot.seq, rx, out_tx.clone());
    attached.insert(id, task);
}

fn detach_session(state: &Arc<SessionsState>, out_tx: &OutTx, attached: &mut Attached, id: &str) {
    if state.hub.get(id).is_none() {
        let _ = out_tx.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
        return;
    }
    if let Some(task) = attached.remove(id) {
        task.abort();
    }
}

fn handle_input(state: &Arc<SessionsState>, out_tx: &OutTx, id: &str, data: &[u8]) {
    let Some(session) = state.hub.get(id) else {
        let _ = out_tx.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
        return;
    };
    if session.status() != SessionStatus::Running {
        let _ = out_tx.send(error_frame(None, ERR_FORBIDDEN, "session has exited"));
        return;
    }
    if let Err(e) = session.write_to_pty(data) {
        let _ = out_tx.send(error_frame(None, ERR_DELIVERY_FAILED, &e.to_string()));
    }
}

fn handle_prompt(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    id: &str,
    prompt: String,
) {
    let Some(session) = state.hub.get(id) else {
        let _ = out_tx.send(error_frame(Some(&req), ERR_NOT_FOUND, &no_such_session(id)));
        return;
    };
    if session.status() != SessionStatus::Running {
        let _ = out_tx.send(error_frame(Some(&req), ERR_FORBIDDEN, "session has exited"));
        return;
    }
    // The paste path sleeps between paste and submit; keep it off the executor.
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let delivered = tokio::task::spawn_blocking(move || session.prompt(&prompt)).await;
        match delivered {
            Ok(Ok(())) => {
                let _ = out_tx.send(ok_frame(&req));
            }
            Ok(Err(e)) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_DELIVERY_FAILED, &e.to_string()));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "prompt task failed"));
            }
        }
    });
}

fn handle_resize(state: &Arc<SessionsState>, out_tx: &OutTx, id: &str, cols: u16, rows: u16) {
    let Some(session) = state.hub.get(id) else {
        let _ = out_tx.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
        return;
    };
    session.resize(cols, rows);
}

fn handle_spawn(state: &Arc<SessionsState>, out_tx: &OutTx, req: String, spawn: SpawnRequest) {
    if spawn.cli.is_empty() {
        let _ = out_tx.send(error_frame(
            Some(&req),
            ERR_BAD_REQUEST,
            "spawn: field 'cli' must not be empty",
        ));
        return;
    }
    if spawn.workdir.is_empty() {
        let _ = out_tx.send(error_frame(
            Some(&req),
            ERR_BAD_REQUEST,
            "spawn: field 'workdir' must not be empty",
        ));
        return;
    }
    let hub = state.hub.clone();
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let spawned = tokio::task::spawn_blocking(move || hub.spawn(&spawn)).await;
        match spawned {
            Ok(Ok(session)) => {
                let _ = out_tx.send(spawned_frame(&req, &session.id));
            }
            Ok(Err(e)) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_SPAWN_FAILED, &e.to_string()));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "spawn task failed"));
            }
        }
    });
}

fn handle_kill(state: &Arc<SessionsState>, out_tx: &OutTx, req: String, id: String) {
    // `kill` polls the child for its exit code; keep it off the executor.
    let hub = state.hub.clone();
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let kill_id = id.clone();
        let killed = tokio::task::spawn_blocking(move || hub.kill(&kill_id)).await;
        match killed {
            Ok(Some(_)) => {
                let _ = out_tx.send(ok_frame(&req));
            }
            Ok(None) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_NOT_FOUND,
                    &format!("no such session '{id}'"),
                ));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "kill task failed"));
            }
        }
    });
}

fn handle_hub_event(out_tx: &OutTx, attached: &mut Attached, event: HubEvent) {
    match event {
        HubEvent::SessionAdded(item) => {
            let _ = out_tx.send(session_added_frame(&item));
        }
        HubEvent::SessionRemoved(id) => {
            if let Some(task) = attached.remove(&id) {
                task.abort();
            }
            let _ = out_tx.send(session_removed_frame(&id));
        }
        HubEvent::Exited { id, code, to_all } => {
            if to_all || attached.contains_key(&id) {
                let _ = out_tx.send(exited_frame(&id, code));
            }
        }
    }
}

/// Forward one attached session's output: `out` frames after `from_seq`, or
/// a fresh snapshot when the subscriber lagged, then continue.
fn spawn_forwarder(
    session: Arc<HubSession>,
    from_seq: u64,
    rx: OutputReceiver,
    out_tx: OutTx,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = rx;
        let mut next = from_seq;
        loop {
            let outcome = rx.recv().await;
            if !forward_recv(&session, &mut next, outcome, &out_tx) {
                break;
            }
        }
    })
}

/// Forward one broadcast outcome to the connection; false stops the forwarder.
fn forward_recv(
    session: &HubSession,
    next: &mut u64,
    outcome: Result<(u64, Vec<u8>), broadcast::error::RecvError>,
    out_tx: &OutTx,
) -> bool {
    match outcome {
        Ok((seq, chunk)) => {
            if seq <= *next {
                return true;
            }
            *next = seq;
            out_tx.send(out_frame(&session.id, seq, &chunk)).is_ok()
        }
        Err(broadcast::error::RecvError::Lagged(_)) => {
            let Some(snapshot) = session.fresh_snapshot() else {
                return false;
            };
            *next = snapshot.seq;
            out_tx
                .send(snapshot_frame(
                    &session.id,
                    snapshot.cols,
                    snapshot.rows,
                    snapshot.seq,
                    &snapshot.data,
                ))
                .is_ok()
        }
        Err(broadcast::error::RecvError::Closed) => false,
    }
}
