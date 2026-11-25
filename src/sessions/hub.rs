//! `SessionHub`: daemon-owned PTY sessions.
//!
//! Each hub session owns its PTY master, child process, writer and an
//! authoritative vt100 parser. A reader thread feeds PTY output into the
//! parser and publishes `(seq, chunk)` pairs on a broadcast channel;
//! WebSocket clients attach to sessions through [`crate::sessions::ws`] and
//! receive snapshots plus the chunks after them.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;

use chrono::{DateTime, Utc};
use portable_pty::{native_pty_system, PtySize};
use tokio::sync::broadcast;

use crate::db::session::SessionRegistration;
use crate::db::Database;
use crate::domain::models::Cli;

#[cfg(unix)]
use crate::tui::agent::pty::install_signal_handlers;

use super::compose_guard::{ComposeState, Delivery};
use super::hub_shutdown::HubShutdown;
use super::protocol::SessionItem;
use super::{preserve_rows_on_shrink, spawn_identity, write_submitted_prompt, VT_SCROLLBACK_LINES};

/// Hub parser: vt100 with the unit no-op callbacks (the daemon forwards
/// nothing to any clipboard; attached clients handle OSC 52 themselves).
pub type HubVt = vt100::Parser<()>;

/// Live output subscription for one attached consumer: `(seq, chunk)` pairs
/// after an attach snapshot's `seq`.
pub type OutputReceiver = broadcast::Receiver<(u64, Vec<u8>)>;

/// Default PTY size for hub spawns — the spawn frame carries no size, so
/// sessions start here and clients resize after attaching.
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

/// Default per-session output broadcast capacity.
const DEFAULT_BROADCAST_CAPACITY: usize = 256;

/// Hub lifecycle event broadcast capacity.
const EVENT_BROADCAST_CAPACITY: usize = 128;

/// Bytes of a harness's output a failed session row keeps as its `exit_detail`.
pub(crate) const EXIT_DETAIL_MAX_BYTES: usize = 2048;

/// What a hub session runs: an AI CLI (`agent`) or a plain shell (`terminal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Agent,
    Terminal,
}

impl SessionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionKind::Agent => "agent",
            SessionKind::Terminal => "terminal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "agent" => Some(SessionKind::Agent),
            "terminal" => Some(SessionKind::Terminal),
            _ => None,
        }
    }
}

/// Liveness of a hub session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Exited(i32),
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Running => "running",
            SessionStatus::Exited(_) => "exited",
        }
    }

    pub fn exit_code(&self) -> Option<i32> {
        match self {
            SessionStatus::Running => None,
            SessionStatus::Exited(code) => Some(*code),
        }
    }
}

/// The hub's lifecycle event bus, defined in [`super::hub_event`] so this
/// file stays under the file-size cap; re-exported to keep the
/// `crate::sessions::hub::HubEvent` path stable.
pub use super::hub_event::HubEvent;

/// Everything under the parser lock: the authoritative screen, the output
/// sequence number, and the dimensions the screen was sized to. Snapshot and
/// resize take this lock once so screen, `seq` and size stay consistent.
struct OutputState {
    parser: HubVt,
    seq: u64,
    last_output_at: DateTime<Utc>,
    cols: u16,
    rows: u16,
}

/// One daemon-owned PTY session.
pub struct HubSession {
    pub id: String,
    pub name: String,
    pub kind: SessionKind,
    pub cli: String,
    pub model: Option<String>,
    pub workdir: String,
    pub started_at: DateTime<Utc>,
    pub pid: Option<i64>,
    state: Mutex<OutputState>,
    status: Mutex<SessionStatus>,
    compose: Mutex<ComposeState>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    bcast: broadcast::Sender<(u64, Vec<u8>)>,
}

/// A point-in-time screen capture plus the output sequence number it covers.
pub struct Snapshot {
    pub seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub data: Vec<u8>,
}

