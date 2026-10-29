//! Interactive agent management — hub sessions with a local vt100 screen.
//!
//! Each agent is a daemon-hub session the TUI attached to: the hub owns the
//! PTY, and a background thread feeds hub output frames into a local
//! `vt100::Parser` which maintains a virtual screen buffer. The UI reads
//! this screen buffer and renders it as ratatui cells inside the right
//! panel — fully embedded, with colors and cursor.

use anyhow::Result;
use chrono::{DateTime, Utc};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ratatui::style::Color;

use crate::domain::models::Cli;
use crate::sessions::client::{
    new_req, proto_mismatch_message, with_active, SessionEvent, SessionOpenError, SessionSend,
    SessionStream,
};
use crate::sessions::protocol::{
    list_frame, spawn_frame, spawn_frame_full, HostFrame, SessionItem, SpawnExt, SpawnFrameArgs,
};
use crate::sessions::{format_accent_rgb, preserve_rows_on_shrink, VT_SCROLLBACK_LINES};

pub mod input;
pub mod naming;
pub mod pty;
pub mod sanitize;
pub mod screen;

/// Status of an interactive agent.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Running,
    Exited(i32),
}

/// A recorded user prompt with its response range in scrollback.
#[derive(Clone)]
#[allow(dead_code)]
pub struct PromptEntry {
    pub input: String,
    /// (start_line, end_line) in the vt100 scrollback buffer
    /// representing the agent's response to this prompt.
    pub output_range: (usize, usize),
    pub timestamp: DateTime<Utc>,
}

/// Maximum number of prompt entries to keep in the ring buffer.
const MAX_PROMPT_HISTORY: usize = 20;

/// `exit_code` while no `exited` frame has arrived.
const EXIT_UNKNOWN: i32 = i32::MIN;

/// How recently a session must have produced PTY output to count as
/// "working" (blinking green) rather than merely "healthy but idle" (solid
/// green) in the status color. Blue is reserved for background agents; see
/// `session_status_color` in `ui/sidebar.rs`. Kept short enough that the
/// color reacts within one interaction, long enough to survive brief pauses
/// between output chunks so a steadily streaming session doesn't flicker.
pub(crate) const ACTIVITY_IDLE_THRESHOLD_MS: i64 = 12_000;

/// vt100 callbacks that mirror a PTY program's OSC 52 clipboard writes to the
/// host system clipboard. Without this the parser silently drops OSC 52, so a
/// harness (Claude Code, opencode, …) reports "copied" but nothing reaches the
/// real clipboard.
#[derive(Default)]
pub(crate) struct ClipboardForwarder;

impl vt100::Callbacks for ClipboardForwarder {
    fn copy_to_clipboard(&mut self, _: &mut vt100::Screen, _ty: &[u8], data: &[u8]) {
        let Some(text) = decode_osc52_payload(data) else {
            return;
        };
        // Set off-thread so clipboard I/O never blocks the held vt parser lock.
        std::thread::spawn(move || crate::tui::clipboard::set_text(&text));
    }
}

/// Decode an OSC 52 base64 payload into UTF-8 text, tolerating missing padding.
fn decode_osc52_payload(data: &[u8]) -> Option<String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data))
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// vt100 parser wired with our clipboard-forwarding callbacks.
pub(crate) type Vt = vt100::Parser<ClipboardForwarder>;

