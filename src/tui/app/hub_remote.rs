//! Data, background I/O and keys for the hub's Remote tab.
//!
//! Mirrors the Settings tab's shape: the status view comes from a
//! `remote` `status` daemon call cached here, writes (`set_enabled`,
//! `pair`, `revoke`, `disconnect`) run on background threads, and the GitHub
//! device flow runs on its own thread with a current-thread runtime. The
//! status also refreshes while the hub is closed — on a slower throttle —
//! so the sidebar's live-device indicator stays current.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::KeyCode;

use super::hub::HubTab;
use super::hub_remote_trust::{TrustField, TrustForm};
use super::types::App;
use crate::domain::canopy_config::CanopyConfig;
use crate::remote::auth::{self, DeviceCodeInfo, RemoteEndpoints};
use crate::remote::panel::{PairView, RemoteView};
use crate::tui::app::dialog::GraphActionMessage;

/// Minimum gap between two `remote_status` fetches while the tab is visible.
pub(crate) const REMOTE_REFRESH_ON_TAB: Duration = Duration::from_secs(2);
/// Minimum gap between two `remote_status` fetches otherwise (the sidebar
/// indicator reads the same cached view).
pub(crate) const REMOTE_REFRESH_IDLE: Duration = Duration::from_secs(30);

/// One selectable row of the Remote tab: the four fixed rows, then one per
/// paired device, then one per trusted profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteRow {
    GitHub,
    Control,
    Relay,
    Pair,
    Device(usize),
    Trusted(usize),
}

/// The rows the tab shows for one status view.
pub(crate) fn remote_rows(view: &RemoteView) -> Vec<RemoteRow> {
    let mut rows = vec![
        RemoteRow::GitHub,
        RemoteRow::Control,
        RemoteRow::Relay,
        RemoteRow::Pair,
    ];
    rows.extend((0..view.devices.len()).map(RemoteRow::Device));
    rows.extend((0..view.trusted.len()).map(RemoteRow::Trusted));
    rows
}

/// One event from the device-flow thread: first the code to show, then the
/// final result.
pub(crate) enum LoginEvent {
    Code(DeviceCodeInfo),
    Done(Result<String, String>),
}

/// The GitHub device flow: idle, or waiting with the code shown so far.
#[derive(Default)]
pub(crate) enum LoginFlow {
    #[default]
    Idle,
    Waiting {
        code: Option<DeviceCodeInfo>,
        rx: mpsc::Receiver<LoginEvent>,
    },
}

/// One finished remote write: the new switch state, a fresh pairing code,
/// the revoked or disconnected device id — or the daemon's refusal.
pub(crate) enum RemoteWriteResult {
    SetEnabled(Result<bool, String>),
    Pair(Result<PairView, String>),
    Revoke(Result<String, String>),
    Disconnect(Result<String, String>),
    TrustAdd(Result<String, String>),
    TrustRemove(Result<String, String>),
}

/// The Remote tab's state.
#[derive(Default)]
pub(crate) struct RemoteState {
    pub view: RemoteView,
    pub row_idx: usize,
    /// The last pairing code fetched; stays valid after the overlay hides.
    pub pair: Option<PairView>,
    pub pair_open: bool,
    /// The device id an armed `r` revoke would delete.
    pub revoke_confirm: Option<String>,
    /// The trust-form draft and its overlay.
    pub trust: TrustForm,
    /// The login an armed `r` trust-remove would forget.
    pub trust_remove_confirm: Option<String>,
    pub login: LoginFlow,
    pub rows_rx: Option<mpsc::Receiver<Result<RemoteView, String>>>,
    pub write_rx: Option<mpsc::Receiver<RemoteWriteResult>>,
    pub message: Option<GraphActionMessage>,
    pub message_at: Option<Instant>,
    pub last_fetch: Option<Instant>,
    pub stale: bool,
}

/// Parse the `remote_status` text payload.
pub(crate) fn parse_remote_view(text: &str) -> Result<RemoteView, String> {
    serde_json::from_str(text)
        .map_err(|error| format!("remote_status: expected the status JSON: {error}"))
}

/// Parse the `remote_pair` text payload.
pub(crate) fn parse_pair_view(text: &str) -> Result<PairView, String> {
    serde_json::from_str(text)
        .map_err(|error| format!("remote_pair: expected the pairing JSON: {error}"))
}

