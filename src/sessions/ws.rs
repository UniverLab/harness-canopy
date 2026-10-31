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
use super::catalog::{handle_catalog, handle_compose, handle_prompt_draft};
use super::graphs::{handle_graph_unwatch, handle_graph_watch, handle_graphs};
use super::protocol::{
    error_frame, exited_frame, file_frame, hello_frame, ok_frame, out_frame, parse_client_frame,
    session_added_frame, session_removed_frame, sessions_frame, shelf_added_frame, shelf_frame,
    shelf_removed_frame, snapshot_frame, spawned_frame, ClientFrame, SessionItem, ERR_BAD_REQUEST,
    ERR_DELIVERY_FAILED, ERR_FORBIDDEN, ERR_INTERNAL, ERR_NOT_FOUND, ERR_SPAWN_FAILED,
};
use super::shelf::{ShelfStore, READ_CHUNK_BYTES};
use super::{
    default_terminal_shell, resolve_spawn_workdir, yolo_flag_for, HubEvent, HubSession,
    OutputReceiver, SessionHub, SessionKind, SessionStatus, SpawnRequest,
};

/// Shared state for the sessions endpoint: the hub, the shelf it broadcasts
/// on, and the bearer token. The token value is never logged.
pub struct SessionsState {
    pub hub: SessionHub,
    pub shelf: std::sync::Arc<ShelfStore>,
    token: String,
    /// Live `graph_watch` poll tasks across this state's connections:
    /// bumped when a watch registers and dropped with its task (abort on
    /// `graph_unwatch` or socket close, or the watch's own exit), so a test
    /// can show a closed socket ended every watch it held.
    live_graph_watches: std::sync::atomic::AtomicUsize,
}

impl SessionsState {
    /// Create the endpoint state for a daemon start: ensure the token file
    /// exists, build the hub, wire the shelf to it (so insert/delete broadcast
    /// `shelf_added`/`shelf_removed` here and `kill` reaps), create the shelf
    /// directory and run the startup lifecycle checks (retention, cap,
    /// orphans — FR1/FR4). Fails when the token cannot be read or created —
    /// the daemon must never serve without auth.
    pub fn ensure(data_dir: &Path, db: Arc<Database>) -> anyhow::Result<Arc<Self>> {
        let token = ensure_token(data_dir)?;
        let hub = SessionHub::new(db.clone(), host_label(data_dir));
        let shelf = ShelfStore::new(db, data_dir.join("shelf"))?;
        hub.wire_shelf(shelf.clone());
        shelf.maintenance();
        Ok(Arc::new(Self {
            hub,
            shelf,
            token,
            live_graph_watches: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    #[cfg(test)]
    pub fn for_test(hub: SessionHub, token: String) -> Arc<Self> {
        let shelf = ShelfStore::for_test(hub.db());
        hub.wire_shelf(shelf.clone());
        shelf.maintenance();
        Arc::new(Self {
            hub,
            shelf,
            token,
            live_graph_watches: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Count one `graph_watch` starting; paired with
    /// [`Self::graph_watch_ended`] in the watch task's drop guard.
    pub(crate) fn graph_watch_started(&self) {
        self.live_graph_watches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// Count one `graph_watch` ending, whatever ended it.
    pub(crate) fn graph_watch_ended(&self) {
        self.live_graph_watches
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }

    /// How many `graph_watch` poll tasks are still alive on this state —
    /// test-only, to assert that a closed connection ends its watches.
    #[cfg(test)]
    pub(crate) fn live_graph_watches(&self) -> usize {
        self.live_graph_watches
            .load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Machine hostname for `hello.host_label`.
/// The label `hello` greets with: `[remote] host_label` when set (the same
/// label the relay lists the host under), otherwise the hostname.
fn host_label(data_dir: &Path) -> String {
    let configured = crate::domain::canopy_config::CanopyConfig::load(data_dir)
        .remote
        .host_label;
    if !configured.trim().is_empty() {
        return configured.trim().to_string();
    }
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
pub(crate) type OutTx = mpsc::UnboundedSender<String>;

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
    // Per-connection `graph_watch` tasks: dropped (aborted) with the
    // connection, so no watch outlives its socket.
    let mut watches: HashMap<String, JoinHandle<()>> = HashMap::new();
    loop {
        tokio::select! {
            msg = stream.next() => {
                let Some(msg) = msg else { break };
                match msg {
                    Ok(Message::Text(text)) => {
                        handle_client_text(&state, &out_tx, &mut attached, &mut watches, &text).await;
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
    for task in watches.into_values() {
        task.abort();
    }
    send_task.abort();
}

async fn handle_client_text(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    attached: &mut Attached,
    watches: &mut HashMap<String, JoinHandle<()>>,
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
        ClientFrame::PromptDraft { req, id, draft } => {
            handle_prompt_draft(state, out_tx, req, &id, draft);
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
                    harness_conversation_id: ext.harness_conversation_id,
                    seed_id: ext.seed_id,
                    accent: ext.accent,
                    yolo: ext.yolo,
                },
            );
        }
        ClientFrame::Kill { req, id } => handle_kill(state, out_tx, req, id),
        ClientFrame::ShelfList { req, id } => handle_shelf_list(state, out_tx, req, id),
        ClientFrame::ShelfGet { req, file_id } => handle_shelf_get(state, out_tx, req, file_id),
        ClientFrame::ShelfDelete { req, file_id } => {
            handle_shelf_delete(state, out_tx, req, file_id);
        }
        ClientFrame::Compose { req, draft } => {
            handle_compose(state, out_tx, req, draft);
        }
        ClientFrame::Catalog { req } => {
            handle_catalog(state, out_tx, req);
        }
        ClientFrame::Graphs { req } => {
            handle_graphs(state, out_tx, req);
        }
        ClientFrame::GraphWatch { req, id } => {
            handle_graph_watch(state, out_tx, watches, req, id).await;
        }
        ClientFrame::GraphUnwatch { id } => {
            handle_graph_unwatch(watches, &id);
        }
        ClientFrame::HostMetrics { req } => reject_host_metrics(out_tx, Some(&req)),
        ClientFrame::HostMetricsWatch { .. } | ClientFrame::HostMetricsUnwatch => {
            reject_host_metrics(out_tx, None);
        }
    }
}

/// Host metrics are remote-only: the local socket refuses them with
/// `error{code:"forbidden"}`, echoing `req` when the frame carried one.
fn reject_host_metrics(out_tx: &OutTx, req: Option<&str>) {
    let _ = out_tx.send(error_frame(
        req,
        ERR_FORBIDDEN,
        "host metrics are remote-only",
    ));
}

/// `shelf_list`: one shelf's items (host shelf when `id` is absent), newest
/// first. The query runs off the executor like every other blocking hub call.
pub(crate) fn handle_shelf_list(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    id: Option<String>,
) {
    let shelf = state.shelf.clone();
    let out_tx = out_tx.clone();
    let echo = id.clone();
    tokio::spawn(async move {
        let listed = tokio::task::spawn_blocking(move || shelf.list(id.as_deref())).await;
        match listed {
            Ok(items) => {
                let _ = out_tx.send(shelf_frame(&req, echo.as_deref(), &items));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "shelf list failed"));
            }
        }
    });
}

/// `shelf_get`: stream one file back as `file` parts of at most 12 KiB raw.
/// The id is matched against rows first, so nothing outside the shelf
/// directory is ever read (FR: `shelf_get` takes a `file_id` only).
pub(crate) fn handle_shelf_get(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    file_id: String,
) {
    let shelf = state.shelf.clone();
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let read = tokio::task::spawn_blocking({
            let file_id = file_id.clone();
            move || shelf.read_file(&file_id)
        })
        .await;
        match read {
            Ok(Some(bytes)) => {
                if bytes.is_empty() {
                    let _ = out_tx.send(file_frame(&req, &file_id, 0, &[], true));
                    return;
                }
                let total = bytes.len().div_ceil(READ_CHUNK_BYTES);
                for (index, chunk) in bytes.chunks(READ_CHUNK_BYTES).enumerate() {
                    let last = index + 1 == total;
                    let _ = out_tx.send(file_frame(&req, &file_id, index as u32, chunk, last));
                }
            }
            Ok(None) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_NOT_FOUND,
                    &format!("no shelf file '{file_id}'"),
                ));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "shelf read failed"));
            }
        }
    });
}

/// `shelf_delete`: remove one file; the store broadcasts `shelf_removed` to
/// every connection (including the requester) when it did.
pub(crate) fn handle_shelf_delete(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    file_id: String,
) {
    let shelf = state.shelf.clone();
    let out_tx = out_tx.clone();
    let id_for_error = file_id.clone();
    tokio::spawn(async move {
        let deleted = tokio::task::spawn_blocking(move || shelf.delete(&file_id)).await;
        match deleted {
            Ok(true) => {
                let _ = out_tx.send(ok_frame(&req));
            }
            Ok(false) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_NOT_FOUND,
                    &format!("no shelf file '{id_for_error}'"),
                ));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "shelf delete failed"));
            }
        }
    });
}