/// An interactive agent with a virtual terminal screen.
pub struct InteractiveAgent {
    /// UUID-based permanent identifier
    pub id: String,
    /// Display name for personality (from RANDOM_NAMES)
    pub name: String,
    /// Seed identity ID (if bound to a seed)
    #[allow(dead_code)]
    pub seed_id: Option<String>,
    /// Seed display name (resolved from seed_id, shown in TUI instead of random name)
    pub seed_name: Option<String>,
    pub cli: Cli,
    #[allow(dead_code)]
    pub working_dir: String,
    #[allow(dead_code)]
    pub started_at: DateTime<Utc>,
    pub status: AgentStatus,
    /// Accent color for this agent's TUI elements (from `CliConfig`).
    pub accent_color: Color,
    /// Whether this is a raw terminal session (no AI CLI).
    #[allow(dead_code)]
    pub is_terminal: bool,
    /// Shell binary for terminal sessions (e.g. "zsh", "bash").
    pub shell: String,
    /// The hub stream: writes go out as frames, output arrives as events.
    pub(crate) stream: SessionSend,
    /// Virtual terminal screen — fed by hub output (for live rendering with colors).
    pub(crate) vt: Arc<Mutex<Vt>>,
    /// The hub child's OS process id, from hub metadata.
    pid: Option<i64>,
    /// Last output `seq` applied; frames at or below it are duplicates.
    last_seq: Arc<AtomicU64>,
    /// Bumped when a reattach replaces the stream. A reader from an older
    /// generation stops feeding the parser and stops touching `connected`,
    /// so a stale socket can neither garble the fresh snapshot nor mark the
    /// live connection disconnected.
    stream_gen: Arc<AtomicU64>,
    /// Exit code from the `exited` frame; `EXIT_UNKNOWN` while running.
    exit_code: Arc<AtomicI32>,
    /// False once the stream drops; the session keeps running in the hub.
    connected: Arc<AtomicBool>,
    /// Protocol mismatch observed while attaching, if any.
    pub(crate) proto_error: Option<String>,
    /// Scroll offset (0 = bottom/live, positive = scrolled up).
    pub scroll_offset: usize,
    /// Last known PTY dimensions (for resize detection).
    pub last_pty_cols: u16,
    pub last_pty_rows: u16,
    /// Ring buffer of recent user prompts and their responses.
    pub prompt_history: Arc<Mutex<VecDeque<PromptEntry>>>,
    /// Current accumulated input (characters since last Enter).
    pub input_buffer: Arc<Mutex<String>>,
    /// Tracks when the PTY last received output (for detecting idle/waiting state).
    pub(crate) last_output_at: Arc<Mutex<DateTime<Utc>>>,
    /// Output arriving before this instant does not count as activity (B21):
    /// a PTY resize (entering a section, layout change) makes every TUI app
    /// repaint, and that repaint burst must not light up the activity pulse.
    pub(crate) activity_suppressed_until: Arc<Mutex<DateTime<Utc>>>,
    /// Tracks when the user last viewed/focused this agent.
    last_viewed_at: Arc<Mutex<DateTime<Utc>>>,
    /// Whether the exit notification has already been sent (avoids repeats).
    pub exit_notified: bool,
    /// Warp-like input mode: accumulate keystrokes in input_buffer, send on Enter.
    /// Only used for terminal sessions (is_terminal == true).
    pub warp_mode: bool,
    /// Cursor position within the warp input buffer (byte offset).
    pub warp_cursor: usize,
    /// Index into session history for Up/Down browsing (None = not browsing).
    pub history_index: Option<usize>,
    /// True once the current shell line has been materialized in the PTY
    /// and warp input should stay synchronized from PTY edits.
    pub warp_passthrough: bool,
    /// Flags from the most recent Kitty keyboard protocol "push" the child
    /// sent (`CSI > flags u`), if any — `None` until one is observed.
    /// Populated by a raw scan of PTY output in the reader thread (vt100
    /// 0.16 has no Kitty-protocol support of its own — see
    /// `input::parse_kitty_keyboard_push`), independent of the vt100
    /// parser's own state. Exposed via
    /// [`crate::tui::agent::InteractiveAgent::kitty_keyboard_negotiated`],
    /// consulted by `event::agent_focus::focused_child_claimed_keyboard` to
    /// decide whether a focus shortcut should yield to the child; not
    /// consulted by any scroll encoder (see the `MPM::None` arm of
    /// `encode_scroll_sequence` in `input.rs` for why Page Up/Page Down
    /// specifically have no distinct Kitty encoding to switch to).
    pub(crate) kitty_keyboard_flags: Arc<Mutex<Option<u8>>>,
    /// Test tap: when set, `write_to_pty` also appends there, so key
    /// routing tests observe writes without a harness round-trip.
    #[cfg(test)]
    pub test_sink: Option<Arc<Mutex<Vec<u8>>>>,
}

