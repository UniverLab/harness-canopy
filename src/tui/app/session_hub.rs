//! Daemon session-hub wiring: connect, attach-all, live dispatch, reconnect.
//!
//! The TUI owns no session: on start it connects to the hub, lists, and
//! attaches every live session; `session_added` / `session_removed` from
//! other clients update the sidebar; a dropped WebSocket marks the sessions
//! disconnected and reconnects with backoff, then re-attaches.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::application::ports::StateRepository;
use crate::domain::models::Cli;
use crate::sessions::client::{
    new_req, proto_mismatch_message, read_token, set_active, ws_url, HubClient, HubRecv,
};
use crate::sessions::protocol::{list_frame, HostFrame, SessionItem, PROTO_VERSION};
use crate::tui::agent::InteractiveAgent;

use super::types::App;

/// Initial reconnect delay; doubles per failed attempt up to `MAX_BACKOFF`.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const MAX_BACKOFF: Duration = Duration::from_secs(5);

/// The hub connection plus reconnect state. `Default` is disconnected and
/// idle: [`App::poll_session_hub`] only retries after an explicit connect
/// attempt (or a drop) armed `next_retry_at`, so `App::new` never dials.
pub(crate) struct SessionHubState {
    /// The live control connection, shared with the Shelf tab's listing
    /// and delete calls (`hub_shelf.rs`).
    pub(super) client: Option<HubClient>,
    url: String,
    token: String,
    next_retry_at: Option<Instant>,
    backoff: Duration,
}

impl Default for SessionHubState {
    fn default() -> Self {
        Self {
            client: None,
            url: String::new(),
            token: String::new(),
            next_retry_at: None,
            backoff: INITIAL_BACKOFF,
        }
    }
}

impl SessionHubState {
    /// True while the control connection is up. The slow tick reads daemon
    /// liveness from this instead of probing a TCP port every second.
    pub(crate) fn is_live(&self) -> bool {
        self.client.is_some()
    }

    /// Schedule the next reconnect attempt with the current backoff, then
    /// double it up to `MAX_BACKOFF`.
    fn arm_retry(&mut self) {
        self.next_retry_at = Some(Instant::now() + self.backoff);
        self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
    }

    fn retry_due(&self) -> bool {
        self.next_retry_at.is_some_and(|at| Instant::now() >= at)
    }

    /// Close the WebSocket without touching the sessions.
    fn close(&mut self) {
        if let Some(client) = self.client.take() {
            client.close();
        }
        self.next_retry_at = None;
    }
}

impl App {
    /// Connect to the daemon hub and attach every live session. Runs once
    /// at startup, after `App::new`.
    pub fn connect_session_hub(&mut self) {
        let token = match read_token(&self.data_dir) {
            Ok(token) => token,
            Err(e) => {
                tracing::warn!("Cannot read the daemon token: {e}");
                return;
            }
        };
        let url = ws_url(self.hub_port());
        self.connect_session_hub_to(&url, &token);
    }

    /// The daemon's session-hub port: the one the daemon recorded when it
    /// started (it may have been launched with `--port`, so the configured
    /// default is only the fallback every other daemon call uses too).
    pub(crate) fn hub_port(&self) -> u16 {
        self.db
            .get_state("port")
            .ok()
            .flatten()
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or_else(|| crate::resolve_port(None))
    }

    /// Core of [`App::connect_session_hub`] against an explicit endpoint
    /// (tests pass a fake hub here).
    pub(crate) fn connect_session_hub_to(&mut self, url: &str, token: &str) {
        self.session_hub.url = url.to_string();
        self.session_hub.token = token.to_string();
        match HubClient::connect(url, token) {
            Ok(client) => {
                if self.adopt_hub_client(client) {
                    self.attach_all_hub_sessions();
                } else {
                    // Only a daemon restart can fix a protocol mismatch; keep
                    // the notice up and retry so that restart heals this TUI
                    // instead of leaving it stuck until it is restarted too.
                    self.session_hub.arm_retry();
                }
            }
            Err(e) => {
                tracing::warn!("Cannot reach the session hub at {url}: {e}");
                self.session_hub.arm_retry();
            }
        }
    }

