//! Graph run-time controls — run/pause/continue/reset/autorun for the graph
//! currently focused in the TUI's live view.
//!
//! Every action dispatches through the daemon's MCP tools via
//! [`crate::tui::mcp_client::call_daemon_tool`] — the exact same
//! daemon-delegating path `daemon/graph_cli.rs` uses for the CLI's
//! `graph run`/`pause`/`continue`/`reset`/`autorun` subcommands — never the
//! database directly. The call itself runs on a background thread so the UI
//! thread never blocks on the daemon's HTTP round-trip; [`App::poll_graph_action`]
//! picks up the result non-blockingly on the next tick, exactly like
//! `App::poll_playground_search`'s `mpsc` pattern.

use crate::application::ports::StateRepository;
use crate::domain::graphs::GraphStatus;
use crate::tui::app::dialog::datetime_picker::{local_naive_to_utc, DateTimeEdit};
use crate::tui::app::types::App;
use crate::tui::mcp_client;

/// Outcome of a background graph-control dispatch, delivered to the main
/// thread via `App::graph_action_rx` and applied by [`App::poll_graph_action`].
pub(crate) struct GraphActionOutcome {
    pub is_error: bool,
    pub text: String,
}

/// Last-shown result of a graph control action — the daemon's response,
/// verbatim, whether it succeeded or was refused.
pub(crate) struct GraphActionMessage {
    pub is_error: bool,
    pub text: String,
}

/// How long a graph-action result banner stays up before auto-dismissing
/// (mirrors `show_copied`/`dismiss_copied`'s 2s, held a bit longer since
/// these messages are often full daemon sentences, not a one-word toast).
const GRAPH_ACTION_MESSAGE_TTL: std::time::Duration = std::time::Duration::from_secs(6);

/// One control offered for a graph in a given [`GraphStatus`] — the source of
/// truth for both the footer's discoverability hints and the key handler's
/// gating, so the two can never disagree about what's currently valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphControlAction {
    Run,
    Pause,
    ContinueRetry,
    ContinueSkip,
    Reset,
}

impl GraphControlAction {
    pub(crate) fn key(self) -> &'static str {
        match self {
            GraphControlAction::Run => "r",
            GraphControlAction::Pause => "p",
            GraphControlAction::ContinueRetry => "c",
            GraphControlAction::ContinueSkip => "C",
            GraphControlAction::Reset => "x",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            GraphControlAction::Run => "run",
            GraphControlAction::Pause => "pause",
            GraphControlAction::ContinueRetry => "continue (retry)",
            GraphControlAction::ContinueSkip => "continue (skip spec)",
            GraphControlAction::Reset => "reset",
        }
    }
}

/// Controls valid for a graph currently in `status` — a running graph offers
/// only pause; a paused one offers both continue modes (retrying the
/// in-flight node vs. abandoning the current spec are not interchangeable,
/// so both are always presented together); a completed or failed one offers
/// reset and run; a draft (never launched) graph offers only run.
pub(crate) fn available_graph_actions(status: GraphStatus) -> Vec<GraphControlAction> {
    match status {
        GraphStatus::Running | GraphStatus::Pausing => vec![GraphControlAction::Pause],
        GraphStatus::Paused => vec![
            GraphControlAction::ContinueRetry,
            GraphControlAction::ContinueSkip,
        ],
        GraphStatus::Completed | GraphStatus::Failed => {
            vec![GraphControlAction::Reset, GraphControlAction::Run]
        }
        GraphStatus::Draft => vec![GraphControlAction::Run],
    }
}

/// Which of the autorun dialog's two input modes is active. Toggled with
/// Tab, mirroring the prompt builder's `send_toggle`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GraphAutorunMode {
    /// Pick a local date/time with the shared picker — becomes `at`.
    Picker,
    /// Type a raw quota-reset message for the daemon to parse — sent
    /// verbatim as `quota_reset_message`. Submitting this mode empty
    /// cancels any pending autorun instead.
    QuotaMessage,
}