impl InteractiveAgent {
    /// Spawn a new interactive agent as a hub session and attach to it.
    ///
    /// `cols` and `rows` should match the panel area where the agent will render.
    /// `interactive_args` (resume/yolo included), `fallback_args`, `model_flag`,
    /// `id` (resume identity), `seed_id`, the harness conversation id and the
    /// accent travel in the spawn frame so the hub builds the same child the
    /// TUI built before and records the conversation on the session row.
    /// `name` is an optional user-provided session name (random if None).
    /// `existing_ids` is used to avoid name collisions.
    /// `model` selects the model (e.g. `gpt-4`).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        cli: Cli,
        working_dir: &str,
        cols: u16,
        rows: u16,
        interactive_args: Option<&str>,
        fallback_args: Option<&str>,
        accent_color: Color,
        name: Option<&str>,
        id: Option<&str>,
        existing_ids: &[&str],
        model: Option<&str>,
        model_flag: Option<&str>,
        seed_id: Option<&str>,
        harness_conversation_id: Option<&str>,
    ) -> Result<Self> {
        let accent = match accent_color {
            Color::Rgb(r, g, b) => Some(format_accent_rgb((r, g, b))),
            _ => None,
        };
        let name = name
            .map(str::to_owned)
            .unwrap_or_else(|| naming::pick_random_name(existing_ids));
        let item = spawn_on_hub(&HubSpawnArgs {
            kind: "agent",
            cli: cli.as_str(),
            model,
            workdir: working_dir,
            name: &name,
            interactive_args,
            fallback_args,
            model_flag,
            id,
            seed_id,
            accent: accent.as_deref(),
            harness_conversation_id,
        })?;
        Self::attach_item(&item, cli, accent_color, seed_id, cols, rows)
    }

    /// Spawn a raw terminal session (no AI CLI model) and attach to it.
    ///
    /// Uses `shell` as the command (e.g. `"bash"`, `"zsh"`).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_terminal(
        shell: &str,
        working_dir: &str,
        cols: u16,
        rows: u16,
        name: Option<&str>,
        existing_ids: &[&str],
        accent_color: Color,
    ) -> Result<Self> {
        let name = name
            .map(str::to_owned)
            .unwrap_or_else(|| naming::pick_terminal_name(existing_ids));
        let item = spawn_on_hub_terminal("terminal", shell, working_dir, &name)?;
        Self::attach_item(&item, Cli::new(shell), accent_color, None, cols, rows)
    }

    /// Attach to a hub session: open its stream, take its snapshot, size it.
    pub(crate) fn attach_item(
        item: &SessionItem,
        cli: Cli,
        accent_color: Color,
        seed_id: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let endpoint = with_active(|active| (active.url.clone(), active.token.clone()))
            .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))?;
        let (stream, rx) = SessionStream::open(&endpoint.0, &endpoint.1, &item.id)
            .map_err(|e| anyhow::anyhow!("cannot attach to session '{}': {e}", item.id))?;
        let mut agent = Self::from_item(item, cli, accent_color, seed_id, stream);
        agent.wait_for_snapshot(&rx)?;
        agent.spawn_stream_reader(rx);
        agent.resize(cols, rows);
        Ok(agent)
    }

    /// Reopen the stream after a hub reconnect: a fresh snapshot rebuilds the
    /// parser, a new reader continues from its `seq`, and writes flow again.
    pub(crate) fn reattach(&mut self) -> Result<()> {
        let endpoint = with_active(|active| (active.url.clone(), active.token.clone()))
            .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))?;
        let opened = SessionStream::open(&endpoint.0, &endpoint.1, &self.id);
        let (stream, rx) = match opened {
            Ok(opened) => opened,
            Err(SessionOpenError::Proto(proto)) => {
                let message = proto_mismatch_message(proto);
                self.proto_error = Some(message.clone());
                anyhow::bail!("cannot reattach to session '{}': {message}", self.id);
            }
            Err(e) => anyhow::bail!("cannot reattach to session '{}': {e}", self.id),
        };
        // Retire the old reader before its socket goes away: it must not feed
        // the parser the snapshot is about to replace, and its shutdown must
        // not mark this new connection disconnected.
        self.stream_gen.fetch_add(1, Ordering::SeqCst);
        self.wait_for_snapshot(&rx)?;
        self.stream = stream;
        self.connected.store(true, Ordering::Relaxed);
        self.proto_error = None;
        self.spawn_stream_reader(rx);
        // Reassert our size: the hub kept its own while we were gone.
        let (cols, rows) = (self.last_pty_cols, self.last_pty_rows);
        self.resize(cols, rows);
        Ok(())
    }

    /// Build the agent shell around an open stream; the snapshot wait and
    /// the reader come right after (see `attach_item`).
    fn from_item(
        item: &SessionItem,
        cli: Cli,
        accent_color: Color,
        seed_id: Option<&str>,
        stream: SessionSend,
    ) -> Self {
        let (seed_id_owned, seed_name) = match seed_id {
            Some(sid) => {
                let resolved = crate::domain::seeds::load_seed(sid)
                    .ok()
                    .map(|identity| identity.name);
                (Some(sid.to_string()), resolved)
            }
            None => (None, None),
        };
        let is_terminal = item.kind == "terminal";
        let status = match item.status.as_str() {
            "exited" => AgentStatus::Exited(item.exit_code.unwrap_or(-1)),
            _ => AgentStatus::Running,
        };
        Self {
            id: item.id.clone(),
            name: item.name.clone(),
            seed_id: seed_id_owned,
            seed_name,
            cli,
            working_dir: item.workdir.clone(),
            started_at: parse_hub_time(&item.started_at),
            status,
            accent_color,
            is_terminal,
            shell: if is_terminal {
                item.cli.clone()
            } else {
                String::new()
            },
            stream,
            vt: Arc::new(Mutex::new(Vt::new_with_callbacks(
                item.rows,
                item.cols,
                VT_SCROLLBACK_LINES,
                ClipboardForwarder,
            ))),
            pid: item.pid,
            last_seq: Arc::new(AtomicU64::new(0)),
            stream_gen: Arc::new(AtomicU64::new(0)),
            exit_code: Arc::new(AtomicI32::new(EXIT_UNKNOWN)),
            connected: Arc::new(AtomicBool::new(true)),
            proto_error: None,
            scroll_offset: 0,
            last_pty_cols: item.cols,
            last_pty_rows: item.rows,
            prompt_history: Arc::new(Mutex::new(VecDeque::with_capacity(MAX_PROMPT_HISTORY))),
            input_buffer: Arc::new(Mutex::new(String::new())),
            last_output_at: Arc::new(Mutex::new(parse_hub_time(&item.last_output_at))),
            activity_suppressed_until: Arc::new(Mutex::new(Utc::now())),
            last_viewed_at: Arc::new(Mutex::new(Utc::now())),
            exit_notified: false,
            warp_mode: is_terminal,
            warp_cursor: 0,
            history_index: None,
            warp_passthrough: false,
            kitty_keyboard_flags: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            test_sink: None,
        }
    }

    /// Wait up to 5 s for the attach snapshot and rebuild the parser from it.
    fn wait_for_snapshot(&self, rx: &std::sync::mpsc::Receiver<SessionEvent>) -> Result<()> {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(SessionEvent::Snapshot {
                cols,
                rows,
                seq,
                data,
            }) => {
                self.apply_snapshot(cols, rows, seq, &data);
                Ok(())
            }
            Ok(_) => Err(anyhow::anyhow!(
                "attach to session '{}' was refused",
                self.id
            )),
            Err(_) => Err(anyhow::anyhow!(
                "timed out waiting for session '{}' snapshot",
                self.id
            )),
        }
    }

    /// Rebuild the parser from a snapshot: a fresh parser at the snapshot
    /// size fed the snapshot bytes, with `seq` becoming the new floor.
    /// Snapshots carry history, so `last_output_at` is left alone.
    pub(crate) fn apply_snapshot(&self, cols: u16, rows: u16, seq: u64, data: &[u8]) {
        Self::rebuild_parser(
            &self.vt,
            &self.kitty_keyboard_flags,
            &self.last_seq,
            cols,
            rows,
            seq,
            data,
        );
    }

    fn rebuild_parser(
        vt: &Mutex<Vt>,
        kitty_keyboard_flags: &Mutex<Option<u8>>,
        last_seq: &AtomicU64,
        cols: u16,
        rows: u16,
        seq: u64,
        data: &[u8],
    ) {
        if let Ok(mut parser) = vt.lock() {
            *parser = Vt::new_with_callbacks(
                rows.max(1),
                cols.max(1),
                VT_SCROLLBACK_LINES,
                ClipboardForwarder,
            );
            parser.process(data);
        }
        if let Some(flags) = input::parse_kitty_keyboard_push(data) {
            if let Ok(mut current) = kitty_keyboard_flags.lock() {
                *current = Some(flags);
            }
        }
        last_seq.store(seq, Ordering::Relaxed);
    }

    /// Apply one `out` frame; returns false when `seq` is not newer than
    /// the last applied and the frame was dropped as a duplicate.
    /// Test-only: the stream reader calls [`Self::ingest_out`] directly.
    #[cfg(test)]
    pub(crate) fn apply_out_frame(&self, seq: u64, data: &[u8]) -> bool {
        Self::ingest_out(
            &self.vt,
            &self.kitty_keyboard_flags,
            &self.last_output_at,
            &self.activity_suppressed_until,
            &self.last_seq,
            seq,
            data,
        )
    }

    fn ingest_out(
        vt: &Mutex<Vt>,
        kitty_keyboard_flags: &Mutex<Option<u8>>,
        last_output_at: &Mutex<DateTime<Utc>>,
        activity_suppressed_until: &Mutex<DateTime<Utc>>,
        last_seq: &AtomicU64,
        seq: u64,
        data: &[u8],
    ) -> bool {
        if seq <= last_seq.load(Ordering::Relaxed) {
            return false;
        }
        Self::process_pty_chunk(
            data,
            vt,
            kitty_keyboard_flags,
            last_output_at,
            activity_suppressed_until,
        );
        last_seq.store(seq, Ordering::Relaxed);
        true
    }

    /// Process one chunk of PTY bytes: feed the vt100 parser, record any Kitty
    /// keyboard push, and stamp `last_output_at` unless the output falls inside
    /// a post-resize suppression window (a repaint, not real activity — B21).
    fn process_pty_chunk(
        chunk: &[u8],
        vt: &Mutex<Vt>,
        kitty_keyboard_flags: &Mutex<Option<u8>>,
        last_output_at: &Mutex<DateTime<Utc>>,
        activity_suppressed_until: &Mutex<DateTime<Utc>>,
    ) {
        if let Ok(mut parser) = vt.lock() {
            parser.process(chunk);
        }
        if let Some(flags) = input::parse_kitty_keyboard_push(chunk) {
            if let Ok(mut f) = kitty_keyboard_flags.lock() {
                *f = Some(flags);
            }
        }
        let suppressed = activity_suppressed_until
            .lock()
            .map(|until| Utc::now() < *until)
            .unwrap_or(false);
        if !suppressed {
            if let Ok(mut t) = last_output_at.lock() {
                *t = Utc::now();
            }
        }
    }

    /// Spawn the background stream reader: `out` frames newer than the
    /// last applied go through [`Self::process_pty_chunk`], a lag-resync
    /// snapshot rebuilds the parser, `exited` records the code for
    /// [`Self::poll`], and `Closed` marks the agent disconnected.
    fn spawn_stream_reader(&self, rx: std::sync::mpsc::Receiver<SessionEvent>) {
        let vt = Arc::clone(&self.vt);
        let kitty_keyboard_flags = Arc::clone(&self.kitty_keyboard_flags);
        let last_output_at = Arc::clone(&self.last_output_at);
        let activity_suppressed_until = Arc::clone(&self.activity_suppressed_until);
        let last_seq = Arc::clone(&self.last_seq);
        let exit_code = Arc::clone(&self.exit_code);
        let connected = Arc::clone(&self.connected);
        let stream_gen = Arc::clone(&self.stream_gen);
        let my_gen = stream_gen.load(Ordering::SeqCst);
        std::thread::spawn(move || {
            for event in rx {
                // A reattach bumped the generation: this reader belongs to the
                // replaced socket, so it drops its frames and stops.
                if !reader_is_current(&stream_gen, my_gen) {
                    return;
                }
                match event {
                    SessionEvent::Snapshot {
                        cols,
                        rows,
                        seq,
                        data,
                    } => Self::rebuild_parser(
                        &vt,
                        &kitty_keyboard_flags,
                        &last_seq,
                        cols,
                        rows,
                        seq,
                        &data,
                    ),
                    SessionEvent::Out { seq, data } => {
                        Self::ingest_out(
                            &vt,
                            &kitty_keyboard_flags,
                            &last_output_at,
                            &activity_suppressed_until,
                            &last_seq,
                            seq,
                            &data,
                        );
                    }
                    SessionEvent::Exited { code } => {
                        exit_code.store(code, Ordering::Relaxed);
                    }
                    SessionEvent::Closed => {
                        mark_disconnected(&stream_gen, my_gen, &connected);
                        return;
                    }
                }
            }
            mark_disconnected(&stream_gen, my_gen, &connected);
        });
    }

    /// Mark the agent as having been viewed/attended by the user.
    /// This suppresses the waiting indicator until new output arrives.
    pub fn mark_viewed(&self) {
        if let Ok(mut t) = self.last_viewed_at.lock() {
            *t = Utc::now();
        }
    }

    /// The OS process id of the hub child, from hub metadata.
    pub fn pid(&self) -> Option<i64> {
        self.pid
    }

    /// Send raw bytes to the agent's stdin as an `input` frame.
    pub fn write_to_pty(&self, data: &[u8]) -> Result<()> {
        #[cfg(test)]
        if let Some(sink) = self.test_sink.as_ref() {
            if let Ok(mut sink) = sink.lock() {
                sink.extend_from_slice(data);
            }
        }
        self.stream.input(data);
        Ok(())
    }

    /// Fold an arrived `exited` frame into the status.
    pub fn poll(&mut self) {
        if self.status != AgentStatus::Running {
            return;
        }
        let code = self.exit_code.load(Ordering::Relaxed);
        if code != EXIT_UNKNOWN {
            self.status = AgentStatus::Exited(code);
        }
    }

    /// End the hub session with `kill`. Quitting the TUI never calls this —
    /// dropping the agent only closes its stream and the session runs on.
    pub fn kill(&mut self) {
        self.stream.kill();
        self.status = AgentStatus::Exited(-9);
    }

    /// Ask the agent to persist a final summary, then terminate it after a short delay.
    ///
    /// This enables "shadow summary" behavior: the UI can close immediately while
    /// the process briefly remains alive to run finalization instructions.
    pub fn schedule_shadow_shutdown(&self, final_instruction: &str, linger: Duration) {
        let _ = self.write_to_pty(final_instruction.as_bytes());
        let _ = self.write_to_pty(b"\n");

        let stream = self.stream.clone();
        std::thread::spawn(move || {
            std::thread::sleep(linger);
            stream.kill();
        });
    }

    /// False once the stream drops; the session keeps running in the hub.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Record an `exited` frame that arrived on another connection.
    pub(crate) fn note_exited(&self, code: i32) {
        self.exit_code.store(code, Ordering::Relaxed);
    }

    /// Mark the stream dropped (hub restart); cleared by `reattach`.
    pub(crate) fn note_disconnected(&self) {
        self.connected.store(false, Ordering::Relaxed);
    }

    /// Resize the hub session and the virtual terminal (e.g. on window resize).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let old_rows = self.last_pty_rows;
        self.last_pty_cols = cols;
        self.last_pty_rows = rows;
        // A resize makes the app repaint; that output burst is not activity.
        // Suppress activity stamping briefly so switching sections doesn't
        // light up every session's pulse (B21).
        if let Ok(mut until) = self.activity_suppressed_until.lock() {
            *until = Utc::now() + chrono::Duration::seconds(1);
        }
        // Resize the hub session so the child knows about the new size.
        self.stream.resize(cols, rows);
        // Resize the virtual terminal screen. On a shrink, first scroll the rows
        // that would otherwise be truncated into the scrollback — both steps run
        // under one `vt` acquisition so a stream reader can't slip output in
        // between the scroll and the resize.
        if let Ok(mut vt) = self.vt.lock() {
            preserve_rows_on_shrink(&mut vt, old_rows, rows);
            vt.screen_mut().set_size(rows, cols);
        }
    }

    /// Update the working directory. Used when CD command is executed.
    pub fn update_working_dir(&mut self, new_dir: &str) {
        self.working_dir = new_dir.to_string();
    }
}

