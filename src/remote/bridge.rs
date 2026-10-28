//! The session hub as a paired device sees it.
//!
//! A device peer is a remote client, not a local one: it speaks the same
//! protocol v1 frames as `/sessions/ws`, with four extra rules —
//!
//! * sessions of kind `terminal` are hidden from `sessions` and
//!   `session_added`;
//! * `attach`/`input`/`prompt`/`kill` aimed at a terminal answer
//!   `error{code:"forbidden"}`;
//! * `resize` always answers `forbidden`;
//! * `spawn` accepts only `kind:"agent"`.
//!
//! Outbound frames are size-bounded for a data channel: raw `out`,
//! `snapshot` and `file` payloads are split into chunks that keep the source
//! `seq`, the snapshot's first part staying in the `snapshot` frame (with
//! `cols`/`rows`) and the rest travelling as `snapshot_more{id,data,last}`.
//! Shelf transfers interleave with `out` frames, at most two per device — a
//! third `shelf_get` answers `error{code:"busy"}`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::{broadcast, Semaphore};
use tokio::task::JoinHandle;

use crate::sessions::catalog::{handle_catalog, handle_compose, handle_prompt_draft};
use crate::sessions::protocol::{
    error_frame, exited_frame, file_frame, ok_frame, out_frame, parse_client_frame,
    session_added_frame, session_removed_frame, sessions_frame, shelf_added_frame, shelf_frame,
    shelf_removed_frame, snapshot_frame, snapshot_more_frame, ClientFrame, SessionItem,
    ERR_BAD_REQUEST, ERR_BUSY, ERR_DELIVERY_FAILED, ERR_FORBIDDEN, ERR_INTERNAL, ERR_NOT_FOUND,
    ERR_SPAWN_FAILED,
};
use crate::sessions::ws::no_such_session;
use crate::sessions::ws::{handle_shelf_delete, OutTx, SessionsState};
use crate::sessions::{
    HubEvent, HubSession, OutputReceiver, SessionHub, SessionKind, SessionStatus, SpawnRequest,
};

/// Hard limit for one outbound JSON frame on a data channel.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Raw bytes per chunk: 12 KiB minus headroom, so that base64 plus the
/// envelope stays under [`MAX_FRAME_BYTES`] (12 KiB raw encodes to 16 KiB).
pub const CHUNK_BYTES: usize = MAX_FRAME_BYTES * 3 / 4 - 256;

/// Attached output forwarders of one device peer, by session id.
type Attached = HashMap<String, JoinHandle<()>>;

/// One device peer's view of the hub.
pub struct RemoteBridge {
    state: Arc<SessionsState>,
    attached: Attached,
    /// At most two `shelf_get` transfers in flight per device.
    transfers: Arc<Semaphore>,
    /// Running shelf transfers, aborted when the peer goes away.
    shelf_tasks: Vec<JoinHandle<()>>,
    /// Terminal-session files hidden from this peer, so their later
    /// `shelf_removed` is hidden too.
    hidden_files: HashSet<String>,
}

impl RemoteBridge {
    /// Bridge the hub behind `state` for one device peer.
    pub fn new(state: Arc<SessionsState>) -> Self {
        Self {
            state,
            attached: HashMap::new(),
            transfers: Arc::new(Semaphore::new(2)),
            shelf_tasks: Vec::new(),
            hidden_files: HashSet::new(),
        }
    }

