//! CT30: state and `App` methods for the Crews panel face.
//!
//! The face reads ONLY through the daemon MCP client (D4): every refresh and
//! every write runs on a background `std::thread` with an `mpsc` channel
//! polled from the refresh tick (D3), so rendering never blocks on a
//! round-trip — while a fetch or a write is in flight the renderer shows
//! `refreshing…` over the last good snapshot. The one database read in
//! this file is `crews_port`, the port lookup every daemon dispatch uses.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use chrono::Utc;

use super::crews_calls;
use super::crews_dialog::CrewsDialog;
use super::crews_logic::{
    build_pair_rows, format_crew_write_message, CrewBindingView, CrewView, CrewsSnapshot, PairRow,
};
use super::types::{App, PanelFace};
use crate::application::ports::StateRepository;
use crate::domain::graphs::GraphStatus;
use crate::tui::app::dialog::GraphActionMessage;

/// Minimum gap between two snapshot fetches while the face is visible (D5).
/// Becoming visible and every completed write bypass the throttle.
pub(crate) const CREWS_REFRESH_EVERY: Duration = Duration::from_secs(5);

/// Which of the face's two sections is showing (D17: Tab/BackTab toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CrewsSection {
    #[default]
    Pairs,
    Crews,
}

/// What a background write produced. `CrewEdit` carries the raw pieces
/// rather than a finished sentence so `poll_crews_write` can add D15's
/// running-graph note from live `App::graphs` state the worker cannot see.
#[derive(Debug, Clone)]
pub(crate) enum WriteOutcome {
    /// A roster write: the status line text, verbatim (success) or the
    /// error. Any completed write attempt refreshes — a batch aborts on its
    /// first error, so earlier writes in it may already have landed.
    Status { is_error: bool, text: String },
    /// A crew member edit succeeded: message formatted on the UI side.
    CrewEdit {
        crew_name: String,
        version: i64,
        bound_graph_ids: Vec<String>,
    },
}

/// One Enter-expand binding read in flight: `(crew_name, result)` (D16).
pub(crate) type BindingsMessage = (String, Result<Vec<CrewBindingView>, String>);

/// A background write job: it receives the daemon port and returns the
/// outcome the UI side formats.
pub(crate) type CrewsWriteWork = Box<dyn FnOnce(&str) -> Result<WriteOutcome, String> + Send>;

/// The Crews face's state: two sections (Pairs / Crews), the last good
/// snapshot, the in-flight channel handles, the status line and the open
/// dialog. All of it hangs off `App::crews`.
#[derive(Default)]
pub(crate) struct CrewsFaceState {
    pub section: CrewsSection,
    /// Last good data; kept (and rendered) while a refresh is in flight.
    pub snapshot: Option<CrewsSnapshot>,
    /// Pairs rows rebuilt from the snapshot on receipt.
    pub pair_rows: Vec<PairRow>,
    pub pair_idx: usize,
    pub pair_scroll: usize,
    pub crew_idx: usize,
    pub crew_scroll: usize,
    /// Selected member inside the expanded crew.
    pub member_idx: usize,
    /// Crew name shown expanded (`Enter` toggles it).
    pub expanded: Option<String>,
    pub snapshot_rx: Option<mpsc::Receiver<Result<CrewsSnapshot, String>>>,
    pub write_rx: Option<mpsc::Receiver<WriteOutcome>>,
    /// Enter-expand binding read (D16): `(crew_name, result)`.
    pub bindings_rx: Option<mpsc::Receiver<BindingsMessage>>,
    /// When the last snapshot fetch started — drives the 5s throttle.
    pub last_fetch: Option<Instant>,
    /// A refresh was requested while busy or throttled: run it as soon as
    /// the current fetch finishes.
    pub stale: bool,
    pub dialog: Option<CrewsDialog>,
    pub message: Option<GraphActionMessage>,
    pub message_at: Option<Instant>,
    /// List height of the last draw, written through `&App` by the
    /// renderer, read by the key handler to keep the selection visible.
    pub last_list_height: std::cell::Cell<usize>,
}

impl App {
    /// Request a snapshot refresh as soon as possible: clears the throttle
    /// so the next `refresh_crews_face` runs immediately (called on pin,
    /// after writes, and by the `r` key).
    pub(crate) fn request_crews_refresh(&mut self) {
        self.crews.stale = true;
        self.crews.last_fetch = None;
    }