    /// Adopt a fresh control connection: check the protocol, share it with
    /// spawns, and reset the backoff. False on a protocol mismatch — the
    /// notice names the restart and nothing attaches.
    fn adopt_hub_client(&mut self, client: HubClient) -> bool {
        if client.proto() != PROTO_VERSION {
            self.hub_notice = Some(proto_mismatch_message(client.proto()));
            client.close();
            if let Some(previous) = self.session_hub.client.take() {
                previous.close();
            }
            // The notice names the restart; the sidebar rebuilds next tick.
            self.slow_force = true;
            return false;
        }
        set_active(
            self.session_hub.url.clone(),
            self.session_hub.token.clone(),
            client.clone(),
        );
        self.session_hub.client = Some(client);
        self.session_hub.backoff = INITIAL_BACKOFF;
        self.session_hub.next_retry_at = None;
        self.hub_notice = None;
        // Adopting clears the hub notice, so the sidebar rebuilds next tick.
        self.slow_force = true;
        true
    }

    /// Poll the hub connection: dispatch frames, reconnect with backoff.
    /// Runs first on every refresh tick. True when the poll produced a
    /// visible change that needs a redraw.
    pub(crate) fn poll_session_hub(&mut self) -> bool {
        if self.session_hub.client.is_none() {
            self.retry_hub_connect();
            // A reconnect that changed membership (or the hub notice) armed
            // a slow refresh; report it so this tick redraws with fresh data.
            // (`slow_force` never survives a tick — the slow refresh clears
            // it — so it can only have been set by this retry.)
            return self.slow_force;
        }
        self.dispatch_hub_frames()
    }

    /// Close the hub WebSocket without touching the sessions.
    pub(crate) fn close_session_hub(&mut self) {
        self.session_hub.close();
    }

    /// Drain one tick's worth of hub frames. True unless only `Unknown`
    /// frames arrived (or none did): anything else — membership, exits,
    /// shelf changes, errors — is a visible change.
    fn dispatch_hub_frames(&mut self) -> bool {
        let mut dirty = false;
        loop {
            let event = self
                .session_hub
                .client
                .as_ref()
                .and_then(|client| client.try_recv());
            let Some(event) = event else {
                return dirty;
            };
            match event {
                HubRecv::Frame(frame) => {
                    if !matches!(frame, HostFrame::Unknown) {
                        dirty = true;
                    }
                    self.handle_hub_frame(frame);
                }
                HubRecv::Closed => {
                    self.note_hub_dropped();
                    return true;
                }
            }
        }
    }

    fn handle_hub_frame(&mut self, frame: HostFrame) {
        match frame {
            HostFrame::SessionAdded { item } => self.attach_hub_item(&item),
            HostFrame::SessionRemoved { id } => self.remove_hub_session(&id),
            HostFrame::Exited { id, code } => self.note_hub_session_exited(&id, code),
            HostFrame::Resized { id, cols, rows } => self.note_remote_resized(&id, cols, rows),
            HostFrame::SizeReleased { id } => self.note_remote_size_released(&id),
            HostFrame::ShelfAdded { item } => self.note_shelf_added(*item),
            HostFrame::ShelfRemoved { file_id } => self.note_shelf_removed(&file_id),
            HostFrame::Shelf { .. } | HostFrame::File { .. } | HostFrame::Unknown => {}
            HostFrame::Error {
                req: None,
                code,
                message,
            } => {
                tracing::warn!("Hub broadcast error {code}: {message}; re-listing to resync");
                self.relist_hub_sessions();
            }
            _ => {}
        }
    }

