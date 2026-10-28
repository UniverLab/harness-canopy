//! `SessionHub`: daemon-owned PTY sessions.
//!
//! Each hub session owns its PTY master, child process, writer and an
//! authoritative vt100 parser. A reader thread feeds PTY output into the
//! parser and publishes `(seq, chunk)` pairs on a broadcast channel;
//! WebSocket clients attach to sessions through [`crate::sessions::ws`] and
//! receive snapshots plus the chunks after them.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use portable_pty::{native_pty_system, PtySize};
use tokio::sync::broadcast;

use crate::db::session::SessionRegistration;
use crate::db::Database;
use crate::domain::models::Cli;

#[cfg(unix)]
use crate::tui::agent::pty::install_signal_handlers;

/// Send `SIGHUP` to the child process group the way closing a session in
/// the TUI does, so the shell and its children all observe the hangup.
#[cfg(unix)]
fn send_sighup_to_group(child: &mut dyn portable_pty::Child) {
    let Some(pid) = child.process_id().map(|pid| pid as i32) else {
        return;
    };
    let _ = crate::daemon::process::send_signal_to_group(pid, libc::SIGHUP);
}

use super::protocol::SessionItem;
use super::{preserve_rows_on_shrink, spawn_identity, write_submitted_prompt, VT_SCROLLBACK_LINES};

/// Divider written after replayed scrollback, shared with the TUI so a
/// restored terminal reads the same whoever replays it.
use crate::tui::agent::screen::{read_abs_range, HISTORY_REPLAY_MARKER};

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