/// What [`SessionHub::spawn`] launches.
pub struct SpawnRequest {
    pub kind: SessionKind,
    /// Agent: CLI platform name; terminal: shell binary.
    pub cli: String,
    pub model: Option<String>,
    pub workdir: String,
    pub name: Option<String>,
    pub prompt: Option<String>,
    /// Explicit launch args from the New dialog (resume/yolo included);
    /// when `None` the hub falls back to the registry.
    pub interactive_args: Option<String>,
    pub fallback_args: Option<String>,
    pub model_flag: Option<String>,
    /// Resume identity: reuse this id instead of minting a fresh uuid.
    pub id: Option<String>,
    /// The harness's own conversation identifier for this spawn, when the
    /// launcher knows it. Recorded on the session row so the next resume of
    /// the same conversation resolves back to this session.
    pub harness_conversation_id: Option<String>,
    /// Canopy accent as `"r,g,b"`.
    pub accent: Option<String>,
    /// Skip the harness's approval prompts: append its registry `yolo_flag`
    /// to the interactive args. Handlers refuse `yolo` for a terminal or a
    /// harness with no flag before the hub ever sees it.
    pub yolo: bool,
    /// A resume relaunching an existing row: reactivate it without rewriting
    /// its recorded launch args — the resume command line is rebuilt from
    /// the stored harness conversation id at every startup, never stored.
    pub preserve_args: bool,
}

impl HubSession {
    /// Ingest one PTY output chunk: feed the parser, bump `seq`, stamp
    /// `last_output_at`, and publish `(seq, chunk)` — all under the parser
    /// lock. The reader thread calls this per chunk; tests call it directly
    /// to drive a session deterministically.
    pub(crate) fn ingest_chunk(&self, chunk: &[u8]) {
        let Ok(mut out) = self.state.lock() else {
            return;
        };
        out.parser.process(chunk);
        out.seq += 1;
        out.last_output_at = Utc::now();
        let seq = out.seq;
        let _ = self.bcast.send((seq, chunk.to_vec()));
    }

    /// Attach a consumer: take a snapshot and subscribe to output in one
    /// step, under the parser lock, so no chunk is lost or duplicated —
    /// everything at or below the snapshot's `seq` is in the snapshot,
    /// everything after it arrives on the receiver.
    pub fn attach(&self) -> Option<(Snapshot, OutputReceiver)> {
        let mut out = self.state.lock().ok()?;
        let rx = self.bcast.subscribe();
        let snapshot = snapshot_locked(&mut out);
        Some((snapshot, rx))
    }

    /// A fresh snapshot for a lagged subscriber: same bytes as [`Self::attach`]
    /// but no new subscription (the receiver keeps its position and the
    /// caller drops everything at or below the snapshot's `seq`).
    pub fn fresh_snapshot(&self) -> Option<Snapshot> {
        self.state
            .lock()
            .ok()
            .map(|mut out| snapshot_locked(&mut out))
    }

    /// Send raw bytes to the session's PTY stdin.
    pub fn write_to_pty(&self, data: &[u8]) -> anyhow::Result<()> {
        if let Ok(mut compose) = self.compose.lock() {
            compose.observe(data);
        }
        if let Ok(mut w) = self.writer.lock() {
            w.write_all(data)?;
            w.flush()?;
        }
        Ok(())
    }