/// Whether a stream reader still belongs to the agent's current socket.
fn reader_is_current(stream_gen: &AtomicU64, my_gen: u64) -> bool {
    stream_gen.load(Ordering::SeqCst) == my_gen
}

/// Clear `connected` only while this reader still owns the connection: a
/// reattach may already have replaced it, and its reader is live.
fn mark_disconnected(stream_gen: &AtomicU64, my_gen: u64, connected: &AtomicBool) {
    if reader_is_current(stream_gen, my_gen) {
        connected.store(false, Ordering::Relaxed);
    }
}

/// Launch fields for one hub spawn; a struct so the spawn helper stays
/// under the argument-count lint without silencing it.
struct HubSpawnArgs<'a> {
    kind: &'a str,
    cli: &'a str,
    model: Option<&'a str>,
    workdir: &'a str,
    name: &'a str,
    interactive_args: Option<&'a str>,
    fallback_args: Option<&'a str>,
    model_flag: Option<&'a str>,
    id: Option<&'a str>,
    seed_id: Option<&'a str>,
    accent: Option<&'a str>,
    harness_conversation_id: Option<&'a str>,
}

/// Spawn a hub session and return its metadata row: `spawn` answers the id,
/// then `list` finds the row the hub announced for it.
fn spawn_on_hub(args: &HubSpawnArgs<'_>) -> Result<SessionItem> {
    let ext = SpawnExt {
        interactive_args: args.interactive_args.map(str::to_owned),
        fallback_args: args.fallback_args.map(str::to_owned),
        model_flag: args.model_flag.map(str::to_owned),
        id: args.id.map(str::to_owned),
        seed_id: args.seed_id.map(str::to_owned),
        accent: args.accent.map(str::to_owned),
        harness_conversation_id: args.harness_conversation_id.map(str::to_owned),
        yolo: false,
    };
    let req = new_req();
    let frame_args = SpawnFrameArgs {
        req: &req,
        kind: args.kind,
        cli: args.cli,
        model: args.model,
        workdir: args.workdir,
        name: Some(args.name),
        prompt: None,
        ext: &ext,
    };
    let spawned = with_active(|active| active.client.request(&req, spawn_frame_full(&frame_args)))
        .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Spawned { id, .. } = spawned else {
        anyhow::bail!("hub answered spawn with an unexpected frame");
    };
    let req = new_req();
    let listed = with_active(|active| active.client.request(&req, list_frame(&req)))
        .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Sessions { items, .. } = listed else {
        anyhow::bail!("hub answered list with an unexpected frame");
    };
    items
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| anyhow::anyhow!("hub has no session '{id}' it just spawned"))
}

