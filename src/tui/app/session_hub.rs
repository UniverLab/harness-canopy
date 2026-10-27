//! Daemon session-hub wiring: connect, attach-all, live dispatch, reconnect.
//!
//! The TUI owns no session: on start it connects to the hub, lists, and
//! attaches every live session; `session_added` / `session_removed` from
//! other clients update the sidebar; a dropped WebSocket marks the sessions
//! disconnected and reconnects with backoff, then re-attaches.

use std::collections::HashSet;
use std::time::{Duration, Instant};

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
    client: Option<HubClient>,
    url: String,
    token: String,
    next_retry_at: Option<Instant>,
    backoff: Duration,
    proto_error: bool,
}

impl Default for SessionHubState {
    fn default() -> Self {
        Self {
            client: None,
            url: String::new(),
            token: String::new(),
            next_retry_at: None,
            backoff: INITIAL_BACKOFF,
            proto_error: false,
        }
    }
}

impl SessionHubState {
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
        let url = ws_url(crate::resolve_port(None));
        self.connect_session_hub_to(&url, &token);
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
            self.session_hub.proto_error = true;
            self.session_hub.client = None;
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
        self.session_hub.proto_error = false;
        self.hub_notice = None;
        true
    }

    /// Poll the hub connection: dispatch frames, reconnect with backoff.
    /// Runs first on every refresh tick.
    pub(crate) fn poll_session_hub(&mut self) {
        if self.session_hub.proto_error {
            return;
        }
        if self.session_hub.client.is_none() {
            self.retry_hub_connect();
            return;
        }
        self.dispatch_hub_frames();
    }

    /// Close the hub WebSocket without touching the sessions.
    pub(crate) fn close_session_hub(&mut self) {
        self.session_hub.close();
    }

    /// Drain one tick's worth of hub frames.
    fn dispatch_hub_frames(&mut self) {
        loop {
            let event = self
                .session_hub
                .client
                .as_ref()
                .and_then(|client| client.try_recv());
            let Some(event) = event else {
                return;
            };
            match event {
                HubRecv::Frame(frame) => self.handle_hub_frame(frame),
                HubRecv::Closed => {
                    self.note_hub_dropped();
                    return;
                }
            }
        }
    }

    fn handle_hub_frame(&mut self, frame: HostFrame) {
        match frame {
            HostFrame::SessionAdded { item } => self.attach_hub_item(&item),
            HostFrame::SessionRemoved { id } => self.remove_hub_session(&id),
            HostFrame::Exited { id, code } => self.note_hub_session_exited(&id, code),
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

    /// Attach every live session, oldest first.
    fn attach_all_hub_sessions(&mut self) {
        let Some(items) = self.list_hub_sessions() else {
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
            self.remove_hub_session(id);
        }
    }

    /// Attach the listed sessions the sidebar does not have yet, oldest first.
    fn attach_new_hub_sessions(&mut self, items: &[SessionItem]) {
        let mut items: Vec<&SessionItem> = items.iter().collect();
        items.sort_by(|a, b| a.started_at.cmp(&b.started_at));
        for item in items {
            self.attach_hub_item(item);
        }
    }

    /// Attach one hub session into its sidebar list, unless attached.
    fn attach_hub_item(&mut self, item: &SessionItem) {
        if self.hub_session_attached(&item.id) {
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
            }
            Err(e) => tracing::warn!("Cannot attach to hub session '{}': {e}", item.name),
        }
    }

    fn hub_session_attached(&self, id: &str) -> bool {
        self.interactive_agents.iter().any(|a| a.id == id)
            || self.terminal_agents.iter().any(|a| a.id == id)
    }

    /// Drop one hub session from the sidebar.
    fn remove_hub_session(&mut self, id: &str) {
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
    }

    /// Record an `exited` frame from the control connection. A natural exit
    /// also arrives on the session's own stream; both fold into the same
    /// exit code and `poll` picks it up on this tick.
    fn note_hub_session_exited(&mut self, id: &str, code: i32) {
        for agent in self
            .interactive_agents
            .iter()
            .chain(self.terminal_agents.iter())
        {
            if agent.id == id {
                agent.note_exited(code);
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
        let url = serve_proto(2);
        let data_dir = tempfile::tempdir().expect("data dir");
        let db = Arc::new(Database::new(&data_dir.path().join("test.db")).expect("test database"));
        let mut app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("test app");

        app.connect_session_hub_to(&url, "test-token");

        let expected = proto_mismatch_message(2);
        assert_eq!(app.hub_notice.as_deref(), Some(expected.as_str()));
        assert!(app.interactive_agents.is_empty());
        assert!(app.terminal_agents.is_empty());
        // No retry either: only a daemon restart can fix the protocol.
        app.poll_session_hub();
        assert!(app.interactive_agents.is_empty());
        assert!(app.terminal_agents.is_empty());
    }
}