    /// Spawn a snapshot fetch unless the face is hidden, a fetch is already
    /// in flight (`refreshing…` shows), or the 5s throttle still holds (D5).
    /// No-op unless the Crews face is the visible one (D2).
    pub(crate) fn refresh_crews_face(&mut self) {
        if self.panel_face != PanelFace::Crews {
            return;
        }
        if self.crews.snapshot_rx.is_some() {
            return;
        }
        if !self.crews.stale
            && self
                .crews
                .last_fetch
                .is_some_and(|at| at.elapsed() < CREWS_REFRESH_EVERY)
        {
            return;
        }
        let port = self.crews_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = crews_calls::fetch_snapshot(&port).map_err(|error| format!("{error:#}"));
            let _ = tx.send(result);
        });
        self.crews.snapshot_rx = Some(rx);
        self.crews.last_fetch = Some(Instant::now());
        self.crews.stale = false;
    }

    /// Apply a finished snapshot fetch (or an in-flight binding read), if
    /// any. Non-blocking; mirrors `poll_graph_action`.
    pub(crate) fn poll_crews_face(&mut self) {
        self.poll_crews_bindings();
        let Some(rx) = self.crews.snapshot_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(snapshot)) => self.apply_crews_snapshot(snapshot),
            Ok(Err(text)) => self.set_crews_message(true, format!("Refresh failed: {text}")),
            Err(mpsc::TryRecvError::Empty) => self.crews.snapshot_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Store a fresh snapshot and clamp every selection against it so a
    /// shrunken list can never leave the keys pointing past the end.
    fn apply_crews_snapshot(&mut self, mut snapshot: CrewsSnapshot) {
        // `crew_list` carries only binding counts, so the Enter-expand read
        // (D16) is the only source of a crew's binding list. Carry the
        // already-loaded bindings over to the new snapshot by name, or the
        // periodic refresh would blank the expanded row's bindings seconds
        // after the user expanded it. A changed count means the bindings
        // moved underneath us, so the stale list is dropped instead.
        if let Some(previous) = self.crews.snapshot.as_ref() {
            for crew in &mut snapshot.crews {
                if let Some(old) = previous.crews.iter().find(|old| old.name == crew.name) {
                    if !old.bindings.is_empty() && old.binding_total == crew.binding_total {
                        crew.bindings = old.bindings.clone();
                    }
                }
            }
        }
        let rows = build_pair_rows(&snapshot, &Utc::now());
        let row_count = rows.len();
        let crew_count = snapshot.crews.len();
        self.crews.pair_rows = rows;
        self.crews.snapshot = Some(snapshot);
        if self.crews.pair_idx >= row_count {
            self.crews.pair_idx = 0;
            self.crews.pair_scroll = 0;
        }
        if self.crews.crew_idx >= crew_count {
            self.crews.crew_idx = 0;
            self.crews.crew_scroll = 0;
        }
        let expanded = self.crews.expanded.clone();
        if let Some(name) = expanded {
            match self
                .crews
                .snapshot
                .as_ref()
                .and_then(|snap| snap.crews.iter().find(|crew| crew.name == name))
            {
                Some(crew) => {
                    if self.crews.member_idx >= crew.members.len() {
                        self.crews.member_idx = crew.members.len().saturating_sub(1);
                    }
                }
                None => {
                    self.crews.expanded = None;
                    self.crews.member_idx = 0;
                }
            }
        }
    }

    /// Apply a finished Enter-expand binding read: stored into the snapshot
    /// by crew name so the renderer can list the bindings (D16).
    fn poll_crews_bindings(&mut self) {
        let Some(rx) = self.crews.bindings_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok((name, result)) => match result {
                Ok(bindings) => {
                    if let Some(crew) = self
                        .crews
                        .snapshot
                        .as_mut()
                        .and_then(|snap| snap.crews.iter_mut().find(|crew| crew.name == name))
                    {
                        crew.bindings = bindings;
                    }
                }
                Err(text) => self.set_crews_message(true, text),
            },
            Err(mpsc::TryRecvError::Empty) => self.crews.bindings_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Apply a finished write: set the status line, add D15's running-graph
    /// note to a crew edit, and refresh the snapshot (any completed write
    /// attempt may have changed data).
    pub(crate) fn poll_crews_write(&mut self) {
        let Some(rx) = self.crews.write_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(WriteOutcome::Status { is_error, text }) => {
                self.set_crews_message(is_error, text);
                self.request_crews_refresh();
            }
            Ok(WriteOutcome::CrewEdit {
                crew_name,
                version,
                bound_graph_ids,
            }) => {
                let bound_running = !self.crew_running_note(&bound_graph_ids).is_empty();
                let text = format_crew_write_message(&crew_name, version, bound_running);
                self.set_crews_message(false, text);
                self.request_crews_refresh();
            }
            Err(mpsc::TryRecvError::Empty) => self.crews.write_rx = Some(rx),
            Err(mpsc::TryRecvError::Disconnected) => {}
        }
    }

    /// Run one write job on a background thread (D3). No-op while a
    /// previous write is still in flight, so a held key can't race two
    /// batches against the same roster. The closure captures owned data
    /// only and receives the daemon port.
    pub(crate) fn spawn_crews_write(&mut self, work: CrewsWriteWork) {
        if self.crews.write_rx.is_some() {
            self.set_crews_message(true, "A crews write is still running");
            return;
        }
        let port = self.crews_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let outcome = work(&port).unwrap_or_else(|text| WriteOutcome::Status {
                is_error: true,
                text,
            });
            let _ = tx.send(outcome);
        });
        self.crews.write_rx = Some(rx);
    }

    /// Read one crew's bindings on a background thread (D16, Enter-expand).
    /// No-op while a previous read is in flight.
    pub(crate) fn spawn_crews_bindings(&mut self, crew: String) {
        if self.crews.bindings_rx.is_some() {
            return;
        }
        let port = self.crews_port();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result =
                crews_calls::fetch_crew_bindings(&port, &crew).map_err(|e| format!("{e:#}"));
            let _ = tx.send((crew, result));
        });
        self.crews.bindings_rx = Some(rx);
    }

    /// The daemon port — the only database read the Crews face makes (D4:
    /// roster/crew/graph data comes exclusively from daemon tools).
    fn crews_port(&self) -> String {
        self.db
            .get_state("port")
            .ok()
            .flatten()
            .unwrap_or_else(|| "7755".to_string())
    }

    /// Set the face's status line (D14: same message shape and TTL as the
    /// graph-control banner).
    pub(crate) fn set_crews_message(&mut self, is_error: bool, text: impl Into<String>) {
        self.crews.message = Some(GraphActionMessage {
            is_error,
            text: text.into(),
        });
        self.crews.message_at = Some(Instant::now());
    }

    /// The pair row under the cursor in the Pairs section.
    pub(crate) fn selected_pair(&self) -> Option<&PairRow> {
        self.crews.pair_rows.get(self.crews.pair_idx)
    }

    /// The crew under the cursor in the Crews section.
    pub(crate) fn selected_crew(&self) -> Option<&CrewView> {
        self.crews.snapshot.as_ref()?.crews.get(self.crews.crew_idx)
    }

    /// D15's note: non-empty when any graph a crew is bound to is running.
    /// `crew_get`'s `bindings[].graph` carries the graph NAME (verified
    /// against `graph_name_for_scope`), so the match is on `lp.name`.
    pub(crate) fn crew_running_note(&self, bound: &[String]) -> &'static str {
        const NOTE: &str = " Bound running graphs pick it up at their next dispatch.";
        let running: Vec<&str> = self
            .graphs
            .iter()
            .filter(|graph| graph.status == GraphStatus::Running)
            .map(|graph| graph.name.as_str())
            .collect();
        if bound.iter().any(|name| running.contains(&name.as_str())) {
            NOTE
        } else {
            ""
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::app::crews_logic::{CrewBindingView, CrewView};
    use std::sync::Arc;
    use tempfile::{tempdir, NamedTempFile};

    fn test_app() -> App {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("create data dir");
        App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app")
    }

    fn binding(name: &str) -> CrewBindingView {
        CrewBindingView {
            graph: "g1".to_string(),
            kind: "ensemble".to_string(),
            name: name.to_string(),
        }
    }

    /// A one-member crew with the given loaded bindings and binding count.
    fn crew(name: &str, bindings: Vec<CrewBindingView>, binding_total: usize) -> CrewView {
        CrewView {
            name: name.to_string(),
            description: None,
            version: 1,
            members: vec![serde_json::json!({"platform": "claude", "model": null})],
            member_pairs: vec![("claude".to_string(), String::new())],
            bindings,
            binding_total,
        }
    }

    fn snapshot(crews: Vec<CrewView>) -> CrewsSnapshot {
        CrewsSnapshot {
            roster: Vec::new(),
            pairs: Vec::new(),
            crews,
        }
    }

    /// The refresh tick must not blank an expanded crew's bindings: the
    /// `crew_list` snapshot only counts them, so the value `crew_get` loaded
    /// on Enter has to survive (`crew_list` carries no binding list).
    #[test]
    fn refresh_keeps_loaded_bindings_on_the_expanded_crew() {
        let mut app = test_app();
        app.crews.snapshot = Some(snapshot(vec![crew(
            "review",
            vec![binding("reviewers")],
            1,
        )]));
        app.apply_crews_snapshot(snapshot(vec![crew("review", Vec::new(), 1)]));

        let crew = &app.crews.snapshot.as_ref().unwrap().crews[0];
        assert_eq!(
            crew.bindings.len(),
            1,
            "the loaded binding must survive the periodic refresh"
        );
        assert_eq!(crew.bindings[0].name, "reviewers");
    }

    /// A changed binding count means the bindings moved underneath us: the
    /// stale list is dropped rather than shown against the new count.
    #[test]
    fn refresh_drops_bindings_when_the_count_changed() {
        let mut app = test_app();
        app.crews.snapshot = Some(snapshot(vec![crew(
            "review",
            vec![binding("reviewers")],
            1,
        )]));
        app.apply_crews_snapshot(snapshot(vec![crew("review", Vec::new(), 2)]));

        let crew = &app.crews.snapshot.as_ref().unwrap().crews[0];
        assert!(
            crew.bindings.is_empty(),
            "a changed count invalidates the loaded list"
        );
    }
}