/// Terminal spawn through the short frame: terminals carry no agent launch
/// fields, so the 7-arg builder stays the non-test caller of `spawn_frame`.
fn spawn_on_hub_terminal(kind: &str, cli: &str, workdir: &str, name: &str) -> Result<SessionItem> {
    let req = new_req();
    let spawned = with_active(|active| {
        active.client.request(
            &req,
            spawn_frame(&req, kind, cli, None, workdir, Some(name), None),
        )
    })
    .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Spawned { id, .. } = spawned else {
        anyhow::bail!("hub answered spawn with an unexpected frame");
    };
    let req = new_req();
    let listed = with_active(|active| active.client.request(&req, list_frame(&req)))
        .ok_or_else(|| anyhow::anyhow!("not connected to the session hub"))??;
    let HostFrame::Sessions { items, .. } = listed else {
        anyhow::bail!("hub answered list with an unexpected frame");
    };
    items
        .into_iter()
        .find(|item| item.id == id)
        .ok_or_else(|| anyhow::anyhow!("hub has no session '{id}' it just spawned"))
}

/// Parse a hub timestamp; now when it does not parse.
fn parse_hub_time(raw: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

pub use pty::key_to_bytes;
pub use screen::ScreenSnapshot;

#[cfg(test)]
mod tests {
    use super::{
        decode_osc52_payload, preserve_rows_on_shrink, ClipboardForwarder, InteractiveAgent,
        VT_SCROLLBACK_LINES,
    };
    use base64::Engine as _;
    use chrono::Utc;

    /// A parser wired the way `InteractiveAgent` builds its `vt` (see the
    /// `new_with_callbacks` call in `InteractiveAgent::new`).
    fn agent_parser(rows: u16) -> vt100::Parser<ClipboardForwarder> {
        vt100::Parser::new_with_callbacks(rows, 80, super::VT_SCROLLBACK_LINES, ClipboardForwarder)
    }

    /// Number of rows currently held in the parser's scrollback deque.
    /// `Screen::scrollback()` reports the *scroll position*, so read it back at
    /// its clamped maximum, then restore the live view.
    fn scrollback_len(parser: &mut vt100::Parser<ClipboardForwarder>) -> usize {
        parser.screen_mut().set_scrollback(usize::MAX);
        let len = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(0);
        len
    }

    #[test]
    fn decodes_padded_osc52_payload() {
        let encoded = base64::engine::general_purpose::STANDARD.encode("hello world");
        assert_eq!(
            decode_osc52_payload(encoded.as_bytes()).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn decodes_unpadded_osc52_payload() {
        // Some emitters strip the `=` padding; we must still decode it.
        let unpadded = base64::engine::general_purpose::STANDARD_NO_PAD.encode("multi\nline");
        assert!(!unpadded.ends_with('='));
        assert_eq!(
            decode_osc52_payload(unpadded.as_bytes()).as_deref(),
            Some("multi\nline")
        );
    }

    #[test]
    fn rejects_non_base64_payload() {
        assert_eq!(decode_osc52_payload(b"!!! not base64 !!!"), None);
    }

    // T1: shrinking the virtual screen must move the rows that fall off into
    // scrollback instead of letting `set_size` truncate them.
    #[test]
    fn shrink_preserves_content_in_scrollback() {
        let mut parser = agent_parser(10);
        for i in 0..10 {
            parser.process(format!("line{i}\r\n").as_bytes());
        }
        // Writing the 10th line's newline already pushed `line0` back.
        assert_eq!(scrollback_len(&mut parser), 1);

        // The shrink routine, then the resize it guards.
        preserve_rows_on_shrink(&mut parser, 10, 7);
        parser.screen_mut().set_size(7, 80);

        // The 3 rows that left the visible screen joined `line0` in scrollback
        // rather than being lost: 1 + 3 = 4.
        assert_eq!(scrollback_len(&mut parser), 4);
        parser.screen_mut().set_scrollback(usize::MAX);
        let scrolled_back = parser.screen().contents();
        for line in ["line1", "line2", "line3"] {
            assert!(
                scrolled_back.contains(line),
                "{line} was dropped instead of scrolled back: {scrolled_back:?}"
            );
        }
        // The newest line was at the bottom of the screen and must survive.
        parser.screen_mut().set_scrollback(0);
        assert!(parser.screen().contents().contains("line9"));
    }

    // T2: the reported case — a shrink then a regrow (input box grows, then
    // collapses on submit). The last line written before the shrink must still
    // be on screen and the pushed-off rows must stay reachable by scrolling.
    #[test]
    fn shrink_then_regrow_preserves_last_line() {
        let mut parser = agent_parser(10);
        for i in 0..10 {
            parser.process(format!("line{i}\r\n").as_bytes());
        }

        preserve_rows_on_shrink(&mut parser, 10, 7);
        parser.screen_mut().set_size(7, 80);
        preserve_rows_on_shrink(&mut parser, 7, 10); // regrow: no-op
        parser.screen_mut().set_size(10, 80);

        let contents = parser.screen().contents();
        assert!(
            contents.contains("line9"),
            "last pre-shrink line was destroyed by the shrink: {contents:?}"
        );
        assert!(
            contents
                .lines()
                .next_back()
                .is_some_and(|l| !l.trim().is_empty()),
            "screen bottom collapsed to a band of blanks: {contents:?}"
        );
        // The rows the shrink pushed off are still in scrollback.
        parser.screen_mut().set_scrollback(usize::MAX);
        assert!(parser.screen().contents().contains("line1"));
    }

    // T3: a grow-only resize must not feed any scroll sequence to the parser.
    #[test]
    fn grow_only_resize_emits_no_scroll() {
        let mut parser = agent_parser(10);
        for i in 0..10 {
            parser.process(format!("line{i}\r\n").as_bytes());
        }
        let before = scrollback_len(&mut parser);

        preserve_rows_on_shrink(&mut parser, 10, 15);
        parser.screen_mut().set_size(15, 80);

        assert_eq!(
            scrollback_len(&mut parser),
            before,
            "a grow moved rows into scrollback"
        );
    }

    #[test]
    fn ct15_ordinary_lines_survive_el_then_exit() {
        let mut parser = agent_parser(24);
        for i in 1..=5 {
            parser.process(format!("ordinary line {i}\r\n").as_bytes());
        }
        // The installer's progress bar: every frame carriage-returns,
        // erases the previous frame with ESC[K, and writes the new one with
        // no newline. Frame 2's EL erases frame 1 — a legitimate request
        // that must keep working. The final frame stays on screen, exactly
        // as in a system terminal (only a later EL could remove it).
        parser.process(b"\r\x1b[K  progress 10%\r");
        parser.process(b"\x1b[K  progress 42%\r\n");
        parser.process(b"error: boom\r\nexit status 1\r\n$ ");

        let contents = parser.screen().contents();
        for line in [
            "ordinary line 1",
            "ordinary line 2",
            "ordinary line 3",
            "ordinary line 4",
            "ordinary line 5",
            "error: boom",
            "exit status 1",
        ] {
            assert!(
                contents.contains(line),
                "output was lost: {line:?}: {contents:?}"
            );
        }
        // The program deliberately erased frame 1 — it stays erased. The
        // fix restores what canopy destroyed, never what the program
        // deliberately overwrote. The final frame remains, as it would in
        // a system terminal.
        assert!(
            !contents.contains("progress 10%"),
            "ESC[K must keep erasing what the program overwrote: {contents:?}"
        );
        assert!(
            contents.contains("progress 42%"),
            "the final progress frame must stay visible like in a system terminal: {contents:?}"
        );
    }

    #[test]
    fn ct15_shrink_of_mostly_blank_screen_preserves_newest() {
        let mut parser = agent_parser(21);
        for i in 0..10 {
            parser.process(format!("real line {i}\r\n").as_bytes());
        }
        parser.process(b"$ ");
        preserve_rows_on_shrink(&mut parser, 21, 8);
        parser.screen_mut().set_size(8, 80);

        assert!(parser.screen().contents().contains("real line 9"));
        parser.screen_mut().set_scrollback(usize::MAX);
        assert!(parser.screen().contents().contains("real line 0"));
    }

    #[test]
    fn ct15_shrink_that_still_fits_scrolls_nothing() {
        let mut parser = agent_parser(21);
        for i in 0..10 {
            parser.process(format!("real line {i}\r\n").as_bytes());
        }
        parser.process(b"$ ");
        preserve_rows_on_shrink(&mut parser, 21, 16);
        parser.screen_mut().set_size(16, 80);
        assert_eq!(scrollback_len(&mut parser), 0);
        let live = parser.screen().contents();
        assert!(live.contains("real line 9") && live.contains("real line 0"));
    }

    // ── process_pty_chunk tests ────────────────────────────────────────
    // The function is private but has three distinct branches that must be
    // covered: (1) kitty keyboard push detection, (2) activity suppression
    // window (output inside window does not update last_output_at),
    // (3) normal activity stamping (output outside window updates last_output_at).

    #[test]
    fn process_pty_chunk_feeds_vt_parser() {
        use crate::tui::agent::ClipboardForwarder;
        use std::sync::{Arc, Mutex};
        use vt100::Parser;

        let vt = Arc::new(Mutex::new(Parser::new_with_callbacks(
            24,
            80,
            VT_SCROLLBACK_LINES,
            ClipboardForwarder,
        )));
        let kitty_flags = Arc::new(Mutex::new(None));
        let last_output = Arc::new(Mutex::new(Utc::now()));
        let suppressed_until = Arc::new(Mutex::new(Utc::now()));

        // Feed some plain text - should go to parser
        InteractiveAgent::process_pty_chunk(
            b"hello\r\n",
            &vt,
            &kitty_flags,
            &last_output,
            &suppressed_until,
        );

        let parser = vt.lock().unwrap();
        let contents = parser.screen().contents();
        assert!(contents.contains("hello"));
    }

    #[test]
    fn process_pty_chunk_detects_kitty_keyboard_push() {
        use crate::tui::agent::ClipboardForwarder;
        use std::sync::{Arc, Mutex};
        use vt100::Parser;

        let vt = Arc::new(Mutex::new(Parser::new_with_callbacks(
            24,
            80,
            VT_SCROLLBACK_LINES,
            ClipboardForwarder,
        )));
        let kitty_flags = Arc::new(Mutex::new(None));
        let last_output = Arc::new(Mutex::new(Utc::now()));
        let suppressed_until = Arc::new(Mutex::new(Utc::now()));

        // CSI > 1 u  — Kitty keyboard protocol push with flags=1
        InteractiveAgent::process_pty_chunk(
            b"\x1b[>1u",
            &vt,
            &kitty_flags,
            &last_output,
            &suppressed_until,
        );

        let flags = kitty_flags.lock().unwrap();
        assert_eq!(*flags, Some(1));
    }

    #[test]
    fn process_pty_chunk_respects_activity_suppression() {
        use crate::tui::agent::ClipboardForwarder;
        use chrono::Utc;
        use std::sync::{Arc, Mutex};
        use vt100::Parser;

        let vt = Arc::new(Mutex::new(Parser::new_with_callbacks(
            24,
            80,
            VT_SCROLLBACK_LINES,
            ClipboardForwarder,
        )));
        let kitty_flags = Arc::new(Mutex::new(None));
        let last_output = Arc::new(Mutex::new(Utc::now()));
        // Suppression window in the future — output now should be suppressed
        let suppressed_until = Arc::new(Mutex::new(Utc::now() + chrono::Duration::seconds(10)));

        let before = *last_output.lock().unwrap();
        InteractiveAgent::process_pty_chunk(
            b"output\r\n",
            &vt,
            &kitty_flags,
            &last_output,
            &suppressed_until,
        );

        // last_output_at should NOT be updated because we're inside the suppression window
        let after = *last_output.lock().unwrap();
        assert_eq!(after, before);
    }

    #[test]
    fn process_pty_chunk_updates_last_output_when_not_suppressed() {
        use crate::tui::agent::ClipboardForwarder;
        use chrono::Utc;
        use std::sync::{Arc, Mutex};
        use vt100::Parser;

        let vt = Arc::new(Mutex::new(Parser::new_with_callbacks(
            24,
            80,
            VT_SCROLLBACK_LINES,
            ClipboardForwarder,
        )));
        let kitty_flags = Arc::new(Mutex::new(None));
        let last_output = Arc::new(Mutex::new(Utc::now()));
        // Suppression window in the past — output now should update last_output_at
        let suppressed_until = Arc::new(Mutex::new(Utc::now() - chrono::Duration::seconds(10)));

        let before = *last_output.lock().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        InteractiveAgent::process_pty_chunk(
            b"output\r\n",
            &vt,
            &kitty_flags,
            &last_output,
            &suppressed_until,
        );

        let after = *last_output.lock().unwrap();
        assert!(after > before);
    }

    // ── inline-TUI scroll-region scrollback ─────────────────────────────
    // Reproduces Muse Code's pattern: pin an input box at the bottom with
    // a top-anchored scroll region (1..N-2), write a numbered line at the
    // region's bottom, newline, reset the region, repeat.

    /// Byte stream mimicking an inline TUI: per line, set region
    /// `top..=bottom` (1-based, as the terminal sees them), move to the
    /// region's bottom row, write `line {i}`, newline, reset the region.
    fn muse_stream(rows: u16, count: usize, top: u16, bottom: u16) -> Vec<u8> {
        let mut stream = Vec::new();
        for i in 1..=count {
            stream.extend_from_slice(format!("\x1b[{top};{bottom}r").as_bytes());
            stream.extend_from_slice(format!("\x1b[{bottom};1H").as_bytes());
            stream.extend_from_slice(format!("line {i}").as_bytes());
            stream.extend_from_slice(b"\r\n");
            stream.extend_from_slice(b"\x1b[r");
        }
        let _ = rows;
        stream
    }

    /// Every numbered line must be reachable by paging from the newest row
    /// back to the oldest. Pre-patch the region scrolls saved nothing.
    #[test]
    fn inline_tui_region_top_saves_every_line() {
        use std::collections::HashSet;
        let rows = 10u16;
        let count = 3 * rows as usize;
        let mut parser = agent_parser(rows);
        parser.process(&muse_stream(rows, count, 1, rows - 2));
        let max = scrollback_len(&mut parser);
        assert!(max > 0, "top-anchored region scroll saved nothing");
        let mut found = HashSet::new();
        let mut s = max;
        loop {
            parser.screen_mut().set_scrollback(s);
            for line in parser.screen().contents().lines() {
                found.insert(line.trim().to_string());
            }
            if s == 0 {
                break;
            }
            s = s.saturating_sub(rows as usize);
        }
        parser.screen_mut().set_scrollback(0);
        for i in 1..=count {
            assert!(
                found.contains(&format!("line {i}")),
                "line {i} unreachable by scrolling (checked {max} back)"
            );
        }
    }

    /// A region whose top is not row 0 keeps upstream behaviour: nothing
    /// is saved, matching xterm.
    #[test]
    fn region_not_top_anchored_saves_nothing() {
        let rows = 10u16;
        let mut parser = agent_parser(rows);
        parser.process(&muse_stream(rows, 3 * rows as usize, 2, rows - 2));
        assert_eq!(scrollback_len(&mut parser), 0);
    }

    /// A full-screen scroll with no region still saves as before.
    #[test]
    fn plain_scroll_no_region_still_saves() {
        let rows = 10u16;
        let count = 3 * rows as usize;
        let mut parser = agent_parser(rows);
        for i in 1..=count {
            parser.process(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(scrollback_len(&mut parser), count - rows as usize + 1);
    }
}