fn fetch_remote_view(port: &str) -> Result<RemoteView, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({ "action": "status" }),
    ) {
        Ok(outcome) if !outcome.is_error => parse_remote_view(&outcome.text),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn set_remote_enabled(port: &str, enabled: bool) -> Result<bool, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({ "action": "set_enabled", "enabled": enabled }),
    ) {
        Ok(outcome) if !outcome.is_error => Ok(enabled),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn fetch_pair_view(port: &str) -> Result<PairView, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({ "action": "pair" }),
    ) {
        Ok(outcome) if !outcome.is_error => parse_pair_view(&outcome.text),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn revoke_device(port: &str, device_id: &str) -> Result<String, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({ "action": "revoke", "device_id": device_id }),
    ) {
        Ok(outcome) if !outcome.is_error => Ok(device_id.to_string()),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn disconnect_device(port: &str, device_id: &str) -> Result<String, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({ "action": "disconnect", "device_id": device_id }),
    ) {
        Ok(outcome) if !outcome.is_error => Ok(device_id.to_string()),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn trust_add_profile(
    port: &str,
    login: &str,
    all_sessions: bool,
    sessions: &[String],
) -> Result<String, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({
            "action": "trust_add",
            "login": login,
            "all_sessions": all_sessions,
            "sessions": sessions,
        }),
    ) {
        Ok(outcome) if !outcome.is_error => Ok(login.to_string()),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

fn trust_remove_profile(port: &str, login: &str) -> Result<String, String> {
    match crate::tui::mcp_client::call_daemon_tool(
        port,
        "remote",
        &serde_json::json!({ "action": "trust_remove", "login": login }),
    ) {
        Ok(outcome) if !outcome.is_error => Ok(login.to_string()),
        Ok(outcome) => Err(outcome.text),
        Err(error) => Err(format!("{error:#}")),
    }
}

/// Run the OAuth device flow to completion: report the code through
/// `code_tx`, then return the login. Blocking, for the login thread.
fn run_device_login(
    client_id: &str,
    data_dir: &Path,
    code_tx: &mpsc::Sender<LoginEvent>,
) -> Result<String, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("{error:#}"))?;
    runtime.block_on(async {
        auth::login(client_id, data_dir, &RemoteEndpoints::default(), |info| {
            let _ = code_tx.send(LoginEvent::Code(info.clone()));
        })
        .await
        .map(|identity| identity.login)
        .map_err(|error| format!("{error:#}"))
    })
}

impl App {
    /// Request a fresh `remote_status` as soon as possible.
    pub(crate) fn remote_request_refresh(&mut self) {
        self.hub.remote.stale = true;
        self.hub.remote.last_fetch = None;
    }

    /// Spawn a `remote_status` fetch unless one is in flight, the daemon is
    /// down, or the throttle still holds — 2 s on the tab, 30 s otherwise.
    /// The very first pass only arms the throttle, so `App::new` never
    /// issues a daemon call by itself; the sidebar indicator's first fetch
    /// waits out one idle interval.
    pub(crate) fn refresh_remote_status(&mut self) {
        if !self.daemon_running {
            return;
        }
        if self.hub.remote.rows_rx.is_some() {
            return;
        }
        if self.hub.remote.stale {
            self.spawn_remote_status_fetch();
            return;
        }
        let every = if self.hub_open && self.hub.tab == HubTab::Remote {
            REMOTE_REFRESH_ON_TAB
        } else {
            REMOTE_REFRESH_IDLE
        };
        match self.hub.remote.last_fetch {
            None => self.hub.remote.last_fetch = Some(Instant::now()),
            Some(at) if at.elapsed() >= every => self.spawn_remote_status_fetch(),
            Some(_) => {}
        }
    }