    /// A `resized` frame from another device: when it differs from what the
    /// TUI last set, the device owns the size now — track it and resize only
    /// the local screen, without answering `resize` back. When it equals the
    /// TUI's own size it is the echo of a TUI `resize` and is ignored. An
    /// unknown id is ignored.
    fn note_remote_resized(&mut self, id: &str, cols: u16, rows: u16) {
        for agent in self
            .interactive_agents
            .iter_mut()
            .chain(self.terminal_agents.iter_mut())
        {
            if agent.id == id {
                if (cols, rows) == (agent.last_pty_cols, agent.last_pty_rows) {
                    return;
                }
                agent.remote_size = Some((cols, rows));
                agent.resize_vt_only(cols, rows);
                return;
            }
        }
    }

    /// A `size_released` frame: the remote device let go (or vanished), so
    /// the TUI owns the size again and re-asserts its own pane size, which
    /// broadcasts `resized` to any remaining device. Unknown id is ignored.
    fn note_remote_size_released(&mut self, id: &str) {
        let mut known = false;
        for agent in self
            .interactive_agents
            .iter_mut()
            .chain(self.terminal_agents.iter_mut())
        {
            if agent.id == id {
                agent.remote_size = None;
                known = true;
                break;
            }
        }
        if known {
            self.resize_interactive_agents();
        }
    }

    /// Reconnect when the backoff is due, then re-attach the survivors.
    fn retry_hub_connect(&mut self) {
        if !self.session_hub.retry_due() {
            return;
        }
        let url = self.session_hub.url.clone();
        let token = self.session_hub.token.clone();
        match HubClient::connect(&url, &token) {
            Ok(client) => {
                if self.adopt_hub_client(client) {
                    self.reconcile_after_reconnect();
                } else {
                    self.session_hub.arm_retry();
                }
            }
            Err(e) => {
                tracing::warn!("Session hub reconnect failed: {e}");
                self.session_hub.arm_retry();
            }
        }
    }

    /// Mark every session disconnected and arm the reconnect backoff.
    fn note_hub_dropped(&mut self) {
        for agent in self
            .interactive_agents
            .iter()
            .chain(self.terminal_agents.iter())
        {
            agent.note_disconnected();
        }
        if let Some(client) = self.session_hub.client.take() {
            client.close();
        }
        self.session_hub.arm_retry();
        tracing::warn!("Session hub connection dropped; reconnecting");
    }

    fn list_hub_sessions(&self) -> Option<Vec<SessionItem>> {
        let req = new_req();
        let answer = self
            .session_hub
            .client
            .as_ref()?
            .request(&req, list_frame(&req))
            .ok()?;
        match answer {
            HostFrame::Sessions { items, .. } => Some(items),
            _ => None,
        }
    }

    /// Attach every live session, in the order the sidebar shows them
    /// (see [`Self::attach_new_hub_sessions`]).
    fn attach_all_hub_sessions(&mut self) {
        let Some(items) = self.list_hub_sessions() else {
            // The connection accepted `list` and never answered: drop it and
            // retry cleanly instead of leaving every session invisible.
            self.note_hub_dropped();
            return;
        };
        self.attach_new_hub_sessions(&items);
        let _ = self.refresh_agents();
    }

    /// Re-list after a reconnect: drop what the hub lost, reattach the
    /// survivors' streams, attach the arrivals.
    fn reconcile_after_reconnect(&mut self) {
        let Some(items) = self.list_hub_sessions() else {
            // The list failed on a live connection; drop and retry cleanly.
            self.note_hub_dropped();
            return;
        };
        self.drop_gone_hub_sessions(&items);
        for agent in self
            .interactive_agents
            .iter_mut()
            .chain(self.terminal_agents.iter_mut())
        {
            if let Err(e) = agent.reattach() {
                tracing::warn!("Cannot reattach to session '{}': {e}", agent.name);
                if let Some(notice) = agent.proto_error.clone() {
                    self.hub_notice = Some(notice);
                }
            }
        }
        self.attach_new_hub_sessions(&items);
        let _ = self.refresh_agents();
    }

    /// Re-list after a hub broadcast error: drop what the hub lost, attach
    /// the arrivals. Streams stay as they are.
    fn relist_hub_sessions(&mut self) {
        let Some(items) = self.list_hub_sessions() else {
            return;
        };
        self.drop_gone_hub_sessions(&items);
        self.attach_new_hub_sessions(&items);
        let _ = self.refresh_agents();
        self.slow_force = true;
    }