    /// Answer one inbound client frame. Sync frames answer inline; `prompt`
    /// and `kill` answer from a blocking task, exactly like the local socket.
    pub fn handle_text(&mut self, text: &str, out: &OutTx) {
        let frame = match parse_client_frame(text) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = out.send(error_frame(
                    error.req.as_deref(),
                    ERR_BAD_REQUEST,
                    &error.message,
                ));
                return;
            }
        };
        match frame {
            ClientFrame::List { req } => self.handle_list(&req, out),
            ClientFrame::Attach { id } => self.attach(&id, out),
            ClientFrame::Detach { id } => self.detach(&id, out),
            ClientFrame::Input { id, data } => self.handle_input(&id, &data, out),
            ClientFrame::Prompt { req, id, text } => {
                self.guard_terminal(&req, &id, out, |bridge, out| {
                    let Some(session) = bridge.state.hub.get(&id) else {
                        let _ = out.send(error_frame(
                            Some(&req),
                            ERR_NOT_FOUND,
                            &no_such_session(&id),
                        ));
                        return;
                    };
                    if session.status() != SessionStatus::Running {
                        let _ =
                            out.send(error_frame(Some(&req), ERR_FORBIDDEN, "session has exited"));
                        return;
                    }
                    deliver_prompt(session, req.clone(), text, out.clone());
                });
            }
            ClientFrame::PromptDraft { req, id, draft } => {
                if self.terminal(&id) {
                    let _ = out.send(error_frame(
                        Some(&req),
                        ERR_FORBIDDEN,
                        "terminal sessions are not shared",
                    ));
                    return;
                }
                handle_prompt_draft(&self.state, out, req, &id, draft);
            }
            ClientFrame::Resize { id, .. } => {
                let _ = out.send(error_frame(
                    None,
                    ERR_FORBIDDEN,
                    &format!("resize is not available to remote devices ('{id}')"),
                ));
            }
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
                let request = SpawnRequest {
                    kind: SessionKind::Agent,
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
                };
                self.handle_spawn(req, &kind, request, out);
            }
            ClientFrame::Kill { req, id } => {
                self.guard_terminal(&req, &id, out, |bridge, out| {
                    deliver_kill(
                        bridge.state.hub.clone(),
                        req.clone(),
                        id.clone(),
                        out.clone(),
                    );
                });
            }
            ClientFrame::ShelfList { req, id } => {
                self.handle_shelf_list(req, id, out);
            }
            ClientFrame::ShelfGet { req, file_id } => {
                self.handle_shelf_get(req, file_id, out);
            }
            ClientFrame::ShelfDelete { req, file_id } => {
                handle_shelf_delete(&self.state, out, req, file_id);
            }
            ClientFrame::Compose { req, draft } => handle_compose(&self.state, out, req, draft),
            ClientFrame::Catalog { req } => handle_catalog(&self.state, out, req),
        }
    }

    /// Forward one hub lifecycle event, with terminal sessions filtered out.
    pub fn handle_event(&mut self, event: HubEvent, out: &OutTx) {
        match event {
            HubEvent::SessionAdded(item) => {
                if item.kind != SessionKind::Terminal.as_str() {
                    send(out, session_added_frame(&item));
                }
            }
            HubEvent::SessionRemoved(id) => {
                if let Some(task) = self.attached.remove(&id) {
                    task.abort();
                }
                send(out, session_removed_frame(&id));
            }
            HubEvent::Exited { id, code, to_all } => {
                if to_all || self.attached.contains_key(&id) {
                    send(out, exited_frame(&id, code));
                }
            }
            HubEvent::ShelfAdded(item) => {
                if item
                    .session_id
                    .as_deref()
                    .is_some_and(|id| self.terminal(id))
                {
                    self.hidden_files.insert(item.file_id.clone());
                } else {
                    send(out, shelf_added_frame(&item));
                }
            }
            HubEvent::ShelfRemoved { file_id } => {
                if self.hidden_files.remove(&file_id) {
                    // Hidden on arrival: the removal stays hidden too.
                } else {
                    send(out, shelf_removed_frame(&file_id));
                }
            }
        }
    }

    /// Abort every output forwarder and shelf transfer (the data channel
    /// closed or the device was revoked).
    pub fn abort(&mut self) {
        for task in self.attached.drain() {
            task.1.abort();
        }
        for task in self.shelf_tasks.drain(..) {
            task.abort();
        }
    }

    fn terminal(&self, id: &str) -> bool {
        self.state
            .hub
            .get(id)
            .is_some_and(|session| session.kind == SessionKind::Terminal)
    }

    /// Run `allowed` unless `id` names a terminal session.
    fn guard_terminal(
        &mut self,
        req: &str,
        id: &str,
        out: &OutTx,
        allowed: impl FnOnce(&mut Self, &OutTx),
    ) {
        if self.terminal(id) {
            let _ = out.send(error_frame(
                Some(req),
                ERR_FORBIDDEN,
                "terminal sessions are not shared with remote devices",
            ));
            return;
        }
        allowed(self, out);
    }

    fn handle_list(&self, req: &str, out: &OutTx) {
        let mut items: Vec<SessionItem> = self
            .state
            .hub
            .list()
            .iter()
            .filter(|session| session.kind != SessionKind::Terminal)
            .map(|session| session.item())
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        send(out, sessions_frame(req, &items));
    }

    fn attach(&mut self, id: &str, out: &OutTx) {
        if self.terminal(id) {
            let _ = out.send(error_frame(
                None,
                ERR_FORBIDDEN,
                "terminal sessions are not shared with remote devices",
            ));
            return;
        }
        let Some(session) = self.state.hub.get(id) else {
            let _ = out.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
            return;
        };
        let Some((snapshot, rx)) = session.attach() else {
            let _ = out.send(error_frame(None, ERR_INTERNAL, "session unavailable"));
            return;
        };
        if let Some(previous) = self.attached.remove(id) {
            previous.abort();
        }
        for frame in split_snapshot_frames(
            id,
            snapshot.cols,
            snapshot.rows,
            snapshot.seq,
            &snapshot.data,
        ) {
            send(out, frame);
        }
        let task = spawn_forwarder(session, snapshot.seq, rx, out.clone());
        self.attached.insert(id.to_string(), task);
    }

    fn detach(&mut self, id: &str, out: &OutTx) {
        if self.state.hub.get(id).is_none() {
            let _ = out.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
            return;
        }
        if let Some(task) = self.attached.remove(id) {
            task.abort();
        }
    }

    fn handle_input(&mut self, id: &str, data: &[u8], out: &OutTx) {
        if self.terminal(id) {
            let _ = out.send(error_frame(
                None,
                ERR_FORBIDDEN,
                "terminal sessions are not shared with remote devices",
            ));
            return;
        }
        let Some(session) = self.state.hub.get(id) else {
            let _ = out.send(error_frame(None, ERR_NOT_FOUND, &no_such_session(id)));
            return;
        };
        if session.status() != SessionStatus::Running {
            let _ = out.send(error_frame(None, ERR_FORBIDDEN, "session has exited"));
            return;
        }
        if let Err(error) = session.write_to_pty(data) {
            let _ = out.send(error_frame(None, ERR_DELIVERY_FAILED, &error.to_string()));
        }
    }

    fn handle_spawn(&self, req: String, kind: &str, mut request: SpawnRequest, out: &OutTx) {
        let Some(parsed) = SessionKind::parse(kind) else {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_BAD_REQUEST,
                "spawn: field 'kind' must be 'agent' or 'terminal'",
            ));
            return;
        };
        if parsed != SessionKind::Agent {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_FORBIDDEN,
                "remote devices may only spawn kind 'agent'",
            ));
            return;
        }
        if request.cli.is_empty() || request.workdir.is_empty() {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_BAD_REQUEST,
                "spawn: fields 'cli' and 'workdir' must not be empty",
            ));
            return;
        }
        request.kind = SessionKind::Agent;
        let hub = self.state.hub.clone();
        let out = out.clone();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || hub.spawn(&request)).await {
                Ok(Ok(session)) => send(
                    &out,
                    crate::sessions::protocol::spawned_frame(&req, &session.id),
                ),
                Ok(Err(error)) => send(
                    &out,
                    error_frame(Some(&req), ERR_SPAWN_FAILED, &error.to_string()),
                ),
                Err(_) => send(
                    &out,
                    error_frame(Some(&req), ERR_INTERNAL, "spawn task failed"),
                ),
            }
        });
    }

    fn handle_shelf_list(&self, req: String, id: Option<String>, out: &OutTx) {
        if id.as_deref().is_some_and(|id| self.terminal(id)) {
            send(out, shelf_frame(&req, id.as_deref(), &[]));
            return;
        }
        let shelf = self.state.shelf.clone();
        let out = out.clone();
        let echo = id.clone();
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || shelf.list(id.as_deref())).await {
                Ok(items) => send(&out, shelf_frame(&req, echo.as_deref(), &items)),
                Err(_) => send(
                    &out,
                    error_frame(Some(&req), ERR_INTERNAL, "shelf list failed"),
                ),
            }
        });
    }

    fn handle_shelf_get(&mut self, req: String, file_id: String, out: &OutTx) {
        let Some(row) = self.state.shelf.get(&file_id) else {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_NOT_FOUND,
                &format!("no shelf file '{file_id}'"),
            ));
            return;
        };
        if row
            .session_id
            .as_deref()
            .is_some_and(|id| self.terminal(id))
        {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_FORBIDDEN,
                "terminal sessions are not shared with remote devices",
            ));
            return;
        }
        let Ok(permit) = self.transfers.clone().try_acquire_owned() else {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_BUSY,
                "too many downloads in flight",
            ));
            return;
        };
        let shelf = self.state.shelf.clone();
        let out = out.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            let read = tokio::task::spawn_blocking({
                let file_id = file_id.clone();
                move || shelf.read_file(&file_id)
            })
            .await;
            let bytes = match read {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    send(
                        &out,
                        error_frame(
                            Some(&req),
                            ERR_NOT_FOUND,
                            &format!("no shelf file '{file_id}'"),
                        ),
                    );
                    return;
                }
                Err(_) => {
                    send(
                        &out,
                        error_frame(Some(&req), ERR_INTERNAL, "shelf read failed"),
                    );
                    return;
                }
            };
            if bytes.is_empty() {
                send(&out, file_frame(&req, &file_id, 0, &[], true));
                return;
            }
            let total = bytes.len().div_ceil(CHUNK_BYTES);
            for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
                let last = index + 1 == total;
                if out
                    .send(file_frame(&req, &file_id, index as u32, chunk, last))
                    .is_err()
                {
                    return;
                }
                if !last {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
        });
        self.shelf_tasks.push(task);
    }
}

