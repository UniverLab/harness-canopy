//! The session hub as a paired device sees it.
//!
//! A device peer is a remote client, not a local one: it speaks the same
//! protocol v1 frames as `/sessions/ws`, with one extra rule —
//!
//! * `resize` sizes the PTY only for the host owner's own paired device
//!   (clamped, at most one per 100 ms per session, latest wins); every
//!   guest answers `forbidden` (`not_found` outside its grant). Any resize
//!   reaches attached peers as `resized{id,cols,rows}` (see
//!   `bridge_resize.rs`).
//!
//!   `spawn`, `kill`, `graphs`, `graph_watch`, `graph_control`, `catalog`
//!   and `compose` are owner-only too: a guest inside its grant answers
//!   `forbidden` naming the frame, and `kill` outside the grant stays
//!   `not_found`.
//!
//! Terminal sessions are first-class here, as on the local socket: they
//! list, attach, and (for the owner) spawn without `cli` they open the
//! default shell. Only their shelf files stay hidden (see
//! `handle_shelf_list`).
//!
//! Outbound frames are size-bounded for a data channel: raw `out`,
//! `snapshot` and `file` payloads are split into chunks that keep the source
//! `seq`, the snapshot's first part staying in the `snapshot` frame (with
//! `cols`/`rows`) and the rest travelling as `snapshot_more{id,data,last}`.
//! Shelf transfers interleave with `out` frames, at most two per device — a
//! third `shelf_get` answers `error{code:"busy"}`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

use crate::remote::metrics::{
    handle_host_metrics, handle_host_metrics_unwatch, handle_host_metrics_watch, MetricsSource,
    MetricsWatch,
};
use crate::remote::trust::{Grant, GrantScope};
use crate::sessions::catalog::{handle_catalog, handle_compose, handle_prompt_draft};
use crate::sessions::graphs::{
    handle_graph_control, handle_graph_unwatch, handle_graph_watch, handle_graphs,
};
use crate::sessions::protocol::{
    error_frame, exited_frame, ok_frame, parse_client_frame, resized_frame, session_added_frame,
    session_removed_frame, sessions_frame, shelf_added_frame, shelf_missing_frame,
    shelf_removed_frame, ClientFrame, SessionItem, SpawnExt, ERR_BAD_REQUEST, ERR_DELIVERY_FAILED,
    ERR_FORBIDDEN, ERR_INTERNAL, ERR_NOT_FOUND, ERR_SPAWN_FAILED,
};
use crate::sessions::ws::no_such_session;
use crate::sessions::ws::{handle_shelf_delete, validate_spawn_extensions, OutTx, SessionsState};
use crate::sessions::{HubEvent, HubSession, SessionHub, SessionKind, SessionStatus, SpawnRequest};

/// Hard limit for one outbound JSON frame on a data channel.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// Raw bytes per chunk: 12 KiB minus headroom, so that base64 plus the
/// envelope stays under [`MAX_FRAME_BYTES`] (12 KiB raw encodes to 16 KiB).
pub const CHUNK_BYTES: usize = MAX_FRAME_BYTES * 3 / 4 - 256;

/// Attached output forwarders of one device peer, by session id.
type Attached = HashMap<String, JoinHandle<()>>;

/// One device peer's view of the hub.
pub struct RemoteBridge {
    pub(crate) state: Arc<SessionsState>,
    attached: Attached,
    /// Running `graph_watch` polls, by graph id: aborted with the bridge
    /// (the data channel closed), so no watch outlives its connection.
    watches: HashMap<String, JoinHandle<()>>,
    /// At most two `shelf_get` transfers in flight per device.
    pub(crate) transfers: Arc<Semaphore>,
    /// Running shelf transfers, aborted when the peer goes away.
    shelf_tasks: Vec<JoinHandle<()>>,
    /// At most three uploads in flight per device.
    pub(crate) uploads: Arc<Semaphore>,
    /// The permit each in-flight upload holds; removing an entry frees its
    /// slot on end, abort or a lost grant.
    pub(crate) upload_permits: HashMap<String, tokio::sync::OwnedSemaphorePermit>,
    /// Terminal-session files hidden from this peer, so their later
    /// `shelf_removed` is hidden too.
    hidden_files: HashSet<String>,
    /// Where `host.metrics` reads: the sampler's last sample.
    metrics: MetricsSource,
    /// The channel's one metrics watch, if any.
    metrics_watch: Option<MetricsWatch>,
    /// What this peer's login may see and touch.
    pub(crate) scope: GrantScope,
    /// Per-session `resize` throttles for this device.
    pub(crate) resize_gates: resize::ResizeGates,
    /// Deferred (throttled) resizes, aborted when the peer goes away.
    pub(crate) resize_tasks: Vec<JoinHandle<()>>,
}