fn handle_list(state: &Arc<SessionsState>, out_tx: &OutTx, req: &str) {
    let mut items: Vec<SessionItem> = state.hub.list().iter().map(|s| s.item()).collect();
    items.sort_by(|a, b| a.id.cmp(&b.id));
    let _ = out_tx.send(sessions_frame(req, &items));
}

pub(crate) fn no_such_session(id: &str) -> String {
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

fn handle_spawn(state: &Arc<SessionsState>, out_tx: &OutTx, req: String, mut spawn: SpawnRequest) {
    if spawn.kind == SessionKind::Agent && spawn.cli.is_empty() {
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
    if let Err(message) = validate_spawn_extensions(&mut spawn) {
        let _ = out_tx.send(error_frame(Some(&req), ERR_BAD_REQUEST, &message));
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

/// Validate a spawn's extension fields before the hub opens a PTY: `yolo`
/// needs an agent whose harness defines a `yolo_flag`; a terminal without
/// `cli` opens the default shell; and `workdir` must expand (`~`, `~/x`) to
/// an absolute path naming a real directory — the expanded path replaces the
/// frame's own. `Err` carries the `bad_request` message.
pub(crate) fn validate_spawn_extensions(spawn: &mut SpawnRequest) -> Result<(), String> {
    if spawn.yolo {
        if spawn.kind != SessionKind::Agent {
            return Err(format!(
                "spawn: field 'yolo' is not allowed for kind '{}'",
                spawn.kind.as_str()
            ));
        }
        if yolo_flag_for(&spawn.cli).is_none() {
            return Err(format!("spawn: harness '{}' has no yolo mode", spawn.cli));
        }
    }
    if spawn.kind == SessionKind::Terminal && spawn.cli.is_empty() {
        spawn.cli = default_terminal_shell();
    }
    spawn.workdir = resolve_spawn_workdir(&spawn.workdir)?;
    Ok(())
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
        HubEvent::ShelfAdded(item) => {
            let _ = out_tx.send(shelf_added_frame(&item));
        }
        HubEvent::ShelfRemoved { file_id } => {
            let _ = out_tx.send(shelf_removed_frame(&file_id));
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