fn send(out: &OutTx, frame: String) {
    let _ = out.send(frame);
}

fn deliver_prompt(session: Arc<HubSession>, req: String, text: String, out: OutTx) {
    tokio::spawn(async move {
        match tokio::task::spawn_blocking(move || session.prompt(&text)).await {
            Ok(Ok(())) => send(&out, ok_frame(&req)),
            Ok(Err(error)) => send(
                &out,
                error_frame(Some(&req), ERR_DELIVERY_FAILED, &error.to_string()),
            ),
            Err(_) => send(
                &out,
                error_frame(Some(&req), ERR_INTERNAL, "prompt task failed"),
            ),
        }
    });
}

fn deliver_kill(hub: SessionHub, req: String, id: String, out: OutTx) {
    tokio::spawn(async move {
        let kill_id = id.clone();
        match tokio::task::spawn_blocking(move || hub.kill(&kill_id)).await {
            Ok(Some(_)) => send(&out, ok_frame(&req)),
            Ok(None) => send(
                &out,
                error_frame(
                    Some(&req),
                    ERR_NOT_FOUND,
                    &format!("no such session '{id}'"),
                ),
            ),
            Err(_) => send(
                &out,
                error_frame(Some(&req), ERR_INTERNAL, "kill task failed"),
            ),
        }
    });
}