    /// Spawn one `remote_status` fetch on a background thread.
    fn spawn_remote_status_fetch(&mut self) {
        let port = self.daemon_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(fetch_remote_view(&port));
        });
        self.hub.remote.rows_rx = Some(rx);
        self.hub.remote.last_fetch = Some(Instant::now());
        self.hub.remote.stale = false;
    }

    /// Apply a finished `remote_status` fetch, if any.
    pub(crate) fn poll_remote_status(&mut self) {
        let Some(rx) = self.hub.remote.rows_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(view)) => {
                self.hub.remote.view = view;
                self.clamp_remote_selection();
            }
            Ok(Err(text)) => {
                self.set_remote_message(true, format!("Remote unavailable: {text}"));
            }
            Err(mpsc::TryRecvError::Empty) => self.hub.remote.rows_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Apply a finished remote write, if any.
    pub(crate) fn poll_remote_write(&mut self) {
        let Some(rx) = self.hub.remote.write_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(result) => self.apply_remote_write(result),
            Err(mpsc::TryRecvError::Empty) => self.hub.remote.write_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Drain the device-flow thread's events: codes update the GitHub row,
    /// the final result ends the flow.
    pub(crate) fn poll_remote_login(&mut self) {
        let LoginFlow::Waiting { mut code, rx } =
            std::mem::replace(&mut self.hub.remote.login, LoginFlow::Idle)
        else {
            return;
        };
        let mut done: Option<Result<String, String>> = None;
        while let Ok(event) = rx.try_recv() {
            match event {
                LoginEvent::Code(info) => code = Some(info),
                LoginEvent::Done(result) => {
                    done = Some(result);
                    break;
                }
            }
        }
        match done {
            Some(Ok(login)) => {
                self.set_remote_message(false, format!("Signed in to GitHub as {login}."));
                self.remote_request_refresh();
            }
            Some(Err(text)) => self.set_remote_message(true, text),
            None => self.hub.remote.login = LoginFlow::Waiting { code, rx },
        }
    }

    fn apply_remote_write(&mut self, result: RemoteWriteResult) {
        match result {
            RemoteWriteResult::SetEnabled(Ok(enabled)) => {
                self.set_remote_message(
                    false,
                    format!("Remote control {}", if enabled { "on" } else { "off" }),
                );
                self.remote_request_refresh();
            }
            RemoteWriteResult::SetEnabled(Err(text)) => self.set_remote_message(true, text),
            RemoteWriteResult::Pair(Ok(pair)) => {
                self.hub.remote.pair = Some(pair);
                self.open_pair_overlay();
            }
            RemoteWriteResult::Pair(Err(text)) => self.set_remote_message(true, text),
            RemoteWriteResult::Revoke(Ok(id)) => {
                self.set_remote_message(false, format!("Revoked {id}"));
                self.remote_request_refresh();
            }
            RemoteWriteResult::Revoke(Err(text)) => self.set_remote_message(true, text),
            RemoteWriteResult::Disconnect(Ok(id)) => {
                self.set_remote_message(false, format!("Disconnected {id}"));
                self.remote_request_refresh();
            }
            RemoteWriteResult::Disconnect(Err(text)) => self.set_remote_message(true, text),
            RemoteWriteResult::TrustAdd(Ok(login)) => {
                self.set_remote_message(false, format!("Trusted {login}"));
                self.remote_request_refresh();
            }
            RemoteWriteResult::TrustAdd(Err(text)) => self.set_remote_message(true, text),
            RemoteWriteResult::TrustRemove(Ok(login)) => {
                self.set_remote_message(false, format!("Removed trusted profile {login}"));
                self.remote_request_refresh();
            }
            RemoteWriteResult::TrustRemove(Err(text)) => self.set_remote_message(true, text),
        }
    }

    /// Run one remote write on a background thread. No-op while a previous
    /// write is still in flight.
    fn spawn_remote_write(
        &mut self,
        work: impl FnOnce(String) -> RemoteWriteResult + Send + 'static,
    ) {
        if self.hub.remote.write_rx.is_some() {
            self.set_remote_message(true, "A remote write is still running");
            return;
        }
        let port = self.daemon_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(work(port));
        });
        self.hub.remote.write_rx = Some(rx);
    }

    /// Route one key to the open Remote tab.
    pub(crate) fn hub_remote_key(&mut self, code: KeyCode) {
        // The pairing overlay owns every key but Esc (which event/hub.rs
        // routes to `close_pair_overlay` before reaching here).
        if self.hub.remote.pair_open {
            return;
        }
        // The trust form owns every key while it is open.
        if self.hub.remote.trust.open {
            self.trust_form_key(code);
            return;
        }
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.move_remote_selection(false),
            KeyCode::Down | KeyCode::Char('j') => self.move_remote_selection(true),
            KeyCode::Enter => self.activate_remote_row(),
            KeyCode::Char('d') => self.disconnect_remote_device(),
            KeyCode::Char('r') => match self.selected_remote_row() {
                Some(RemoteRow::Device(_)) => self.arm_remote_revoke(),
                Some(RemoteRow::Trusted(_)) => self.arm_trust_remove(),
                _ => {}
            },
            KeyCode::Char('t') => self.open_trust_form(),
            _ => {}
        }
    }

    /// Route one key to the open trust form: typing edits the focused text
    /// stop, `Tab`/`↑`/`↓` move between stops, `a` or `Space` flips the
    /// toggle, `Enter` submits, `Esc` closes.
    pub(crate) fn trust_form_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Esc => self.hub.remote.trust.close(),
            KeyCode::Enter => self.submit_trust_form(),
            KeyCode::Tab => self.hub.remote.trust.cycle_focus(true),
            KeyCode::BackTab => self.hub.remote.trust.cycle_focus(false),
            KeyCode::Up => self.hub.remote.trust.cycle_focus(false),
            KeyCode::Down => self.hub.remote.trust.cycle_focus(true),
            KeyCode::Backspace => self.hub.remote.trust.pop_char(),
            KeyCode::Char('a') if self.hub.remote.trust.focus == TrustField::All => {
                self.hub.remote.trust.toggle_all();
            }
            KeyCode::Char(' ') if self.hub.remote.trust.focus == TrustField::All => {
                self.hub.remote.trust.toggle_all();
            }
            KeyCode::Char(c) => self.hub.remote.trust.push_char(c),
            _ => {}
        }
    }

    /// Enter on the selected row: sign in, toggle the switch, or fetch a
    /// fresh pairing code. Relay and device rows do nothing.
    fn activate_remote_row(&mut self) {
        match self.selected_remote_row() {
            Some(RemoteRow::GitHub) => {
                if !self.hub.remote.view.signed_in {
                    self.start_remote_login();
                }
            }
            Some(RemoteRow::Control) => self.toggle_remote_control(),
            Some(RemoteRow::Pair) => self.request_pair_code(),
            Some(RemoteRow::Trusted(_)) => self.edit_trusted_row(),
            Some(RemoteRow::Relay) | Some(RemoteRow::Device(_)) | None => {}
        }
    }

    /// Enter on the Control row: flip the relay switch, or say why it is
    /// disabled while signed out.
    pub(crate) fn toggle_remote_control(&mut self) {
        if !self.hub.remote.view.signed_in {
            self.set_remote_message(true, "Sign in to GitHub first");
            return;
        }
        let enabled = !self.hub.remote.view.enabled;
        self.spawn_remote_write(move |port| {
            RemoteWriteResult::SetEnabled(set_remote_enabled(&port, enabled))
        });
    }

    /// Enter on the GitHub row while signed out: run the device flow on a
    /// background thread.
    pub(crate) fn start_remote_login(&mut self) {
        if matches!(self.hub.remote.login, LoginFlow::Waiting { .. }) {
            return;
        }
        if self.hub.remote.view.signed_in {
            return;
        }
        let data_dir = self.data_dir.clone();
        let client_id = CanopyConfig::load(&data_dir).remote.github_client_id;
        let (tx, rx) = mpsc::channel();
        self.hub.remote.login = LoginFlow::Waiting { code: None, rx };
        std::thread::spawn(move || {
            let code_tx = tx.clone();
            let login = run_device_login(&client_id, &data_dir, &code_tx);
            let _ = tx.send(LoginEvent::Done(login));
        });
    }

    /// Enter on the Pair row: fetch a fresh code (it replaces the live one).
    pub(crate) fn request_pair_code(&mut self) {
        self.spawn_remote_write(|port| RemoteWriteResult::Pair(fetch_pair_view(&port)));
    }

    /// Show the pairing overlay for the last fetched code.
    pub(crate) fn open_pair_overlay(&mut self) {
        if self.hub.remote.pair.is_some() {
            self.hub.remote.pair_open = true;
        }
    }

    /// Hide the pairing overlay; the code stays valid until it expires.
    pub(crate) fn close_pair_overlay(&mut self) {
        self.hub.remote.pair_open = false;
    }

    /// `d` on a device row: disconnect its live peer at once (no confirm;
    /// the pairing stays). Ignored on any other row.
    pub(crate) fn disconnect_remote_device(&mut self) {
        if let Some(RemoteRow::Device(index)) = self.selected_remote_row() {
            if let Some(device) = self.hub.remote.view.devices.get(index) {
                let device_id = device.id.clone();
                self.spawn_remote_write(move |port| {
                    RemoteWriteResult::Disconnect(disconnect_device(&port, &device_id))
                });
            }
        }
    }

    /// `r` on a device row: arm the y/n revoke confirmation.
    pub(crate) fn arm_remote_revoke(&mut self) {
        self.hub.remote.trust_remove_confirm = None;
        if let Some(RemoteRow::Device(index)) = self.selected_remote_row() {
            if let Some(device) = self.hub.remote.view.devices.get(index) {
                self.hub.remote.revoke_confirm = Some(device.id.clone());
            }
        }
    }

    /// `y` on an armed revoke: delete the device through the daemon.
    pub(crate) fn confirm_remote_revoke(&mut self) {
        let Some(device_id) = self.hub.remote.revoke_confirm.clone() else {
            return;
        };
        self.hub.remote.revoke_confirm = None;
        self.spawn_remote_write(move |port| {
            RemoteWriteResult::Revoke(revoke_device(&port, &device_id))
        });
    }

    /// `n`/Esc on an armed revoke: stand down.
    pub(crate) fn cancel_remote_revoke(&mut self) {
        self.hub.remote.revoke_confirm = None;
    }

    /// `t` anywhere on the tab: trust a new profile through the form.
    pub(crate) fn open_trust_form(&mut self) {
        self.hub.remote.trust.open_new();
    }

    /// `Enter` on a trusted row: edit that profile's grant through the form.
    fn edit_trusted_row(&mut self) {
        if let Some(RemoteRow::Trusted(index)) = self.selected_remote_row() {
            if let Some(profile) = self.hub.remote.view.trusted.get(index).cloned() {
                self.hub.remote.trust.open_edit(&profile);
            }
        }
    }

    /// `Enter` in the trust form: validate the login and save the grant
    /// through the daemon.
    pub(crate) fn submit_trust_form(&mut self) {
        let login = self.hub.remote.trust.login.trim().to_string();
        if login.is_empty() {
            self.set_remote_message(true, "Trust needs a GitHub username");
            return;
        }
        let all = self.hub.remote.trust.all;
        let sessions = self.hub.remote.trust.sessions_list();
        self.hub.remote.trust.close();
        self.spawn_remote_write(move |port| {
            RemoteWriteResult::TrustAdd(trust_add_profile(&port, &login, all, &sessions))
        });
    }

    /// `r` on a trusted row: arm the y/n remove confirmation.
    pub(crate) fn arm_trust_remove(&mut self) {
        self.hub.remote.revoke_confirm = None;
        if let Some(RemoteRow::Trusted(index)) = self.selected_remote_row() {
            if let Some(profile) = self.hub.remote.view.trusted.get(index) {
                self.hub.remote.trust_remove_confirm = Some(profile.login.clone());
            }
        }
    }

    /// `y` on an armed trust-remove: forget the profile through the daemon.
    pub(crate) fn confirm_trust_remove(&mut self) {
        let Some(login) = self.hub.remote.trust_remove_confirm.clone() else {
            return;
        };
        self.hub.remote.trust_remove_confirm = None;
        self.spawn_remote_write(move |port| {
            RemoteWriteResult::TrustRemove(trust_remove_profile(&port, &login))
        });
    }

    /// `n`/Esc on an armed trust-remove: stand down.
    pub(crate) fn cancel_trust_remove(&mut self) {
        self.hub.remote.trust_remove_confirm = None;
    }

    /// The row under the cursor, if any.
    pub(crate) fn selected_remote_row(&self) -> Option<RemoteRow> {
        remote_rows(&self.hub.remote.view)
            .get(self.hub.remote.row_idx)
            .copied()
    }

    fn clamp_remote_selection(&mut self) {
        let count = remote_rows(&self.hub.remote.view).len();
        self.hub.remote.row_idx = if count == 0 {
            0
        } else {
            self.hub.remote.row_idx.min(count - 1)
        };
    }

    fn move_remote_selection(&mut self, forward: bool) {
        let count = remote_rows(&self.hub.remote.view).len();
        if count == 0 {
            self.hub.remote.row_idx = 0;
            return;
        }
        self.hub.remote.row_idx =
            crate::tui::selection::move_index(self.hub.remote.row_idx, count, forward);
    }

    pub(crate) fn set_remote_message(&mut self, is_error: bool, text: impl Into<String>) {
        self.hub.remote.message = Some(GraphActionMessage {
            is_error,
            text: text.into(),
        });
        self.hub.remote.message_at = Some(Instant::now());
    }

    /// Drop the status line once its TTL has passed. True when it cleared
    /// this tick (a visible change).
    pub(crate) fn dismiss_remote_message(&mut self) -> bool {
        if self.hub.remote.message.is_none() {
            return false;
        }
        let elapsed = self
            .hub
            .remote
            .message_at
            .map(|at| at.elapsed())
            .unwrap_or(Duration::MAX);
        if elapsed >= crate::tui::app::dialog::GRAPH_ACTION_MESSAGE_TTL {
            self.hub.remote.message = None;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::remote::panel::{DeviceView, TrustedView};
    use std::sync::Arc;

    fn test_app() -> App {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap()
    }

    fn device(id: &str) -> DeviceView {
        DeviceView {
            id: id.to_string(),
            label: format!("{id} laptop"),
            last_seen: None,
            live: false,
        }
    }

    #[test]
    fn parse_remote_view_round_trips_and_refuses_garbage() {
        let view = RemoteView {
            signed_in: true,
            login: Some("octocat".to_string()),
            enabled: true,
            relay: "connected".to_string(),
            live_devices: vec!["bfcba2b742a807bb".to_string()],
            devices: vec![device("bfcba2b742a807bb")],
            ..RemoteView::default()
        };
        let text = serde_json::to_string(&view).expect("serialize");

        let parsed = parse_remote_view(&text).expect("parse");
        assert!(parsed.signed_in);
        assert_eq!(parsed.login.as_deref(), Some("octocat"));
        assert!(parsed.enabled);
        assert_eq!(parsed.relay, "connected");
        assert_eq!(parsed.live_devices, vec!["bfcba2b742a807bb".to_string()]);
        assert_eq!(parsed.devices.len(), 1);

        assert!(
            parse_remote_view("not json").is_err(),
            "garbage must not parse"
        );
        assert!(
            parse_pair_view("not json").is_err(),
            "garbage must not parse as a pair either"
        );
    }

    #[test]
    fn remote_rows_are_the_four_fixed_rows_then_one_per_device() {
        let mut view = RemoteView::default();
        assert_eq!(
            remote_rows(&view),
            vec![
                RemoteRow::GitHub,
                RemoteRow::Control,
                RemoteRow::Relay,
                RemoteRow::Pair,
            ]
        );
        view.devices = vec![device("a"), device("b")];
        assert_eq!(
            remote_rows(&view),
            vec![
                RemoteRow::GitHub,
                RemoteRow::Control,
                RemoteRow::Relay,
                RemoteRow::Pair,
                RemoteRow::Device(0),
                RemoteRow::Device(1),
            ]
        );
    }

    #[test]
    fn enter_on_control_while_signed_out_says_so_and_writes_nothing() {
        let mut app = test_app();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        assert!(!app.hub.remote.view.signed_in);
        app.hub.remote.row_idx = 1; // Control.

        app.hub_remote_key(KeyCode::Enter);

        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(message.is_error);
        assert!(message.text.contains("Sign in"), "{}", message.text);
        assert!(
            app.hub.remote.write_rx.is_none(),
            "disabled means no write is spawned"
        );
    }

    #[test]
    fn enter_on_control_while_signed_in_spawns_the_write() {
        let mut app = test_app();
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        app.hub.remote.view.signed_in = true;
        app.hub.remote.view.login = Some("octocat".to_string());
        app.hub.remote.view.enabled = false;
        app.hub.remote.row_idx = 1; // Control.

        app.hub_remote_key(KeyCode::Enter);

        assert!(
            app.hub.remote.write_rx.is_some(),
            "signed in means the toggle write is spawned"
        );
    }

    #[test]
    fn revoke_arms_only_on_device_rows_and_cancel_stands_down() {
        let mut app = test_app();
        app.hub.remote.view.devices = vec![device("bfcba2b742a807bb")];

        app.hub.remote.row_idx = 0; // GitHub.
        app.hub_remote_key(KeyCode::Char('r'));
        assert_eq!(app.hub.remote.revoke_confirm, None);

        app.hub.remote.row_idx = 4; // Device(0).
        app.hub_remote_key(KeyCode::Char('r'));
        assert_eq!(
            app.hub.remote.revoke_confirm.as_deref(),
            Some("bfcba2b742a807bb")
        );

        app.cancel_remote_revoke();
        assert_eq!(app.hub.remote.revoke_confirm, None);
    }

    fn trusted(login: &str) -> TrustedView {
        TrustedView {
            login: login.to_string(),
            all_sessions: false,
            sessions: vec!["alpha".to_string()],
        }
    }

    #[test]
    fn trusted_rows_follow_devices_and_r_arms_only_on_trusted_rows() {
        let mut app = test_app();
        app.hub.remote.view.devices = vec![device("bfcba2b742a807bb")];
        app.hub.remote.view.trusted = vec![trusted("guest")];
        assert_eq!(remote_rows(&app.hub.remote.view)[5], RemoteRow::Trusted(0));

        app.hub.remote.row_idx = 0; // GitHub.
        app.hub_remote_key(KeyCode::Char('r'));
        assert_eq!(app.hub.remote.trust_remove_confirm, None);

        app.hub.remote.row_idx = 4; // Device(0).
        app.hub_remote_key(KeyCode::Char('r'));
        assert_eq!(app.hub.remote.trust_remove_confirm, None);
        assert!(app.hub.remote.revoke_confirm.is_some());

        app.hub.remote.row_idx = 5; // Trusted(0).
        app.hub_remote_key(KeyCode::Char('r'));
        assert_eq!(
            app.hub.remote.trust_remove_confirm.as_deref(),
            Some("guest")
        );
        assert_eq!(
            app.hub.remote.revoke_confirm, None,
            "arming one confirm stands the other down"
        );

        app.cancel_trust_remove();
        assert_eq!(app.hub.remote.trust_remove_confirm, None);
    }

    #[test]
    fn trust_form_types_toggles_and_submits_through_the_write() {
        let mut app = test_app();
        app.hub_remote_key(KeyCode::Char('t'));
        assert!(app.hub.remote.trust.open);

        for c in "guest".chars() {
            app.hub_remote_key(KeyCode::Char(c));
        }
        app.hub_remote_key(KeyCode::Tab);
        for c in "alpha, beta".chars() {
            app.hub_remote_key(KeyCode::Char(c));
        }
        app.hub_remote_key(KeyCode::Tab);
        app.hub_remote_key(KeyCode::Char('a'));
        assert!(app.hub.remote.trust.all);
        app.hub_remote_key(KeyCode::Char('a'));
        assert!(!app.hub.remote.trust.all);

        app.hub_remote_key(KeyCode::Enter);
        assert!(!app.hub.remote.trust.open, "submit closes the form");
        assert!(
            app.hub.remote.write_rx.is_some(),
            "submit saves through the daemon"
        );

        // An empty login refuses to submit and keeps the form open.
        app.hub_remote_key(KeyCode::Char('t'));
        app.hub_remote_key(KeyCode::Enter);
        assert!(app.hub.remote.trust.open);
        assert!(
            app.hub.remote.message.as_ref().is_some_and(|m| m.is_error),
            "an empty login reports inline"
        );
        app.hub_remote_key(KeyCode::Esc);
        assert!(!app.hub.remote.trust.open);
    }

    #[test]
    fn enter_on_a_trusted_row_edits_that_grant() {
        let mut app = test_app();
        app.hub.remote.view.trusted = vec![trusted("guest")];
        app.hub.remote.row_idx = 4; // Trusted(0), no devices.
        app.hub_remote_key(KeyCode::Enter);
        assert!(app.hub.remote.trust.open);
        assert_eq!(app.hub.remote.trust.login, "guest");
        assert_eq!(app.hub.remote.trust.sessions, "alpha");
    }

    #[test]
    fn trust_results_report_inline_and_refresh_on_success() {
        let mut app = test_app();
        app.apply_remote_write(RemoteWriteResult::TrustAdd(Ok("guest".to_string())));
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(!message.is_error);
        assert!(message.text.contains("Trusted"), "{}", message.text);
        assert!(app.hub.remote.stale, "success refreshes the status");

        app.apply_remote_write(RemoteWriteResult::TrustRemove(Err(
            "no trusted profile for 'guest'".to_string(),
        )));
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(message.is_error, "the exact error reports inline");
    }

    #[test]
    fn disconnect_spawns_the_write_on_device_rows_and_ignores_other_rows() {
        let mut app = test_app();
        app.hub.remote.view.devices = vec![device("bfcba2b742a807bb")];

        app.hub.remote.row_idx = 0; // GitHub.
        app.hub_remote_key(KeyCode::Char('d'));
        assert!(
            app.hub.remote.write_rx.is_none(),
            "d off a device row writes nothing"
        );

        app.hub.remote.row_idx = 4; // Device(0).
        app.hub_remote_key(KeyCode::Char('d'));
        assert!(
            app.hub.remote.write_rx.is_some(),
            "d on a device row disconnects at once, with no confirm"
        );
        assert_eq!(app.hub.remote.revoke_confirm, None);
    }

    #[test]
    fn disconnect_results_report_inline_and_refresh_on_success() {
        let mut app = test_app();

        app.apply_remote_write(RemoteWriteResult::Disconnect(Ok(
            "bfcba2b742a807bb".to_string()
        )));
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(!message.is_error);
        assert!(message.text.contains("Disconnected"), "{}", message.text);
        assert!(app.hub.remote.stale, "success refreshes the status");

        app.apply_remote_write(RemoteWriteResult::Disconnect(Err(
            "no paired device with id bfcba2b742a807bb".to_string(),
        )));
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(message.is_error, "the exact error reports inline");
        assert!(
            message.text.contains("no paired device"),
            "{}",
            message.text
        );
    }

    #[test]
    fn revoke_results_report_inline_and_keep_the_row_until_a_refresh() {
        let mut app = test_app();
        app.hub.remote.view.devices = vec![device("bfcba2b742a807bb")];

        app.apply_remote_write(RemoteWriteResult::Revoke(
            Ok("bfcba2b742a807bb".to_string()),
        ));
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(!message.is_error);
        assert!(message.text.contains("Revoked"), "{}", message.text);
        assert!(app.hub.remote.stale, "success refreshes the status");
        assert_eq!(
            app.hub.remote.view.devices.len(),
            1,
            "the row leaves only when the refresh confirms it"
        );

        // A failed revoke reports the exact error inline and refreshes
        // nothing: the row stays exactly as the daemon last reported it,
        // never looking revoked.
        app.hub.remote.stale = false;
        app.apply_remote_write(RemoteWriteResult::Revoke(Err(
            "no paired device with id bfcba2b742a807bb".to_string(),
        )));
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(message.is_error, "the exact error reports inline");
        assert!(
            message.text.contains("no paired device"),
            "{}",
            message.text
        );
        assert!(!app.hub.remote.stale, "failure refreshes nothing");
        assert_eq!(app.hub.remote.view.devices.len(), 1);
        assert_eq!(app.hub.remote.view.devices[0].id, "bfcba2b742a807bb");
    }

    #[test]
    fn pair_overlay_opens_only_with_a_code_and_esc_hides_it() {
        let mut app = test_app();
        app.open_pair_overlay();
        assert!(!app.hub.remote.pair_open, "no code, no overlay");

        app.hub.remote.pair = Some(PairView {
            code: "7K3QH-M9P2X".to_string(),
            link: "https://remote.univerlab.org/pair#7K3QH-M9P2X".to_string(),
            expires_at: chrono::Utc::now().timestamp() + 600,
            qr: "qr".to_string(),
        });
        app.open_pair_overlay();
        assert!(app.hub.remote.pair_open);

        // Keys under the overlay do nothing (Esc is routed to the close).
        app.hub.remote.row_idx = 0;
        app.hub_remote_key(KeyCode::Down);
        assert_eq!(app.hub.remote.row_idx, 0);

        app.close_pair_overlay();
        assert!(!app.hub.remote.pair_open);
        assert!(app.hub.remote.pair.is_some(), "the code stays");
    }

    #[test]
    fn first_refresh_arms_the_throttle_without_calling_the_daemon() {
        let mut app = test_app();
        app.daemon_running = true;

        app.refresh_remote_status();
        assert!(
            app.hub.remote.rows_rx.is_none(),
            "`App::new` must not call the daemon by itself"
        );
        assert!(
            app.hub.remote.last_fetch.is_some(),
            "the first pass arms the throttle"
        );

        app.remote_request_refresh();
        app.refresh_remote_status();
        assert!(
            app.hub.remote.rows_rx.is_some(),
            "an explicit request still fetches immediately"
        );
    }

    #[test]
    fn remote_throttle_30s_idle_2s_on_tab() {
        assert_eq!(REMOTE_REFRESH_IDLE, Duration::from_secs(30));
        assert_eq!(REMOTE_REFRESH_ON_TAB, Duration::from_secs(2));

        let mut app = test_app();
        app.daemon_running = true;

        // Idle (hub closed): 10 s still throttled, 31 s fetches.
        app.hub.remote.last_fetch = Some(Instant::now() - Duration::from_secs(10));
        app.refresh_remote_status();
        assert!(
            app.hub.remote.rows_rx.is_none(),
            "10 s idle must still throttle at a 30 s cadence"
        );

        app.hub.remote.last_fetch = Some(Instant::now() - Duration::from_secs(31));
        app.refresh_remote_status();
        assert!(
            app.hub.remote.rows_rx.is_some(),
            "31 s idle must fetch at a 30 s cadence"
        );
        app.hub.remote.rows_rx = None;

        // On the Remote tab: 3 s fetches, a fresh fetch throttles.
        app.open_hub();
        app.set_hub_tab(HubTab::Remote);
        app.hub.remote.last_fetch = Some(Instant::now() - Duration::from_secs(3));
        app.refresh_remote_status();
        assert!(
            app.hub.remote.rows_rx.is_some(),
            "3 s on the tab must fetch at a 2 s cadence"
        );
        app.hub.remote.rows_rx = None;

        app.hub.remote.last_fetch = Some(Instant::now());
        app.refresh_remote_status();
        assert!(
            app.hub.remote.rows_rx.is_none(),
            "a fresh fetch on the tab must throttle"
        );
    }

    #[test]
    fn login_poll_stores_the_code_then_ends_on_the_result() {
        let mut app = test_app();
        let (tx, rx) = mpsc::channel();
        app.hub.remote.login = LoginFlow::Waiting { code: None, rx };
        tx.send(LoginEvent::Code(DeviceCodeInfo {
            user_code: "ABCD-1234".to_string(),
            verification_uri_complete: None,
        }))
        .expect("send");
        app.poll_remote_login();
        let LoginFlow::Waiting { code, .. } = &app.hub.remote.login else {
            panic!("the flow stays waiting on a code");
        };
        assert_eq!(code.as_ref().expect("code").user_code, "ABCD-1234");

        tx.send(LoginEvent::Done(Ok("octocat".to_string())))
            .expect("send");
        app.poll_remote_login();
        assert!(matches!(app.hub.remote.login, LoginFlow::Idle));
        assert!(app.hub.remote.stale, "a login refreshes the status");
        let message = app.hub.remote.message.as_ref().expect("a message");
        assert!(!message.is_error);
        assert!(message.text.contains("octocat"), "{}", message.text);
    }

    /// The TTL dismissal reports whether it cleared: no message or a fresh
    /// one returns false, an expired one clears and returns true so the tick
    /// redraws without it.
    #[test]
    fn dismiss_remote_message_reports_whether_it_cleared() {
        let mut app = test_app();
        assert!(!app.dismiss_remote_message(), "no message must not clear");

        app.set_remote_message(false, "paired");
        assert!(
            !app.dismiss_remote_message(),
            "a fresh message must survive"
        );

        app.hub.remote.message_at =
            Some(std::time::Instant::now() - std::time::Duration::from_secs(30));
        assert!(
            app.dismiss_remote_message(),
            "an expired message must clear"
        );
        assert!(app.hub.remote.message.is_none());
        assert!(
            !app.dismiss_remote_message(),
            "a cleared message must not clear again"
        );
    }
}