    /// Remove sidebar sessions the hub no longer has.
    fn drop_gone_hub_sessions(&mut self, items: &[SessionItem]) {
        let live: HashSet<&str> = items.iter().map(|item| item.id.as_str()).collect();
        let gone: Vec<String> = self
            .interactive_agents
            .iter()
            .chain(self.terminal_agents.iter())
            .filter(|agent| !live.contains(agent.id.as_str()))
            .map(|agent| agent.id.clone())
            .collect();
        for id in &gone {
            // A daemon restart can fail a resume while this TUI is away: no
            // `exited` frame ever arrives, so reload the row's verdict first
            // — a recorded failure keeps its error card, everything else
            // drops as before.
            self.revive_gone_failed_session(id);
            self.remove_hub_session(id);
        }
    }

    /// A session the hub lost while this TUI was away (daemon restart) whose
    /// row records a failure: reload its exit code and detail so the removal
    /// keeps it as an error card instead of dropping it silently.
    fn revive_gone_failed_session(&mut self, id: &str) {
        let Ok(Some(row)) = self.db.get_interactive_session(id) else {
            return;
        };
        if row.status != "error" {
            return;
        }
        let Ok(detail) = self.db.get_session_exit_detail(id) else {
            return;
        };
        let Some(detail) = detail.filter(|detail| !detail.trim().is_empty()) else {
            return;
        };
        let code = self
            .db
            .get_session_exit_code(id)
            .ok()
            .flatten()
            .unwrap_or(1);
        if code == 0 {
            return;
        }
        if let Some(agent) = self.interactive_agents.iter_mut().find(|a| a.id == id) {
            agent.note_exited(code);
            agent.exit_detail = Some(detail);
        }
    }

    /// Attach the listed sessions the sidebar does not have yet, in the
    /// order the sidebar shows them: newest first — the `started_at DESC`
    /// order `get_active_sessions` resumed them in before this change, so a
    /// restart does not reorder the section (FR6).
    fn attach_new_hub_sessions(&mut self, items: &[SessionItem]) {
        let mut items: Vec<&SessionItem> = items.iter().collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        for item in items {
            self.attach_hub_item(item);
        }
    }

    /// Attach one hub session into its sidebar list, unless attached.
    fn attach_hub_item(&mut self, item: &SessionItem) {
        if self.hub_session_attached(&item.id) {
            return;
        }
        // Only a *live* session is attached. One that already exited before
        // this TUI attached is not: attaching it would replay its exit
        // handling (a failure notification, mission closing) for work that
        // ended in an earlier run, and would put a finished card back in the
        // sidebar on every start. The hub drops an exited session from the
        // list, but an exited item can still arrive in a list taken before
        // the removal.
        if item.status != "running" {
            return;
        }
        let is_terminal = item.kind == "terminal";
        let cli = Cli::from_str(&item.cli);
        let accent = if is_terminal {
            self.theme.header_color
        } else {
            let config = crate::domain::canopy_config::CanopyConfig::load(&self.data_dir);
            Self::resume_session_accent(config.get_cli(cli.as_str()))
        };
        let (cols, rows) = Self::session_panel_size();
        match InteractiveAgent::attach_item(item, cli, accent, None, cols, rows) {
            Ok(agent) => {
                if is_terminal {
                    // The snapshot already carries the scrollback; only the
                    // command history needs its cache entry.
                    let hist =
                        crate::tui::terminal_history::load_history(&self.data_dir, &agent.name);
                    self.terminal_histories.insert(agent.name.clone(), hist);
                    self.terminal_agents.push(agent);
                } else {
                    self.interactive_agents.push(agent);
                }
                self.slow_force = true;
            }
            Err(e) => tracing::warn!("Cannot attach to hub session '{}': {e}", item.name),
        }
    }

    fn hub_session_attached(&self, id: &str) -> bool {
        self.interactive_agents.iter().any(|a| a.id == id)
            || self.terminal_agents.iter().any(|a| a.id == id)
    }