    /// Whether the child enabled bracketed-paste mode.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.state
            .try_lock()
            .map(|out| out.parser.screen().bracketed_paste())
            .unwrap_or(false)
    }

    /// Deliver `text` as a SUBMITTED prompt through the shared paste path,
    /// honoring the child's bracketed-paste mode and this CLI's submit spec.
    pub fn prompt(&self, text: &str) -> anyhow::Result<()> {
        let bracketed = self.bracketed_paste_enabled();
        let spec = Cli::new(self.cli.as_str()).paste_submit_spec();
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("pty writer lock poisoned"))?;
        write_submitted_prompt(&mut *writer, text, bracketed, spec)?;
        Ok(())
    }

    /// Whether the user is composing a prompt: a dirty line, or any
    /// keystroke in the last two seconds.
    pub fn user_is_composing(&self) -> bool {
        self.compose
            .lock()
            .map(|state| state.is_composing())
            .unwrap_or(false)
    }

    /// Deliver `text` only while the user is idle (else [`Delivery::Held`]);
    /// explicit prompts stay on [`Self::prompt`].
    pub fn prompt_if_idle(&self, text: &str) -> anyhow::Result<Delivery> {
        let bracketed = self.bracketed_paste_enabled();
        let spec = Cli::new(self.cli.as_str()).paste_submit_spec();
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| anyhow::anyhow!("pty writer lock poisoned"))?;
        if self.user_is_composing() {
            return Ok(Delivery::Held);
        }
        write_submitted_prompt(&mut *writer, text, bracketed, spec)?;
        Ok(Delivery::Delivered)
    }

    /// Resize the PTY and the parser through the same shrink-safe path the
    /// TUI uses: rows that would fall off go to scrollback first, and both
    /// steps run under one lock acquisition.
    ///
    /// Returns `true` when the size actually changed, so the caller tells
    /// attached connections with a `resized` frame. The check, the PTY resize
    /// and the parser resize all run under the parser lock, so two racing
    /// resizes cannot both report (or both miss) the same change.
    pub fn resize(&self, cols: u16, rows: u16) -> bool {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let Ok(mut out) = self.state.lock() else {
            return false;
        };
        if out.cols == cols && out.rows == rows {
            return false;
        }
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        let old_rows = out.rows;
        preserve_rows_on_shrink(&mut out.parser, old_rows, rows);
        out.parser.screen_mut().set_size(rows, cols);
        out.cols = cols;
        out.rows = rows;
        true
    }

    pub fn status(&self) -> SessionStatus {
        self.status
            .lock()
            .ok()
            .map(|s| *s)
            .unwrap_or(SessionStatus::Running)
    }

    /// The session's metadata row for `list` / `session_added` frames.
    pub fn item(&self) -> SessionItem {
        let (cols, rows, last_output_at) = self
            .state
            .lock()
            .map(|out| (out.cols, out.rows, out.last_output_at))
            .unwrap_or((DEFAULT_COLS, DEFAULT_ROWS, self.started_at));
        let status = self.status();
        SessionItem {
            id: self.id.clone(),
            name: self.name.clone(),
            kind: self.kind.as_str().to_string(),
            cli: self.cli.clone(),
            model: self.model.clone(),
            workdir: self.workdir.clone(),
            started_at: self.started_at.to_rfc3339(),
            pid: self.pid,
            cols,
            rows,
            last_output_at: last_output_at.to_rfc3339(),
            status: status.as_str().to_string(),
            exit_code: status.exit_code(),
        }
    }

    /// Mark this session's database row finished, the way the TUI does when
    /// a session of the same kind ends. The status flip and the failure
    /// detail commit in one update: a reader polling the row can never
    /// observe `error` ahead of its explanation. The caller passes the exit
    /// code it transitioned the status with while holding the status lock,
    /// so the recorded code always matches the transition.
    fn finish_db_row(&self, db: &Database, code: i32) {
        match self.kind {
            SessionKind::Agent => {
                let detail = (code != 0).then(|| self.exit_detail_text(code));
                let _ =
                    db.finish_interactive_session_with_detail(&self.id, code, detail.as_deref());
            }
            SessionKind::Terminal => {
                let _ = db.finish_terminal_session(&self.id);
            }
        }
    }

    /// Mark this session's database row finished after an explicit `kill`:
    /// the user closed it, so no failure detail is recorded or kept — a
    /// killed session reads as closed, never as a failure with a tail, and
    /// the TUI drops it instead of retaining an error card for it. The
    /// caller passes the exit code while holding the status lock, so a
    /// concurrent natural exit cannot interleave a failure tail after this
    /// clear (see `record_natural_exit`).
    fn finish_db_row_killed(&self, db: &Database, code: i32) {
        match self.kind {
            SessionKind::Agent => {
                let _ = db.finish_interactive_session_with_detail(&self.id, code, None);
            }
            SessionKind::Terminal => {
                let _ = db.finish_terminal_session(&self.id);
            }
        }
    }

    /// This session's nonzero-exit detail: its output's head and tail,
    /// or the bare code when it died silent (see the free function).
    fn exit_detail_text(&self, code: i32) -> String {
        exit_detail_text(&self.state, code)
    }

    /// Test pin for the tail helper (production detail goes through
    /// `exit_detail_text`). `None` only when the output lock is poisoned.
    #[cfg(test)]
    pub(crate) fn output_tail(&self, max_bytes: usize) -> Option<String> {
        self.state.lock().ok().map(|out| {
            let contents = out.parser.screen().contents();
            tail_chars(contents.trim_end(), max_bytes)
        })
    }

    /// One non-blocking exit poll; `None` while the child is still alive.
    fn poll_exit_code(&self) -> Option<i32> {
        self.child
            .lock()
            .ok()
            .and_then(|mut child| child.try_wait().ok().flatten())
            .map(|status| status.exit_code().try_into().unwrap_or(-1))
    }
}