/// Autorun scheduling input for the graph currently focused in the live view,
/// mirroring the CLI's `--at`/`--quota-reset-message` split: picking a time
/// with [`Self::picker`] (the same [`DateTimeEdit`] widget the prompt
/// builder's scheduled-send control uses) is sent as `at`; typing into
/// [`Self::quota_input`] is sent as the raw `quota_reset_message` for the
/// daemon to parse (the engine, never the TUI, computes the resulting
/// instant from that text); submitting the quota-message mode empty cancels
/// any pending autorun.
pub(crate) struct GraphAutorunDialog {
    pub graph_id: String,
    pub graph_name: String,
    pub mode: GraphAutorunMode,
    pub picker: DateTimeEdit,
    pub quota_input: String,
    /// Inline validation hint (e.g. a picked time already in the past).
    pub error: Option<String>,
}

impl GraphAutorunDialog {
    /// Seed the picker from an existing pending `autorun_at` (converted to
    /// local time) so reopening the dialog on a graph that already has one
    /// scheduled shows it, rather than an empty/now-seeded field. With none
    /// pending, seed with the current local time (seconds zeroed), mirroring
    /// the prompt builder's `send_begin_edit`.
    pub fn new(
        graph_id: String,
        graph_name: String,
        existing_autorun_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Self {
        use chrono::Timelike;
        let seed = existing_autorun_at
            .map(|at| at.with_timezone(&chrono::Local).naive_local())
            .unwrap_or_else(|| {
                let now = chrono::Local::now().naive_local();
                now.with_second(0)
                    .and_then(|t| t.with_nanosecond(0))
                    .unwrap_or(now)
            });
        Self {
            graph_id,
            graph_name,
            mode: GraphAutorunMode::Picker,
            picker: DateTimeEdit::new(seed),
            quota_input: String::new(),
            error: None,
        }
    }

    /// Toggle between the picker and the free-text quota-message input.
    pub fn toggle_mode(&mut self) {
        self.error = None;
        self.mode = match self.mode {
            GraphAutorunMode::Picker => GraphAutorunMode::QuotaMessage,
            GraphAutorunMode::QuotaMessage => GraphAutorunMode::Picker,
        };
    }

    /// The picker's currently displayed value, resolved to its UTC instant —
    /// shown to the user before submission so the local-to-UTC conversion is
    /// visible rather than trusted (requirement 3).
    pub fn picker_resulting_utc(&self) -> chrono::DateTime<chrono::Utc> {
        local_naive_to_utc(self.picker.value)
    }
}

impl App {
    /// The graph these run-time controls act on: the selected graph, but only
    /// from the live (non-archived) list — an archived graph is inert until
    /// restored, so every control here is a no-op while browsing the
    /// archive.
    fn actionable_selected_graph(&self) -> Option<&crate::domain::graphs::Graph> {
        if self.graph_view_archived {
            return None;
        }
        self.selected_graph()
    }