    /// Drop one hub session from the sidebar — unless it is a failed
    /// interactive session, which stays as an `error · <tail>` card until
    /// the user dismisses it. Without this the `exited` frame that carries
    /// the failure and the `session_removed` frame that drops the card land
    /// in the same tick, and no failure is ever visible. Terminals always
    /// drop: their poll reaps every exited terminal anyway.
    fn remove_hub_session(&mut self, id: &str) {
        if self.retain_failed_hub_session(id) {
            return;
        }
        let names: Vec<String> = self
            .interactive_agents
            .iter()
            .chain(self.terminal_agents.iter())
            .filter(|agent| agent.id == id)
            .map(|agent| agent.name.clone())
            .collect();
        self.interactive_agents.retain(|agent| agent.id != id);
        self.terminal_agents.retain(|agent| agent.id != id);
        for name in &names {
            self.dissolve_groups_for_session(name);
        }
        self.slow_force = true;
    }

    /// Keep a failed interactive session as its error card: fold the arrived
    /// exit code into its status so the row reads `error · <first tail
    /// line>` and the exit poll handles (but never reaps) it. True when the
    /// session was retained. Only a nonzero exit WITH a recorded detail is
    /// kept — an explicit `kill` clears the detail, so a user-closed session
    /// still drops instead of lingering as a false failure.
    fn retain_failed_hub_session(&mut self, id: &str) -> bool {
        let Some(agent) = self.interactive_agents.iter_mut().find(|a| a.id == id) else {
            return false;
        };
        agent.poll();
        let failed = matches!(
            agent.status,
            crate::tui::agent::AgentStatus::Exited(code) if code != 0
        ) && agent
            .exit_detail
            .as_deref()
            .is_some_and(|detail| !detail.trim().is_empty());
        if failed {
            self.slow_force = true;
        }
        failed
    }