#[path = "hub_snapshot.rs"]
mod snapshot;
use snapshot::snapshot_locked;
#[path = "hub_process.rs"]
mod child;
#[cfg(unix)]
use child::send_sighup_to_group;
#[cfg(test)]
use child::tail_chars;
use child::{exit_detail_text, spawn_reader_thread, wait_for_exit_code};
#[cfg(test)]
pub(crate) use snapshot::{scrollback_formatted, SNAPSHOT_SCROLLBACK_LINES};

/// The daemon's session hub: owns hub sessions and broadcasts their
/// lifecycle events. Clone shares the hub.
#[derive(Clone)]
pub struct SessionHub {
    inner: Arc<HubInner>,
}

struct HubInner {
    sessions: Mutex<HashMap<String, Arc<HubSession>>>,
    events: broadcast::Sender<HubEvent>,
    db: Arc<Database>,
    bcast_capacity: usize,
    host_label: String,
    shelf: super::hub_shelf::ShelfLink,
    /// Where fresh muse spawns look for their conversation id; unset means
    /// the live store ([`super::muse_conversation::muse_store_dir`]). Tests
    /// point it at a scratch store.
    muse_store_override: std::sync::OnceLock<std::path::PathBuf>,
    /// Set when the daemon's stop signal arrives: child exits from then on
    /// keep their rows resumable instead of finishing them.
    shutdown: Arc<HubShutdown>,
}

impl SessionHub {
    pub fn new(db: Arc<Database>, host_label: String) -> Self {
        Self::with_broadcast_capacity(db, host_label, DEFAULT_BROADCAST_CAPACITY)
    }

    pub fn with_broadcast_capacity(
        db: Arc<Database>,
        host_label: String,
        bcast_capacity: usize,
    ) -> Self {
        let (events, _) = broadcast::channel(EVENT_BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(HubInner {
                sessions: Mutex::new(HashMap::new()),
                events,
                db,
                bcast_capacity: bcast_capacity.max(1),
                host_label,
                shelf: super::hub_shelf::ShelfLink::default(),
                muse_store_override: std::sync::OnceLock::new(),
                shutdown: Arc::new(HubShutdown::new()),
            }),
        }
    }

    /// Mark the daemon as shutting down: a session child that exits from now
    /// on keeps its row resumable (the next start brings it back) instead of
    /// finishing it.
    pub fn set_shutting_down(&self) {
        self.inner.shutdown.set();
    }