/// The hub request a remote `spawn` frame describes. The kind is parsed
/// from the frame and fixed on the request by
/// [`RemoteBridge::handle_spawn`], which refuses anything but `agent` and
/// `terminal`.
fn spawn_request(
    cli: String,
    model: Option<String>,
    workdir: String,
    name: Option<String>,
    prompt: Option<String>,
    ext: SpawnExt,
) -> SpawnRequest {
    SpawnRequest {
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
        harness_conversation_id: ext.harness_conversation_id,
        seed_id: ext.seed_id,
        accent: ext.accent,
        yolo: ext.yolo,
        preserve_args: ext.preserve_args,
    }
}

impl RemoteBridge {
    /// Bridge the hub behind `state` for one device peer. Files already on
    /// a terminal shelf hide from the start, so their later `shelf_removed`
    /// stays hidden too (one indexed query per terminal session).
    pub fn new(state: Arc<SessionsState>) -> Self {
        Self::with_metrics(state, Arc::new(crate::system::host_metrics::snapshot))
    }

    /// Bridge the hub for one guest peer: [`Grant::All`] sees today's view;
    /// a scoped peer sees only its granted names, and anything outside the
    /// grant answers `not_found` (never `forbidden`: it would confirm it).
    pub fn with_grant(state: Arc<SessionsState>, grant: Grant, can_upload: bool) -> Self {
        let mut bridge = Self::new(state);
        bridge.scope = GrantScope::new(grant);
        bridge.scope.set_can_upload(can_upload);
        bridge.scope.set_owner(false);
        if !bridge.scope.is_all() {
            bridge.hide_unseen_shelf_files();
        }
        bridge
    }

    /// Files already on a shelf this peer may not see hide from the start, so
    /// their later `shelf_removed` stays hidden too.
    fn hide_unseen_shelf_files(&mut self) {
        let sessions = self.state.hub.list();
        let shelves =
            std::iter::once(None).chain(sessions.iter().map(|session| Some(session.id.clone())));
        for shelf in shelves {
            if !self.scope.hidden_file(&self.state.hub, shelf.as_deref()) {
                continue;
            }
            for item in self.state.shelf.list(shelf.as_deref()) {
                self.hidden_files.insert(item.file_id);
            }
        }
    }

    /// Mark this peer as the host owner's own paired device (the only remote
    /// party allowed to `resize`).
    pub fn set_owner(&mut self, owner: bool) {
        self.scope.set_owner(owner);
    }

    fn with_metrics(state: Arc<SessionsState>, metrics: MetricsSource) -> Self {
        let mut hidden_files = HashSet::new();
        for session in state.hub.list() {
            if session.kind == SessionKind::Terminal {
                for item in state.shelf.list(Some(&session.id)) {
                    hidden_files.insert(item.file_id);
                }
            }
        }
        Self {
            state,
            attached: HashMap::new(),
            watches: HashMap::new(),
            transfers: Arc::new(Semaphore::new(2)),
            shelf_tasks: Vec::new(),
            uploads: Arc::new(Semaphore::new(3)),
            upload_permits: HashMap::new(),
            hidden_files,
            metrics,
            metrics_watch: None,
            scope: GrantScope::all(),
            resize_gates: resize::ResizeGates::new(),
            resize_tasks: Vec::new(),
        }
    }