    /// Record an `exited` frame from the control connection. A natural exit
    /// also arrives on the session's own stream; both fold into the same
    /// exit code and `poll` picks it up on this tick. A nonzero exit also
    /// loads the row's failure detail (the hub wrote the harness's stderr
    /// tail there before emitting this frame) so the sidebar card shows
    /// *why* without opening logs.
    fn note_hub_session_exited(&mut self, id: &str, code: i32) {
        let detail = if code == 0 {
            None
        } else {
            self.db.get_session_exit_detail(id).ok().flatten()
        };
        for agent in self
            .interactive_agents
            .iter_mut()
            .chain(self.terminal_agents.iter_mut())
        {
            if agent.id == id {
                agent.note_exited(code);
                if detail.is_some() {
                    agent.exit_detail = detail.clone();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use std::sync::Arc;

    /// Serve one WebSocket that greets with `hello.proto = proto` and then
    /// holds the connection open without answering anything.
    fn serve_proto(proto: u32) -> String {
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("fake hub runtime");
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("bind fake hub");
                addr_tx
                    .send(listener.local_addr().expect("fake hub addr"))
                    .expect("fake hub addr channel");
                if let Ok((stream, _)) = listener.accept().await {
                    if let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await {
                        use futures::{SinkExt as _, StreamExt as _};
                        let hello = serde_json::json!({
                            "t": "hello", "proto": proto, "host_label": "fake",
                        })
                        .to_string();
                        let _ = ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(hello.into()))
                            .await;
                        while ws.next().await.is_some() {}
                    }
                }
            });
        });
        let addr: std::net::SocketAddr = addr_rx.recv().expect("fake hub address");
        format!("ws://{addr}/sessions/ws")
    }

    #[test]
    fn proto_mismatch_sets_the_notice_and_attaches_nothing() {
        let url = serve_proto(PROTO_VERSION + 1);
        let data_dir = tempfile::tempdir().expect("data dir");
        let db = Arc::new(Database::new(&data_dir.path().join("test.db")).expect("test database"));
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");

        app.connect_session_hub_to(&url, "test-token");

        let expected = proto_mismatch_message(PROTO_VERSION + 1);
        assert_eq!(app.hub_notice.as_deref(), Some(expected.as_str()));
        assert!(app.interactive_agents.is_empty());
        assert!(app.terminal_agents.is_empty());
        // The notice names a daemon restart, so the TUI keeps trying: the
        // restart has to heal it, not a second TUI restart.
        assert!(
            app.session_hub.next_retry_at.is_some(),
            "a protocol mismatch left no retry armed"
        );
        // Polling before the backoff is due attaches nothing and keeps the
        // notice up.
        app.poll_session_hub();
        assert!(app.interactive_agents.is_empty());
        assert!(app.terminal_agents.is_empty());
        assert_eq!(app.hub_notice.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn hub_port_prefers_the_recorded_daemon_port() {
        // A daemon started with `--port` records it; the configured default
        // would point the hub client at a socket nobody is listening on.
        let data_dir = tempfile::tempdir().expect("data dir");
        let db = Arc::new(Database::new(&data_dir.path().join("test.db")).expect("test database"));
        db.set_state("port", "8123").expect("record the port");
        let app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");

        assert_eq!(app.hub_port(), 8123);

        // A malformed value falls back to the configured port.
        db.set_state("port", "not-a-port")
            .expect("record a bad port");
        assert_eq!(app.hub_port(), crate::resolve_port(None));
    }

    /// An exited session item — the hub drops it from the list, but one can
    /// still arrive from a list taken before the removal — must not be
    /// re-attached on a later start: attaching it replays its exit handling
    /// (a failure notification, mission closing) for work that ended in an
    /// earlier run, and puts a finished card back in the sidebar.
    #[test]
    fn list_attach_skips_sessions_that_already_exited() {
        let hub = &crate::sessions::client::test_client().hub;
        let agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("exited-before-attach"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn cat on the test hub");
        let session = hub.get(&agent.id).expect("the hub holds the spawn");
        hub.record_natural_exit(&session, 7);
        drop(agent);
        let item = session.item();
        assert_eq!(item.status, "exited");

        let data_dir = tempfile::tempdir().expect("data dir");
        let db = Arc::new(Database::new(&data_dir.path().join("test.db")).expect("test database"));
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");

        app.attach_new_hub_sessions(&[item]);

        assert!(app.interactive_agents.is_empty());
        assert!(app.terminal_agents.is_empty());
    }

    /// FR6: start attaches the live sessions in the order the sidebar shows
    /// them today — newest first, the `started_at DESC` order the pre-hub
    /// `get_active_sessions` resume pushed them in — so restarting does not
    /// reorder the section.
    #[test]
    fn start_attaches_sessions_newest_first_like_the_sidebar_today() {
        let hub = &crate::sessions::client::test_client().hub;
        let older = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("order-older"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn the older cat session");
        let older_item = hub.get(&older.id).expect("the hub holds the spawn").item();
        let newer = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("order-newer"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn the newer cat session");
        let newer_item = hub.get(&newer.id).expect("the hub holds the spawn").item();
        drop(older);
        drop(newer);

        let data_dir = tempfile::tempdir().expect("data dir");
        let db = Arc::new(Database::new(&data_dir.path().join("test.db")).expect("test database"));
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");

        // Deliberately oldest-first, as a `list` answer can arrive: the
        // attach order must still come out newest-first.
        app.attach_new_hub_sessions(&[older_item, newer_item]);

        let order: Vec<&str> = app
            .terminal_agents
            .iter()
            .map(|agent| agent.name.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["order-newer", "order-older"],
            "startup attach order differs from the sidebar's order today"
        );
        for agent in &mut app.terminal_agents {
            agent.kill();
        }
    }

    fn retention_app() -> (App, tempfile::TempDir) {
        let data_dir = tempfile::tempdir().expect("data dir");
        let db = Arc::new(Database::new(&data_dir.path().join("test.db")).expect("test database"));
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");
        (app, data_dir)
    }

    fn push_running_interactive(app: &mut App, name: &str) -> String {
        let agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some(name),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn cat as a stand-in session");
        let id = agent.id.clone();
        app.interactive_agents.push(agent);
        id
    }

    fn kill_test_hub_session(id: &str) {
        crate::sessions::client::test_client().hub.kill(id);
    }

    #[test]
    fn failed_interactive_session_stays_as_an_error_card() {
        // The `exited` and `session_removed` frames land in the same tick:
        // without retention the failure is never visible.
        let (mut app, _dir) = retention_app();
        let id = push_running_interactive(&mut app, "failed-keep");
        let agent = app
            .interactive_agents
            .iter_mut()
            .find(|agent| agent.id == id)
            .expect("pushed agent");
        agent.note_exited(1);
        agent.exit_detail = Some("boom tail".to_string());

        app.remove_hub_session(&id);

        let kept = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("a failed session must stay as its error card");
        assert!(
            matches!(kept.status, crate::tui::agent::AgentStatus::Exited(1)),
            "the retained card folds the exit into its status"
        );
        assert_eq!(kept.exit_detail.as_deref(), Some("boom tail"));
        kill_test_hub_session(&id);
    }

    #[test]
    fn clean_killed_and_running_sessions_drop() {
        let (mut app, _dir) = retention_app();
        // Clean exit: the poll reaps it anyway.
        let clean = push_running_interactive(&mut app, "clean-drop");
        // Killed (nonzero, but no detail — `kill` clears it): a user close,
        // not a failure.
        let killed = push_running_interactive(&mut app, "killed-drop");
        // Still running: nothing failed.
        let running = push_running_interactive(&mut app, "running-drop");
        for (id, code) in [(&clean, 0), (&killed, 1)] {
            app.interactive_agents
                .iter()
                .find(|agent| &agent.id == id)
                .expect("pushed agent")
                .note_exited(code);
        }

        for id in [&clean, &killed, &running] {
            app.remove_hub_session(id);
        }

        assert!(
            app.interactive_agents.is_empty(),
            "only failed-with-detail sessions are retained"
        );
        for id in [&clean, &killed, &running] {
            kill_test_hub_session(id);
        }
    }

    #[test]
    fn failed_terminal_still_drops() {
        // Terminals always drop: their poll reaps every exited terminal, so
        // retaining one here would only last until the next poll.
        let (mut app, _dir) = retention_app();
        let mut agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("failed-term"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn cat as a stand-in terminal");
        let id = agent.id.clone();
        agent.note_exited(2);
        agent.exit_detail = Some("term tail".to_string());
        app.terminal_agents.push(agent);

        app.remove_hub_session(&id);

        assert!(
            app.terminal_agents.is_empty(),
            "a failed terminal still drops"
        );
        kill_test_hub_session(&id);
    }

    #[test]
    fn gone_failed_session_is_revived_from_its_row() {
        // A daemon restart can fail a resume while the TUI is away: no
        // `exited` frame ever arrives, so the relist reloads the row's
        // verdict instead of dropping the card silently.
        let (mut app, _dir) = retention_app();
        let id = push_running_interactive(&mut app, "gone-failed");
        app.db
            .insert_interactive_session(
                &id,
                "gone-failed",
                "muse",
                "/tmp",
                None,
                None,
                "interactive",
                None,
            )
            .expect("insert row");
        app.db.finish_interactive_session(&id, 3).expect("fail row");
        app.db
            .set_session_exit_detail(&id, "resume tail")
            .expect("record tail");

        app.drop_gone_hub_sessions(&[]);

        let kept = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("the recorded failure must keep its error card");
        assert!(
            matches!(kept.status, crate::tui::agent::AgentStatus::Exited(3)),
            "the revived card carries the row's exit code"
        );
        assert_eq!(kept.exit_detail.as_deref(), Some("resume tail"));
        kill_test_hub_session(&id);
    }

    #[test]
    fn gone_clean_or_detail_less_sessions_drop() {
        let (mut app, _dir) = retention_app();
        let clean = push_running_interactive(&mut app, "gone-clean");
        let bare = push_running_interactive(&mut app, "gone-bare");
        let unknown = push_running_interactive(&mut app, "gone-unknown");
        for (id, name) in [(&clean, "gone-clean"), (&bare, "gone-bare")] {
            app.db
                .insert_interactive_session(
                    id,
                    name,
                    "muse",
                    "/tmp",
                    None,
                    None,
                    "interactive",
                    None,
                )
                .expect("insert row");
        }
        app.db
            .finish_interactive_session(&clean, 0)
            .expect("complete row");
        // An error row with no detail is a kill, not a failure.
        app.db
            .finish_interactive_session(&bare, 1)
            .expect("fail row");

        app.drop_gone_hub_sessions(&[]);

        assert!(
            app.interactive_agents.is_empty(),
            "clean, detail-less and unknown rows all drop"
        );
        for id in [&clean, &bare, &unknown] {
            kill_test_hub_session(id);
        }
    }

    #[test]
    fn own_resize_echo_is_ignored() {
        let (mut app, _dir) = retention_app();
        let id = push_running_interactive(&mut app, "echo-ignored");
        app.handle_hub_frame(HostFrame::Resized {
            id: id.clone(),
            cols: 80,
            rows: 24,
        });
        let agent = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("pushed agent");
        assert!(
            agent.remote_size.is_none(),
            "the TUI's own echo must not mark the size remote"
        );
        assert_eq!((agent.last_pty_cols, agent.last_pty_rows), (80, 24));
        kill_test_hub_session(&id);
    }

    #[test]
    fn foreign_resize_sets_remote_and_skips_bulk_resize() {
        let (mut app, _dir) = retention_app();
        let id = push_running_interactive(&mut app, "remote-owned");
        app.handle_hub_frame(HostFrame::Resized {
            id: id.clone(),
            cols: 60,
            rows: 20,
        });
        let agent = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("pushed agent");
        assert_eq!(agent.remote_size, Some((60, 20)));
        assert_eq!((agent.last_pty_cols, agent.last_pty_rows), (60, 20));

        // The bulk resizer must not fight the device that owns the size.
        app.last_panel_inner = (80, 24);
        app.resize_interactive_agents();
        let agent = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("pushed agent");
        assert_eq!(
            (agent.last_pty_cols, agent.last_pty_rows),
            (60, 20),
            "bulk resize must skip a remotely-owned session"
        );
        kill_test_hub_session(&id);
    }

    #[test]
    fn release_clears_remote_and_reasserts_pane_size() {
        let (mut app, _dir) = retention_app();
        let id = push_running_interactive(&mut app, "released-back");
        app.handle_hub_frame(HostFrame::Resized {
            id: id.clone(),
            cols: 60,
            rows: 20,
        });
        app.last_panel_inner = (80, 24);

        app.handle_hub_frame(HostFrame::SizeReleased { id: id.clone() });

        let agent = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("pushed agent");
        assert!(
            agent.remote_size.is_none(),
            "a release hands the size back to the TUI"
        );
        assert_eq!(
            (agent.last_pty_cols, agent.last_pty_rows),
            (80, 24),
            "a release re-asserts the pane size"
        );
        kill_test_hub_session(&id);
    }

    #[test]
    fn frames_for_unknown_sessions_are_ignored() {
        let (mut app, _dir) = retention_app();
        let id = push_running_interactive(&mut app, "known-session");
        app.handle_hub_frame(HostFrame::Resized {
            id: "no-such-session".to_string(),
            cols: 60,
            rows: 20,
        });
        app.handle_hub_frame(HostFrame::SizeReleased {
            id: "no-such-session".to_string(),
        });
        assert_eq!(app.interactive_agents.len(), 1);
        let agent = app
            .interactive_agents
            .iter()
            .find(|agent| agent.id == id)
            .expect("pushed agent");
        assert!(agent.remote_size.is_none());
        assert_eq!((agent.last_pty_cols, agent.last_pty_rows), (80, 24));
        kill_test_hub_session(&id);
    }
}