/// How long `kill` waits for the child to exit before falling back.
const KILL_WAIT_POLLS: u32 = 100;
const KILL_WAIT_INTERVAL: Duration = Duration::from_millis(20);

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
    pub seed_id: Option<String>,
    /// Canopy accent as `"r,g,b"`.
    pub accent: Option<String>,
    /// Skip the harness's approval prompts: append its registry `yolo_flag`
    /// to the interactive args. Handlers refuse `yolo` for a terminal or a
    /// harness with no flag before the hub ever sees it.
    pub yolo: bool,
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
        let out = self.state.lock().ok()?;
        let rx = self.bcast.subscribe();
        let snapshot = snapshot_locked(&out);
        Some((snapshot, rx))
    }

    /// A fresh snapshot for a lagged subscriber: same bytes as [`Self::attach`]
    /// but no new subscription (the receiver keeps its position and the
    /// caller drops everything at or below the snapshot's `seq`).
    pub fn fresh_snapshot(&self) -> Option<Snapshot> {
        self.state.lock().ok().map(|out| snapshot_locked(&out))
    }

    /// Send raw bytes to the session's PTY stdin.
    pub fn write_to_pty(&self, data: &[u8]) -> anyhow::Result<()> {
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

    /// Resize the PTY and the parser through the same shrink-safe path the
    /// TUI uses: rows that would fall off go to scrollback first, and both
    /// steps run under one lock acquisition.
    pub fn resize(&self, cols: u16, rows: u16) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        if let Ok(mut out) = self.state.lock() {
            let old_rows = out.rows;
            preserve_rows_on_shrink(&mut out.parser, old_rows, rows);
            out.parser.screen_mut().set_size(rows, cols);
            out.cols = cols;
            out.rows = rows;
        }
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
    /// a session of the same kind ends.
    fn finish_db_row(&self, db: &Database) {
        match self.kind {
            SessionKind::Agent => {
                let code = self.status().exit_code().unwrap_or(-1);
                let _ = db.finish_interactive_session(&self.id, code);
            }
            SessionKind::Terminal => {
                let _ = db.finish_terminal_session(&self.id);
            }
        }
    }

    /// Plain-text visible screen, for tests asserting client parity.
    #[cfg(test)]
    pub(crate) fn test_contents(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .map(|out| out.parser.screen().contents())
    }

    /// Plain-text scrollback at maximum scroll position, for tests.
    #[cfg(test)]
    pub(crate) fn test_scrollback_contents(&self) -> Option<String> {
        self.state.lock().ok().map(|mut out| {
            out.parser.screen_mut().set_scrollback(usize::MAX);
            let contents = out.parser.screen().contents();
            out.parser.screen_mut().set_scrollback(0);
            contents
        })
    }

    /// All text reachable by paging from the oldest scrollback row to the
    /// live view, for tests asserting no shrink dropped a row. A single
    /// window (live view plus maximum scrollback) misses the middle rows
    /// whenever the scrollback outgrows the screen.
    #[cfg(test)]
    pub(crate) fn test_all_text(&self) -> Option<String> {
        self.state.lock().ok().map(|mut out| {
            let step = usize::from(out.rows).max(1);
            out.parser.screen_mut().set_scrollback(usize::MAX);
            let max = out.parser.screen().scrollback();
            let mut all = String::new();
            let mut position = max;
            loop {
                out.parser.screen_mut().set_scrollback(position);
                all.push_str(&out.parser.screen().contents());
                all.push('\n');
                if position == 0 {
                    break;
                }
                position = position.saturating_sub(step);
            }
            out.parser.screen_mut().set_scrollback(0);
            all
        })
    }

    /// Whether the child process is using alternate screen mode. A
    /// full-screen program's frame is not scrollback history, so shutdown
    /// capture skips a terminal still in it (see `save_terminal_histories`).
    pub fn in_alternate_screen(&self) -> bool {
        self.state
            .try_lock()
            .map(|out| out.parser.screen().alternate_screen())
            .unwrap_or(false)
    }

    /// Replay persisted plain-text scrollback into the session's parser, the
    /// way `InteractiveAgent::replay_scrollback_lines` restores a TUI
    /// terminal: each line as output with CRLF separators, then a dim marker
    /// line so the operator sees where the replay ends and live output
    /// begins.
    ///
    /// `last_output_at` is deliberately left alone: a replay is history
    /// reconstruction, not fresh PTY output, and stamping it would make a
    /// restored session look actively working.
    pub fn replay_scrollback_lines(&self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        let Ok(mut out) = self.state.lock() else {
            return;
        };
        let mut replay = Vec::new();
        for line in lines {
            replay.extend_from_slice(line.as_bytes());
            replay.extend_from_slice(b"\r\n");
        }
        replay.extend_from_slice(HISTORY_REPLAY_MARKER);
        out.parser.process(&replay);
    }

    /// The last `n` lines of the whole buffer (scrollback + visible screen),
    /// as the text that gets persisted at shutdown.
    pub fn last_lines(&self, n: usize) -> String {
        if n == 0 {
            return String::new();
        }
        let Ok(mut out) = self.state.lock() else {
            return String::new();
        };
        let rows = usize::from(out.rows);
        if rows == 0 {
            return String::new();
        }
        out.parser.screen_mut().set_scrollback(usize::MAX);
        let max_sb = out.parser.screen().scrollback();
        let total_lines = max_sb + rows;
        let from_abs = total_lines.saturating_sub(n);
        let result = read_abs_range(&mut out.parser, max_sb, rows, from_abs, total_lines);
        out.parser.screen_mut().set_scrollback(0);
        result.join("\n")
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

/// Render a snapshot from already-locked output state: the scrollback, the
/// visible screen and the input modes, plus the `seq` they cover.
fn snapshot_locked(out: &OutputState) -> Snapshot {
    let screen = out.parser.screen();
    let mut data = screen.scrollback_formatted();
    data.extend(screen.contents_formatted());
    data.extend(screen.input_mode_formatted());
    Snapshot {
        seq: out.seq,
        cols: out.cols,
        rows: out.rows,
        data,
    }
}

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
            }),
        }
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
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            master: Mutex::new(master),
            bcast,
        });

        if let Ok(mut map) = self.inner.sessions.lock() {
            map.insert(id.clone(), Arc::clone(&session));
        }
        self.insert_db_row(req, &session, args.as_deref());
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
                    harness_conversation_id: None,
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
    /// finished, and tell attached connections. A no-op when `kill` already
    /// recorded the exit, so `exited` is sent once.
    pub(crate) fn record_natural_exit(&self, session: &HubSession, code: i32) {
        let Ok(mut status) = session.status.lock() else {
            return;
        };
        if *status != SessionStatus::Running {
            return;
        }
        *status = SessionStatus::Exited(code);
        drop(status);
        session.finish_db_row(&self.inner.db);
        self.emit(HubEvent::Exited {
            id: session.id.clone(),
            code,
            to_all: false,
        });
    }

    /// End a session the way closing it in the TUI does (signal to the
    /// process group), mark its row finished, remove it, and send `exited`
    /// plus `session_removed` to every connection. Returns the exit code, or
    /// `None` when there is no such session.
    pub fn kill(&self, id: &str) -> Option<i32> {
        let session = self.inner.sessions.lock().ok()?.remove(id)?;
        if let Ok(mut child) = session.child.lock() {
            #[cfg(unix)]
            send_sighup_to_group(child.as_mut());
            let _ = child.kill();
        }
        let code = match session.status() {
            SessionStatus::Exited(code) => code,
            SessionStatus::Running => {
                let code = wait_for_exit_code(&session).unwrap_or(-9);
                if let Ok(mut status) = session.status.lock() {
                    *status = SessionStatus::Exited(code);
                }
                code
            }
        };
        session.finish_db_row(&self.inner.db);
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

/// PTY reader thread: ingest chunks until EOF, then wait for the child to
/// exit and record the natural exit.
fn spawn_reader_thread(
    hub: SessionHub,
    session: Arc<HubSession>,
    mut reader: Box<dyn Read + Send>,
) {
    std::thread::spawn(move || {
        let mut tmp = [0u8; 4096];
        loop {
            match reader.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => session.ingest_chunk(&tmp[..n]),
            }
        }
        let code = loop {
            if let Some(code) = session.status().exit_code() {
                break code;
            }
            if let Some(code) = session.poll_exit_code() {
                break code;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        hub.record_natural_exit(&session, code);
    });
}

/// Poll the child for its exit code up to the kill grace; `None` if it is
/// somehow still alive afterwards.
fn wait_for_exit_code(session: &HubSession) -> Option<i32> {
    for _ in 0..KILL_WAIT_POLLS {
        if let Some(code) = session.poll_exit_code() {
            return Some(code);
        }
        std::thread::sleep(KILL_WAIT_INTERVAL);
    }
    session.poll_exit_code()
}