    /// Whether the daemon's stop signal has arrived.
    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutdown.is_set()
    }
    pub fn wire_shelf(&self, shelf: Arc<super::shelf::ShelfStore>) {
        self.inner.shelf.wire(self.inner.events.clone(), shelf);
    }

    pub fn host_label(&self) -> &str {
        &self.inner.host_label
    }

    /// The database the hub records its session rows in. Startup
    /// reconciliation reads the rows it resumes through this.
    pub fn db(&self) -> &Arc<Database> {
        &self.inner.db
    }

    /// Spawn a hub session: build the child exactly like the TUI does, open
    /// its PTY, record its database row, start its reader thread, and announce
    /// it. `prompt`, when given, is delivered through the paste path.
    pub fn spawn(&self, req: &SpawnRequest) -> anyhow::Result<Arc<HubSession>> {
        #[cfg(unix)]
        install_signal_handlers();

        let existing: Vec<String> = self
            .inner
            .sessions
            .lock()
            .map(|map| map.values().map(|s| s.name.clone()).collect())
            .unwrap_or_default();
        let existing_refs: Vec<&str> = existing.iter().map(String::as_str).collect();
        let live_ids: std::collections::HashSet<String> = self
            .inner
            .sessions
            .lock()
            .map(|map| map.keys().cloned().collect())
            .unwrap_or_default();
        let wanted_id = req.id.as_deref().filter(|id| !live_ids.contains(*id));
        let (id, name) = match req.kind {
            SessionKind::Agent => spawn_identity(wanted_id, req.name.as_deref(), &existing_refs),
            SessionKind::Terminal => {
                let name = req.name.clone().unwrap_or_else(|| {
                    crate::tui::agent::naming::pick_terminal_name(&existing_refs)
                });
                let id = wanted_id
                    .map(str::to_owned)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                (id, name)
            }
        };

        let (cmd, args) = super::hub_command::build_spawn_command(req, &id, &name);
        // Sampled before the child exists: its conversation lock is newer.
        let spawn_since = std::time::SystemTime::now();
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows: DEFAULT_ROWS,
            cols: DEFAULT_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| anyhow::anyhow!("spawn {}: {e}", req.cli))?;
        drop(pair.slave);
        let pid = child.process_id().map(|p| p as i64);

        let writer = pair.master.take_writer()?;
        let reader = pair.master.try_clone_reader()?;
        let master = pair.master;
        let (bcast, _) = broadcast::channel(self.inner.bcast_capacity);
        let now = Utc::now();
        let session = Arc::new(HubSession {
            id: id.clone(),
            name,
            kind: req.kind,
            cli: req.cli.clone(),
            model: req.model.clone(),
            workdir: req.workdir.clone(),
            started_at: now,
            pid,
            state: Mutex::new(OutputState {
                parser: HubVt::new_with_callbacks(
                    DEFAULT_ROWS,
                    DEFAULT_COLS,
                    VT_SCROLLBACK_LINES,
                    (),
                ),
                seq: 0,
                last_output_at: now,
                cols: DEFAULT_COLS,
                rows: DEFAULT_ROWS,
            }),
            status: Mutex::new(SessionStatus::Running),
            compose: Mutex::new(ComposeState::new()),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            master: Mutex::new(master),
            bcast,
        });

        if let Ok(mut map) = self.inner.sessions.lock() {
            map.insert(id.clone(), Arc::clone(&session));
        }
        self.insert_db_row(req, &session, args.as_deref());
        self.record_muse_conversation(req, &session, spawn_since);
        spawn_reader_thread(self.clone(), Arc::clone(&session), reader);
        self.emit(HubEvent::SessionAdded(Box::new(session.item())));

        if let Some(prompt) = req.prompt.as_deref() {
            if let Err(e) = session.prompt(prompt) {
                tracing::warn!("Hub session '{id}' spawned but its prompt was not delivered: {e}");
            }
        }
        Ok(session)
    }

    /// Write the `interactive_sessions` / `terminal_sessions` row the TUI
    /// writes for the same kind of launch.
    fn insert_db_row(&self, req: &SpawnRequest, session: &HubSession, args: Option<&str>) {
        match req.kind {
            SessionKind::Agent => {
                let boot_id = crate::system::boot_id();
                let registration = SessionRegistration {
                    id: &session.id,
                    name: &session.name,
                    cli: req.cli.as_str(),
                    working_dir: req.workdir.as_str(),
                    args,
                    pid: session.pid,
                    session_type: "interactive",
                    boot_id: boot_id.as_deref(),
                    harness_conversation_id: req.harness_conversation_id.as_deref(),
                    preserve_args: req.preserve_args,
                };
                if let Err(e) = self.inner.db.register_interactive_session(&registration) {
                    tracing::warn!("Failed to register hub session: {e}");
                }
            }
            SessionKind::Terminal => {
                let _ = self.inner.db.insert_terminal_session(
                    &session.id,
                    &session.name,
                    req.cli.as_str(),
                    req.workdir.as_str(),
                );
            }
        }
    }

    /// A fresh muse spawn is a new conversation: discover the harness's own
    /// id for it on a background thread, so a later restart resumes this
    /// conversation. Done here, not by each launcher, so every spawn path
    /// (TUI, remote, scheduler) records it.
    fn record_muse_conversation(
        &self,
        req: &SpawnRequest,
        session: &HubSession,
        since: std::time::SystemTime,
    ) {
        if !super::muse_conversation::hub_spawn_records_conversation(req) {
            return;
        }
        let store = self
            .inner
            .muse_store_override
            .get()
            .cloned()
            .or_else(super::muse_conversation::muse_store_dir);
        super::muse_conversation::record_fresh_conversation(
            Arc::clone(&self.inner.db),
            session.id.clone(),
            req.workdir.clone(),
            since,
            session.pid,
            store,
        );
    }

    /// Resize `session` and, when its size really changed, tell every
    /// connection attached to it (`resized`). The one resize path for the
    /// local socket and the remote bridge alike.
    pub fn resize_session(&self, session: &HubSession, cols: u16, rows: u16) -> bool {
        if !session.resize(cols, rows) {
            return false;
        }
        self.emit(HubEvent::Resized {
            id: session.id.clone(),
            cols: cols.max(1),
            rows: rows.max(1),
        });
        true
    }

    /// Point fresh muse spawns at a scratch session store.
    #[cfg(test)]
    pub(crate) fn set_muse_store_dir(&self, dir: std::path::PathBuf) {
        let _ = self.inner.muse_store_override.set(dir);
    }

    pub fn get(&self, id: &str) -> Option<Arc<HubSession>> {
        self.inner.sessions.lock().ok()?.get(id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<HubSession>> {
        self.inner
            .sessions
            .lock()
            .map(|map| map.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<HubEvent> {
        self.inner.events.subscribe()
    }

    pub(crate) fn emit(&self, event: HubEvent) {
        let _ = self.inner.events.send(event);
    }

    /// Record a natural child exit: mark the session exited and its row
    /// finished, tell attached connections, drop it from the list, and tell
    /// every connection it is gone. A no-op when `kill` already recorded
    /// the exit, so the events are sent once. The status lock is held across
    /// the transition AND the row finish so a concurrent `kill` — which
    /// finishes the same row with no detail under this same lock — strictly
    /// orders with this write: the transition winner finishes its row
    /// before the loser observes the status, and `kill` always finishes
    /// with no detail, so a killed session can never keep a failure tail
    /// written after the kill's clear.
    pub(crate) fn record_natural_exit(&self, session: &HubSession, code: i32) {
        let Ok(mut status) = session.status.lock() else {
            return;
        };
        if *status != SessionStatus::Running {
            return;
        }
        *status = SessionStatus::Exited(code);
        // During a daemon stop the child dies because the daemon is going
        // away, not because the session ended: leave the row as it was so
        // the next start resumes it (agent `active`, terminal `idle`).
        if !self.is_shutting_down() {
            session.finish_db_row(&self.inner.db, code);
        }
        // Drop from the list only after the row write commits: a concurrent
        // `kill` that finds no session still clears the failure tail, and
        // that clear must land after this write, never before it.
        if let Ok(mut sessions) = self.inner.sessions.lock() {
            sessions.remove(session.id.as_str());
        }
        drop(status);
        self.emit(HubEvent::Exited {
            id: session.id.clone(),
            code,
            to_all: false,
        });
        self.emit(HubEvent::SessionRemoved(session.id.clone()));
    }

    /// End a session the way closing it in the TUI does (signal to the
    /// process group), mark its row finished, remove it, and send `exited`
    /// plus `session_removed` to every connection. Returns the exit code, or
    /// `None` when there is no such session.
    pub fn kill(&self, id: &str) -> Option<i32> {
        let session = self.inner.sessions.lock().ok()?.remove(id);
        let Some(session) = session else {
            // Already finished naturally (e.g. the reader thread won the
            // race): still treat this as an explicit close and drop any
            // failure tail so the row reads as closed, not failed.
            let _ = self.inner.db.clear_session_exit_detail(id);
            return None;
        };
        if let Ok(mut child) = session.child.lock() {
            #[cfg(unix)]
            send_sighup_to_group(child.as_mut());
            let _ = child.kill();
        }
        let code = match session.status() {
            SessionStatus::Exited(code) => code,
            SessionStatus::Running => wait_for_exit_code(&session).unwrap_or(-9),
        };
        // Finish under the same status lock `record_natural_exit` holds
        // across its transition and row write: the lock is held across this
        // path's transition AND its row write too, so the two writes are
        // strictly ordered and this path — which always finishes with no
        // detail — wins whenever it raced a natural exit. The wait above
        // stays outside the lock; the reader thread needs the lock to make
        // progress while this waits for the child.
        if let Ok(mut status) = session.status.lock() {
            if *status == SessionStatus::Running {
                *status = SessionStatus::Exited(code);
            }
            let final_code = status.exit_code().unwrap_or(code);
            session.finish_db_row_killed(&self.inner.db, final_code);
        } else {
            session.finish_db_row_killed(&self.inner.db, code);
        }
        self.emit(HubEvent::Exited {
            id: id.to_string(),
            code,
            to_all: true,
        });
        self.emit(HubEvent::SessionRemoved(id.to_string()));
        self.inner.shelf.reap_and_emit(self);
        Some(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("test.db")).expect("test database");
        (Arc::new(db), dir)
    }

    fn agent_spawn() -> SpawnRequest {
        SpawnRequest {
            kind: SessionKind::Agent,
            cli: "cat".to_string(),
            model: None,
            workdir: "/tmp".to_string(),
            name: None,
            prompt: None,
            interactive_args: None,
            fallback_args: None,
            model_flag: None,
            id: None,
            harness_conversation_id: None,
            accent: None,
            yolo: false,
            preserve_args: false,
        }
    }

    /// A keystroke parked behind the writer lock must already count as
    /// composing: `prompt_if_idle` checks under that same lock, so observing
    /// after writing would let a delivery slip through ahead of the parked
    /// keystroke and mix with the user's line.
    #[test]
    fn parked_keystroke_counts_as_composing_before_reaching_the_pty() {
        let (db, _dir) = test_db();
        let hub =
            SessionHub::with_broadcast_capacity(Arc::clone(&db), "test-host".to_string(), 256);
        let session = hub.spawn(&agent_spawn()).expect("spawn cat session");
        assert!(!session.user_is_composing());

        // Hold the writer so a concurrent keystroke parks before writing.
        let writer_guard = session.writer.lock().expect("writer lock");
        let parked = Arc::clone(&session);
        let handle = std::thread::spawn(move || parked.write_to_pty(b"x"));
        // The parked keystroke is observed before it reaches the PTY.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !session.user_is_composing() {
            assert!(
                std::time::Instant::now() < deadline,
                "parked keystroke never marked the session composing"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(writer_guard);
        handle
            .join()
            .expect("keystroke thread")
            .expect("write_to_pty");
        assert!(session.user_is_composing());
        let _ = hub.kill(&session.id);
    }
}