/// Split one raw output payload into wire frames of at most
/// [`MAX_FRAME_BYTES`], each keeping the source `seq`. An empty payload is
/// one empty frame; nothing is ever truncated.
pub fn split_out_frames(id: &str, seq: u64, data: &[u8]) -> Vec<String> {
    if data.is_empty() {
        return vec![out_frame(id, seq, &[])];
    }
    data.chunks(CHUNK_BYTES)
        .map(|chunk| out_frame(id, seq, chunk))
        .collect()
}

/// Split one snapshot into a leading `snapshot` frame (cols/rows/seq and the
/// first chunk) followed by `snapshot_more` frames, the final one carrying
/// `last: true`.
pub fn split_snapshot_frames(id: &str, cols: u16, rows: u16, seq: u64, data: &[u8]) -> Vec<String> {
    let mut chunks = data.chunks(CHUNK_BYTES);
    let first = chunks.next().unwrap_or(&[]);
    let mut frames = vec![snapshot_frame(id, cols, rows, seq, first)];
    let rest: Vec<&[u8]> = chunks.collect();
    let total = rest.len();
    for (index, chunk) in rest.into_iter().enumerate() {
        frames.push(snapshot_more_frame(id, chunk, index + 1 == total));
    }
    frames
}

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;

fn spawn_forwarder(
    session: Arc<HubSession>,
    from_seq: u64,
    rx: OutputReceiver,
    out: OutTx,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rx = rx;
        let mut next = from_seq;
        loop {
            if !pump(&session, &mut next, &mut rx, &out).await {
                return;
            }
        }
    })
}

async fn pump(session: &HubSession, next: &mut u64, rx: &mut OutputReceiver, out: &OutTx) -> bool {
    match rx.recv().await {
        Ok((seq, chunk)) => {
            if seq <= *next {
                return true;
            }
            *next = seq;
            send_frames(out, split_out_frames(&session.id, seq, &chunk))
        }
        Err(broadcast::error::RecvError::Lagged(_)) => {
            let Some(snapshot) = session.fresh_snapshot() else {
                return false;
            };
            *next = snapshot.seq;
            let frames = split_snapshot_frames(
                &session.id,
                snapshot.cols,
                snapshot.rows,
                snapshot.seq,
                &snapshot.data,
            );
            send_frames(out, frames)
        }
        Err(broadcast::error::RecvError::Closed) => false,
    }
}

/// Send every frame in order; `false` when the connection is gone.
fn send_frames(out: &OutTx, frames: Vec<String>) -> bool {
    frames.into_iter().all(|frame| out.send(frame).is_ok())
}