    /// Answer one inbound client frame. Sync frames answer inline; `prompt`
    /// and `kill` answer from a blocking task, exactly like the local socket.
    /// `graph_watch` and `graph_control` are awaited here (they validate and
    /// work off the executor) before the next frame is processed.
    pub async fn handle_text(&mut self, text: &str, out: &OutTx) {
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
                if !self.scope.granted(&self.state.hub, Some(&req), &id, out) {
                    return;
                }
                let Some(session) = self.state.hub.get(&id) else {
                    let _ = out.send(error_frame(
                        Some(&req),
                        ERR_NOT_FOUND,
                        &no_such_session(&id),
                    ));
                    return;
                };
                if session.status() != SessionStatus::Running {
                    let _ = out.send(error_frame(Some(&req), ERR_FORBIDDEN, "session has exited"));
                    return;
                }
                deliver_prompt(session, req, text, out.clone());
            }
            ClientFrame::PromptDraft { req, id, draft } => {
                if !self.scope.granted(&self.state.hub, Some(&req), &id, out) {
                    return;
                }
                handle_prompt_draft(&self.state, out, req, &id, draft);
            }
            frame @ (ClientFrame::PromptSchedule { .. }
            | ClientFrame::ScheduledList { .. }
            | ClientFrame::ScheduledCancel { .. }) => self.handle_schedule(frame, out),
            ClientFrame::Resize { id, cols, rows } => self.handle_resize(&id, cols, rows, out),
            ClientFrame::ResizeRelease { id } => self.handle_resize_release(&id, out),
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
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "spawn");
                    return;
                }
                let request = spawn_request(cli, model, workdir, name, prompt, *ext);
                self.handle_spawn(req, &kind, request, out);
            }
            ClientFrame::Kill { req, id } => {
                // The grant check runs first, so a session outside the grant
                // still answers `not_found` and never confirms it exists.
                if !self.scope.granted(&self.state.hub, Some(&req), &id, out) {
                    return;
                }
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "kill");
                    return;
                }
                deliver_kill(self.state.hub.clone(), req, id, out.clone());
            }
            frame @ ClientFrame::ShelfPutBegin { .. } => self.handle_shelf_put_begin(frame, out),
            frame @ ClientFrame::ShelfPutPart { .. } => self.handle_shelf_put_part(frame, out),
            ClientFrame::ShelfPutEnd { file_id } => self.handle_shelf_put_end(&file_id, out),
            ClientFrame::ShelfPutAbort { file_id } => self.handle_shelf_put_abort(&file_id),
            frame @ (ClientFrame::ShelfList { .. }
            | ClientFrame::ShelfGet { .. }
            | ClientFrame::ShelfDelete { .. }
            | ClientFrame::Compose { .. }
            | ClientFrame::Catalog { .. }
            | ClientFrame::Graphs { .. }
            | ClientFrame::GraphWatch { .. }
            | ClientFrame::GraphUnwatch { .. }
            | ClientFrame::GraphControl { .. }
            | ClientFrame::HostMetrics { .. }
            | ClientFrame::HostMetricsWatch { .. }
            | ClientFrame::HostMetricsUnwatch) => self.handle_read_frame(frame, out).await,
        }
    }

    /// Answer one read frame: shelf, catalog, graphs and host metrics, plus
    /// the owner-only `graph_control` that pauses or continues a graph.
    /// The session-mutating arms stay in [`Self::handle_text`], which only
    /// routes the variants matched here, so both matches stay exhaustive: a
    /// new frame fails to compile until both sides name it.
    async fn handle_read_frame(&mut self, frame: ClientFrame, out: &OutTx) {
        match frame {
            ClientFrame::ShelfList { req, id } => {
                self.handle_shelf_list(req, id, out);
            }
            ClientFrame::ShelfGet { req, file_id } => {
                self.handle_shelf_get(req, file_id, out);
            }
            ClientFrame::ShelfDelete { req, file_id } => {
                if self
                    .scope
                    .hidden_shelf_file(&self.state.hub, &self.state.shelf, &file_id)
                {
                    let _ = out.send(shelf_missing_frame(&req, &file_id));
                } else {
                    handle_shelf_delete(&self.state, out, req, file_id);
                }
            }
            ClientFrame::Compose { req, draft } => {
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "compose");
                    return;
                }
                handle_compose(&self.state, out, req, draft);
            }
            ClientFrame::Catalog { req } => {
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "catalog");
                    return;
                }
                handle_catalog(&self.state, out, req);
            }
            ClientFrame::Graphs { req } => {
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "graphs");
                    return;
                }
                handle_graphs(&self.state, out, req);
            }
            ClientFrame::GraphWatch { req, id } => {
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "graph_watch");
                    return;
                }
                handle_graph_watch(&self.state, out, &mut self.watches, req, id).await;
            }
            ClientFrame::GraphUnwatch { id } => {
                handle_graph_unwatch(&mut self.watches, &id);
            }
            ClientFrame::GraphControl { req, id, action } => {
                if !self.scope.is_owner() {
                    owner_denied(out, Some(&req), "graph_control");
                    return;
                }
                handle_graph_control(&self.state, out, req, id, action).await;
            }
            ClientFrame::HostMetrics { req } => handle_host_metrics(&self.metrics, out, &req),
            ClientFrame::HostMetricsWatch { interval_s } => {
                handle_host_metrics_watch(&mut self.metrics_watch, &self.metrics, out, interval_s);
            }
            ClientFrame::HostMetricsUnwatch => {
                handle_host_metrics_unwatch(&mut self.metrics_watch);
            }
            ClientFrame::List { .. }
            | ClientFrame::Attach { .. }
            | ClientFrame::Detach { .. }
            | ClientFrame::Input { .. }
            | ClientFrame::Prompt { .. }
            | ClientFrame::PromptDraft { .. }
            | ClientFrame::PromptSchedule { .. }
            | ClientFrame::ScheduledList { .. }
            | ClientFrame::ScheduledCancel { .. }
            | ClientFrame::Resize { .. }
            | ClientFrame::ResizeRelease { .. }
            | ClientFrame::Spawn { .. }
            | ClientFrame::Kill { .. }
            | ClientFrame::ShelfPutBegin { .. }
            | ClientFrame::ShelfPutPart { .. }
            | ClientFrame::ShelfPutEnd { .. }
            | ClientFrame::ShelfPutAbort { .. } => {
                unreachable!("session frames never route to handle_read_frame");
            }
        }
    }

    /// Forward one hub lifecycle event, with ungranted sessions
    /// filtered out.
    pub fn handle_event(&mut self, event: HubEvent, out: &OutTx) {
        match event {
            HubEvent::SessionAdded(item) => {
                if self.scope.note_listed(&item.id, &item.name) {
                    send(out, session_added_frame(&item));
                }
            }
            HubEvent::SessionRemoved(id) => {
                let mut was_attached = false;
                if let Some(task) = self.attached.remove(&id) {
                    task.abort();
                    was_attached = true;
                }
                if self.scope.removed_forwards(&id, was_attached) {
                    send(out, session_removed_frame(&id));
                }
            }
            HubEvent::Exited { id, code, to_all } => {
                if self
                    .scope
                    .exited_forwards(&id, to_all, self.attached.contains_key(&id))
                {
                    send(out, exited_frame(&id, code));
                }
            }
            HubEvent::Resized { id, cols, rows } => {
                // Attach is grant-checked, so an attached session is granted.
                if self.attached.contains_key(&id) {
                    send(out, resized_frame(&id, cols, rows));
                }
            }
            // Releases stay host-local: remote peers never size, so they
            // never need to re-assert a size.
            HubEvent::SizeReleased { .. } => {}
            HubEvent::ShelfAdded(item) => {
                if self.hidden_terminal_file(item.session_id.as_deref())
                    || self
                        .scope
                        .hidden_file(&self.state.hub, item.session_id.as_deref())
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

    /// Abort every output forwarder, graph watch, shelf transfer and
    /// metrics watch (the data channel closed or the device was revoked).
    /// In-flight uploads are abandoned with their temps: only this bridge
    /// knows their reserved ids, so nothing else can finish them, and an
    /// in-map temp would otherwise survive the orphan sweep until a restart.
    pub fn abort(&mut self) {
        for task in self.attached.drain() {
            task.1.abort();
        }
        for task in self.watches.drain() {
            task.1.abort();
        }
        for task in self.shelf_tasks.drain(..) {
            task.abort();
        }
        for task in self.resize_tasks.drain(..) {
            task.abort();
        }
        // A device that vanishes lets go of every session it sized: one
        // release per sized session, so attached TUIs re-assert their size.
        let sized: Vec<String> = self.resize_gates.keys().cloned().collect();
        self.resize_gates.clear();
        for id in sized {
            self.state.hub.emit(HubEvent::SizeReleased { id });
        }
        for file_id in self.upload_permits.keys().cloned().collect::<Vec<_>>() {
            self.state.shelf.abort_upload(&file_id);
        }
        self.upload_permits.clear();
        self.metrics_watch = None;
    }

    /// Record a shelf event's hidden-ness without forwarding it. The serve
    /// loop feeds pre-greeting events here: nothing may overtake `hello`,
    /// but a terminal file that lands during the handshake must still hide
    /// its later removal.
    pub fn note_hidden(&mut self, event: &HubEvent) {
        match event {
            HubEvent::ShelfAdded(item) => {
                if self.hidden_terminal_file(item.session_id.as_deref())
                    || self
                        .scope
                        .hidden_file(&self.state.hub, item.session_id.as_deref())
                {
                    self.hidden_files.insert(item.file_id.clone());
                }
            }
            HubEvent::ShelfRemoved { file_id } => {
                self.hidden_files.remove(file_id);
            }
            _ => {}
        }
    }

    pub(super) fn terminal(&self, id: &str) -> bool {
        self.state
            .hub
            .get(id)
            .is_some_and(|session| session.kind == SessionKind::Terminal)
    }

    /// A file that stays hidden from this peer: it belongs to a live
    /// terminal session.
    pub(super) fn hidden_terminal_file(&self, session_id: Option<&str>) -> bool {
        session_id.is_some_and(|id| self.terminal(id))
    }

    fn handle_list(&mut self, req: &str, out: &OutTx) {
        let mut items: Vec<SessionItem> = self
            .state
            .hub
            .list()
            .iter()
            .filter(|session| self.scope.allows(&session.name))
            .map(|session| session.item())
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        for item in &items {
            self.scope.note_listed(&item.id, &item.name);
        }
        send(out, sessions_frame(req, &items));
    }

    fn attach(&mut self, id: &str, out: &OutTx) {
        if !self.scope.granted(&self.state.hub, None, id, out) {
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
        let frames = forward::split_snapshot_frames(
            id,
            snapshot.cols,
            snapshot.rows,
            snapshot.seq,
            &snapshot.data,
        );
        // The snapshot's wire bytes still owe a pass through the connection's
        // byte budget: the forwarder credits them so output produced while
        // the snapshot travels queues behind it instead of asking for a
        // second snapshot.
        let snapshot_wire = frames.iter().map(String::len).sum();
        for frame in frames {
            send(out, frame);
        }
        let task = forward::spawn_forwarder(session, snapshot.seq, rx, out.clone(), snapshot_wire);
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
        if !self.scope.granted(&self.state.hub, None, id, out) {
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
        if (parsed == SessionKind::Agent && request.cli.is_empty()) || request.workdir.is_empty() {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_BAD_REQUEST,
                "spawn: fields 'cli' and 'workdir' must not be empty",
            ));
            return;
        }
        request.kind = parsed;
        if let Err(message) = validate_spawn_extensions(&mut request) {
            let _ = out.send(error_frame(Some(&req), ERR_BAD_REQUEST, &message));
            return;
        }
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
}

fn send(out: &OutTx, frame: String) {
    let _ = out.send(frame);
}

/// Refuse an owner-only frame, naming it in `resize`'s wording.
fn owner_denied(out: &OutTx, req: Option<&str>, frame: &str) {
    let _ = out.send(error_frame(
        req,
        ERR_FORBIDDEN,
        &format!("{frame} is only available to the host owner's devices"),
    ));
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

#[cfg(test)]
#[path = "bridge_tests.rs"]
mod tests;

#[path = "bridge_read.rs"]
mod read;

#[path = "bridge_upload.rs"]
mod upload;

#[path = "bridge_resize.rs"]
pub(crate) mod resize;

#[path = "bridge_forward.rs"]
mod forward;