    /// Dispatch one graph-control MCP tool call on a background thread. A
    /// no-op while a previous dispatch is still in flight, so a held key (or
    /// a fast double-press) can't race two calls against the same graph.
    pub(crate) fn dispatch_graph_action(
        &mut self,
        tool: &'static str,
        arguments: serde_json::Value,
    ) {
        if self.graph_action_pending {
            return;
        }
        self.graph_action_message = None;
        let port = self
            .db
            .get_state("port")
            .ok()
            .flatten()
            .unwrap_or_else(|| "7755".to_string());

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = match mcp_client::call_daemon_tool(&port, tool, &arguments) {
                Ok(outcome) => GraphActionOutcome {
                    is_error: outcome.is_error,
                    text: outcome.text,
                },
                Err(e) => GraphActionOutcome {
                    is_error: true,
                    text: format!("Daemon call failed: {e:#}"),
                },
            };
            let _ = tx.send(outcome);
        });
        self.graph_action_pending = true;
        self.graph_action_rx = Some(rx);
    }

    /// Apply a finished background graph-action dispatch, if any. Called from
    /// the tick graph; never blocks (mirrors `App::poll_playground_search`).
    pub(crate) fn poll_graph_action(&mut self) {
        let Some(rx) = &self.graph_action_rx else {
            return;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.graph_action_pending = false;
                self.graph_action_rx = None;
                return;
            }
        };
        self.graph_action_pending = false;
        self.graph_action_rx = None;
        let is_error = outcome.is_error;
        self.graph_action_message = Some(GraphActionMessage {
            is_error,
            text: outcome.text,
        });
        self.graph_action_message_at = std::time::Instant::now();
        if !is_error {
            let _ = self.refresh_graphs();
        }
    }

    /// Clear an old graph-action result banner once its TTL has elapsed
    /// (mirrors `App::dismiss_copied`).
    pub(crate) fn dismiss_graph_action_message(&mut self) {
        if self.graph_action_message.is_some()
            && self.graph_action_message_at.elapsed() > GRAPH_ACTION_MESSAGE_TTL
        {
            self.graph_action_message = None;
        }
    }

    /// Run the selected graph (`r`) — valid for a `draft`, `completed`, or
    /// `failed` graph (see [`available_graph_actions`]); a no-op otherwise.
    ///
    /// (CB22) Pure dispatch: sends `graph_run` with only `graph_id` — never a
    /// `queue_id`, never an `idea`, and never a spec-creation request. A graph
    /// with neither bound specs nor a selected queue is refused by the
    /// daemon's launch validation (surfacing through
    /// `dispatch_graph_action`'s error banner), not papered over with a blank
    /// spec row. There is no MCP-dispatch seam to unit-test the payload
    /// here, so the user-visible refusal is covered at the handler/engine
    /// boundary instead.
    pub fn run_selected_graph(&mut self) {
        let Some(lp) = self.actionable_selected_graph() else {
            return;
        };
        if !available_graph_actions(lp.status).contains(&GraphControlAction::Run) {
            return;
        }
        let graph_id = lp.id.clone();
        self.dispatch_graph_action("graph_run", serde_json::json!({ "graph_id": graph_id }));
    }

    /// Pause the selected graph (`p`) — valid only while it is `running`.
    pub fn pause_selected_graph(&mut self) {
        let Some(lp) = self.actionable_selected_graph() else {
            return;
        };
        if !available_graph_actions(lp.status).contains(&GraphControlAction::Pause) {
            return;
        }
        let graph_id = lp.id.clone();
        self.dispatch_graph_action("graph_pause", serde_json::json!({ "graph_id": graph_id }));
    }

    /// Continue the selected paused graph, retrying the node that was
    /// mid-flight when it paused (`c`).
    pub fn continue_selected_graph_retry(&mut self) {
        self.continue_selected_graph(GraphControlAction::ContinueRetry, "retry_current_node");
    }

    /// Continue the selected paused graph, abandoning the current spec and
    /// moving on to the next one (`C`).
    pub fn continue_selected_graph_skip(&mut self) {
        self.continue_selected_graph(GraphControlAction::ContinueSkip, "skip_next_spec");
    }

    fn continue_selected_graph(&mut self, action: GraphControlAction, daemon_action: &'static str) {
        let Some(lp) = self.actionable_selected_graph() else {
            return;
        };
        if !available_graph_actions(lp.status).contains(&action) {
            return;
        }
        let graph_id = lp.id.clone();
        self.dispatch_graph_action(
            "graph_continue",
            serde_json::json!({ "graph_id": graph_id, "action": daemon_action }),
        );
    }

    /// Open the reset confirmation for the selected graph (`x`) — valid only
    /// for a `completed`/`failed` graph. A no-op otherwise, so the modal never
    /// opens for a graph reset can't act on.
    pub fn open_graph_reset_confirm(&mut self) {
        let Some(lp) = self.actionable_selected_graph() else {
            return;
        };
        if !available_graph_actions(lp.status).contains(&GraphControlAction::Reset) {
            return;
        }
        self.graph_reset_confirm = true;
    }

    /// Dispatch `graph_reset` for the selected graph after the confirm modal's
    /// `y`/Enter. Called only while `graph_reset_confirm` is set, so the
    /// selected graph is whatever `open_graph_reset_confirm` validated.
    pub fn confirm_reset_selected_graph(&mut self) {
        let Some(lp) = self.actionable_selected_graph() else {
            return;
        };
        let graph_id = lp.id.clone();
        self.dispatch_graph_action("graph_reset", serde_json::json!({ "graph_id": graph_id }));
    }

    /// Open the autorun-scheduling input for the graph currently focused in
    /// the live view (`a`). Available regardless of the graph's status — the
    /// underlying `graph_schedule_autorun` tool imposes no status guard
    /// (cancelling in particular must work "regardless of the graph's current
    /// status", per its own description).
    pub fn open_graph_autorun_dialog(&mut self) {
        let Some(lp) = self.actionable_selected_graph() else {
            return;
        };
        self.graph_autorun_dialog = Some(GraphAutorunDialog::new(
            lp.id.clone(),
            lp.name.clone(),
            lp.autorun_at,
        ));
    }

    /// Close the autorun dialog without submitting.
    pub fn close_graph_autorun_dialog(&mut self) {
        self.graph_autorun_dialog = None;
    }

    /// Submit the autorun dialog: in [`GraphAutorunMode::Picker`], the
    /// picker's currently displayed local time is converted to UTC and sent
    /// as `at` — a past instant is refused and the dialog stays open with an
    /// inline error instead of submitting (requirement 7). In
    /// [`GraphAutorunMode::QuotaMessage`], non-empty text is sent as the raw
    /// `quota_reset_message` for the daemon to parse; empty text cancels any
    /// pending autorun. The TUI never computes an instant from that text
    /// itself — only from what the user explicitly picked.
    pub fn submit_graph_autorun_dialog(&mut self) {
        let Some(mut dialog) = self.graph_autorun_dialog.take() else {
            return;
        };
        match autorun_dialog_arguments(&dialog, chrono::Utc::now()) {
            Ok(arguments) => self.dispatch_graph_action("graph_schedule_autorun", arguments),
            Err(error) => {
                dialog.error = Some(error);
                self.graph_autorun_dialog = Some(dialog);
            }
        }
    }
}

/// Compute the `graph_schedule_autorun` arguments for the dialog's current
/// state, or the inline error to show instead of submitting. `now` is
/// injected so the past/future boundary (requirement 7) is testable without
/// depending on wall-clock time. Pure and `App`-free so the three submit
/// paths (pick a time, cancel, quota message) can be tested directly against
/// the produced JSON — the regression guard for decision 3.
fn autorun_dialog_arguments(
    dialog: &GraphAutorunDialog,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<serde_json::Value, String> {
    match dialog.mode {
        GraphAutorunMode::Picker => {
            let at = dialog.picker_resulting_utc();
            if at <= now {
                return Err("picked time is in the past".to_string());
            }
            Ok(serde_json::json!({
                "graph_id": dialog.graph_id,
                "at": at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            }))
        }
        GraphAutorunMode::QuotaMessage => {
            let text = dialog.quota_input.trim();
            if text.is_empty() {
                Ok(serde_json::json!({ "graph_id": dialog.graph_id }))
            } else {
                Ok(serde_json::json!({ "graph_id": dialog.graph_id, "quota_reset_message": text }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_actions_running_offers_only_pause() {
        assert_eq!(
            available_graph_actions(GraphStatus::Running),
            vec![GraphControlAction::Pause]
        );
    }

    #[test]
    fn available_actions_paused_offers_both_continue_modes() {
        assert_eq!(
            available_graph_actions(GraphStatus::Paused),
            vec![
                GraphControlAction::ContinueRetry,
                GraphControlAction::ContinueSkip
            ]
        );
    }

    #[test]
    fn available_actions_completed_and_failed_offer_reset_and_run() {
        for status in [GraphStatus::Completed, GraphStatus::Failed] {
            assert_eq!(
                available_graph_actions(status),
                vec![GraphControlAction::Reset, GraphControlAction::Run]
            );
        }
    }

    #[test]
    fn available_actions_draft_offers_only_run() {
        assert_eq!(
            available_graph_actions(GraphStatus::Draft),
            vec![GraphControlAction::Run]
        );
    }

    #[test]
    fn action_keys_are_distinct() {
        let actions = [
            GraphControlAction::Run,
            GraphControlAction::Pause,
            GraphControlAction::ContinueRetry,
            GraphControlAction::ContinueSkip,
            GraphControlAction::Reset,
        ];
        let keys: std::collections::HashSet<&str> = actions.iter().map(|a| a.key()).collect();
        assert_eq!(keys.len(), actions.len());
    }

    // ── autorun dialog: submit paths (C18) ─────────────────────────

    fn future_dialog() -> GraphAutorunDialog {
        let mut dialog =
            GraphAutorunDialog::new("lp1".to_string(), "Nightly review".to_string(), None);
        dialog.picker.value = chrono::Local::now().naive_local() + chrono::Duration::hours(2);
        dialog
    }

    #[test]
    fn picker_mode_future_time_produces_at_argument() {
        let dialog = future_dialog();
        let arguments = autorun_dialog_arguments(&dialog, chrono::Utc::now()).unwrap();
        assert!(arguments.get("at").is_some(), "{arguments}");
        assert!(
            arguments.get("quota_reset_message").is_none(),
            "{arguments}"
        );
        assert_eq!(arguments["graph_id"], "lp1");
    }

    #[test]
    fn picker_mode_past_time_is_rejected() {
        let mut dialog = future_dialog();
        dialog.picker.value = chrono::Local::now().naive_local() - chrono::Duration::hours(1);
        let err = autorun_dialog_arguments(&dialog, chrono::Utc::now()).unwrap_err();
        assert!(err.contains("past"), "{err}");
    }

    #[test]
    fn picker_mode_future_time_is_accepted() {
        let dialog = future_dialog();
        assert!(autorun_dialog_arguments(&dialog, chrono::Utc::now()).is_ok());
    }

    #[test]
    fn quota_message_mode_empty_produces_cancel_argument() {
        let mut dialog = future_dialog();
        dialog.mode = GraphAutorunMode::QuotaMessage;
        dialog.quota_input = "   ".to_string();
        let arguments = autorun_dialog_arguments(&dialog, chrono::Utc::now()).unwrap();
        assert_eq!(arguments, serde_json::json!({ "graph_id": "lp1" }));
    }

    #[test]
    fn quota_message_mode_nonparsing_text_is_sent_unparsed() {
        let mut dialog = future_dialog();
        dialog.mode = GraphAutorunMode::QuotaMessage;
        dialog.quota_input = "resets 2:10am (America/Bogota)".to_string();
        let arguments = autorun_dialog_arguments(&dialog, chrono::Utc::now()).unwrap();
        assert_eq!(
            arguments["quota_reset_message"],
            "resets 2:10am (America/Bogota)"
        );
        assert!(arguments.get("at").is_none(), "{arguments}");
    }

    #[test]
    fn three_submit_paths_produce_distinguishable_arguments() {
        let now = chrono::Utc::now();

        let mut cancel = future_dialog();
        cancel.mode = GraphAutorunMode::QuotaMessage;
        cancel.quota_input = String::new();
        let cancel_args = autorun_dialog_arguments(&cancel, now).unwrap();

        let mut quota = future_dialog();
        quota.mode = GraphAutorunMode::QuotaMessage;
        quota.quota_input = "resets 1pm".to_string();
        let quota_args = autorun_dialog_arguments(&quota, now).unwrap();

        let picked = future_dialog();
        let picked_args = autorun_dialog_arguments(&picked, now).unwrap();

        assert!(
            cancel_args.get("at").is_none() && cancel_args.get("quota_reset_message").is_none()
        );
        assert!(quota_args.get("quota_reset_message").is_some() && quota_args.get("at").is_none());
        assert!(
            picked_args.get("at").is_some() && picked_args.get("quota_reset_message").is_none()
        );
    }

    #[test]
    fn opening_dialog_seeds_picker_from_existing_pending_autorun() {
        let existing = chrono::Utc::now() + chrono::Duration::hours(3);
        let dialog = GraphAutorunDialog::new(
            "lp1".to_string(),
            "Nightly review".to_string(),
            Some(existing),
        );
        assert_eq!(
            dialog.picker.value,
            existing.with_timezone(&chrono::Local).naive_local()
        );
    }

    #[test]
    fn opening_dialog_without_pending_autorun_seeds_current_time() {
        let dialog = GraphAutorunDialog::new("lp1".to_string(), "Nightly review".to_string(), None);
        let now = chrono::Local::now().naive_local();
        assert!((dialog.picker.value - now).num_minutes().abs() < 2);
    }
}
