//! Refresh cadence split: every-tick cheap work vs. throttled slow work.
//!
//! [`App::refresh_fast`] runs on every 50 ms tick and only touches in-memory
//! state plus non-blocking channel polls. [`App::refresh_slow`] runs at most
//! once per second (or when [`App::slow_force`] is armed by a pushed change)
//! and owns every database read, daemon round-trip and port probe the old
//! per-tick `refresh` performed. [`SlowFingerprint`] captures the slow
//! outputs so the loop redraws for the slow path only when something in it
//! actually changed.

use std::time::{Duration, Instant};

use super::super::agent::AgentStatus;
use super::dialog::GraphActionMessage;
use super::system_monitor::system_settling;
use super::*;
use crate::domain::graphs::GraphRunStatus;

// ── Slow fingerprint ────────────────────────────────────────────────

/// Visible slow-refresh outputs, captured before and after
/// [`App::refresh_slow`]: the tick redraws for the slow path only when the
/// fingerprint moved, so settled data never draws on its own (FR5).
#[derive(PartialEq, Eq)]
pub(crate) struct SlowFingerprint {
    agents_len: usize,
    graphs: Vec<(String, GraphStatus)>,
    daemon_running: bool,
    daemon_version: String,
    update_available: Option<String>,
    hub_notice: Option<String>,
    active_runs_len: usize,
    backlog_len: usize,
    projects_len: usize,
    project_knowledge_len: usize,
    project_graph_edges_len: usize,
    project_graph_trees_len: usize,
    archived_graph_count: usize,
    graph_specs: Vec<(String, GraphSpecStatus)>,
    graph_runs: Vec<(String, GraphRunStatus, i64)>,
    graph_live: Option<(GraphStatus, usize, usize, Option<String>)>,
    log_hash: u64,
    rag_queue_len: usize,
    rag_paused: bool,
    rag_model_loaded: bool,
    rag_info: (i64, i64, i64, i64),
    rag_files_len: usize,
    missions_unlocked: usize,
    playground_results_len: usize,
    playground_pending: bool,
    playground_executed_query: String,
    crews_snapshot: bool,
    crews_rows: u64,
    crews_message: Option<(bool, String)>,
    settings_rows: u64,
    settings_message: Option<(bool, String)>,
    remote_signed_in: bool,
    remote_login: Option<String>,
    remote_enabled: bool,
    remote_relay: String,
    remote_live_devices: Vec<String>,
    remote_devices_len: usize,
    remote_pair: bool,
    remote_message: Option<(bool, String)>,
}

/// `(is_error, text)` of a transient daemon-operation message, if one shows.
fn message_summary(message: &Option<GraphActionMessage>) -> Option<(bool, String)> {
    message
        .as_ref()
        .map(|message| (message.is_error, message.text.clone()))
}

/// Hash of the Crews face's pair rows: availability flips keep the row count
/// but must still dirty the tick.
fn crews_rows_digest(app: &App) -> u64 {
    let mut summary = String::new();
    for row in &app.crews.pair_rows {
        use std::fmt::Write as _;
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}|{}|{}",
            row.platform, row.model, row.available, row.expired, row.graph_count, row.crew_count,
        );
    }
    calculate_log_hash(&summary)
}

/// Hash of the Settings tab's rows: a refetch that changes values but not
/// the row count must still dirty the tick.
fn settings_rows_digest(app: &App) -> u64 {
    let mut summary = String::new();
    for row in &app.hub.settings.rows {
        use std::fmt::Write as _;
        let _ = writeln!(
            summary,
            "{}|{}|{}|{}",
            row.key, row.effective, row.source, row.status,
        );
    }
    calculate_log_hash(&summary)
}

impl SlowFingerprint {
    pub(crate) fn capture(app: &App) -> Self {
        let mut graph_specs = Vec::new();
        if let Some(details) = &app.graph_details {
            for spec in &details.specs {
                graph_specs.push((spec.spec.id.clone(), spec.spec.status));
            }
        }
        Self {
            agents_len: app.agents.len(),
            graphs: app
                .graphs
                .iter()
                .map(|graph| (graph.id.clone(), graph.status))
                .collect(),
            daemon_running: app.daemon_running,
            daemon_version: app.daemon_version.clone(),
            update_available: app.update_available.clone(),
            hub_notice: app.hub_notice.clone(),
            active_runs_len: app.active_runs.len(),
            backlog_len: app.backlog_specs.len(),
            projects_len: app.projects.len(),
            project_knowledge_len: app.project_knowledge.len(),
            project_graph_edges_len: app.project_graph_edges.len(),
            project_graph_trees_len: app.project_graph_trees.len(),
            archived_graph_count: app.archived_graph_count,
            graph_specs,
            graph_runs: app
                .graph_runs
                .iter()
                .map(|run| (run.node_id.clone(), run.status, run.iteration))
                .collect(),
            graph_live: app.graph_live_state.as_ref().map(|live| {
                (
                    live.graph_status,
                    live.done_count,
                    live.total_count,
                    live.current_spec_id.clone(),
                )
            }),
            log_hash: calculate_log_hash(&app.log_content),
            rag_queue_len: app.global_rag_queue.len(),
            rag_paused: app.rag_paused,
            rag_model_loaded: app.rag_model_loaded,
            rag_info: (
                app.rag_info.total_chunks,
                app.rag_info.indexed_files,
                app.rag_info.queued_items,
                app.rag_info.processing_items,
            ),
            rag_files_len: app.rag_file_status.len(),
            missions_unlocked: app.mission_manager.unlocked_count(),
            playground_results_len: app.playground_results.len(),
            playground_pending: app.playground_search_pending,
            playground_executed_query: app.playground_last_executed_query.clone(),
            crews_snapshot: app.crews.snapshot.is_some(),
            crews_rows: crews_rows_digest(app),
            crews_message: message_summary(&app.crews.message),
            settings_rows: settings_rows_digest(app),
            settings_message: message_summary(&app.hub.settings.message),
            remote_signed_in: app.hub.remote.view.signed_in,
            remote_login: app.hub.remote.view.login.clone(),
            remote_enabled: app.hub.remote.view.enabled,
            remote_relay: app.hub.remote.view.relay.clone(),
            remote_live_devices: app.hub.remote.view.live_devices.clone(),
            remote_devices_len: app.hub.remote.view.devices.len(),
            remote_pair: app.hub.remote.pair.is_some(),
            remote_message: message_summary(&app.hub.remote.message),
        }
    }
}

// ── Fast-path change snapshots ──────────────────────────────────────

/// Session identities, statuses and connection flags across both PTY-backed
/// lists: compared around the fast polls so an exit or disconnect (which
/// `output_seq` does not observe — the `exited` frame carries no `seq`)
/// dirties the tick.
fn session_snapshot(app: &App) -> Vec<(String, AgentStatus, bool)> {
    app.interactive_agents
        .iter()
        .chain(app.terminal_agents.iter())
        .map(|agent| (agent.id.clone(), agent.status, agent.is_connected()))
        .collect()
}

/// Visible live-tail content: run id, node name, status, stdout/stderr line
/// counts. Compared around `poll_node_tail_dialog` so a streaming tail (or
/// a run transition) dirties the tick. Scroll and cursor are input-driven
/// and deliberately excluded.
type TailSnapshot = Option<(Option<String>, String, Option<GraphRunStatus>, usize, usize)>;

fn tail_snapshot(app: &App) -> TailSnapshot {
    app.node_tail_dialog.as_ref().map(|dialog| {
        (
            dialog.run_id.clone(),
            dialog.node_name.clone(),
            dialog.status,
            dialog.stdout_lines.len(),
            dialog.stderr_lines.len(),
        )
    })
}

// ── The split refresh ───────────────────────────────────────────────

impl App {
    /// Whether the slow refresh must run this tick: at most once per second,
    /// or immediately when a pushed change armed [`App::slow_force`].
    pub(crate) fn slow_refresh_due(&self) -> bool {
        self.slow_force || self.last_slow_refresh.elapsed() >= Duration::from_secs(1)
    }

    /// Every-tick work: in-memory polls, animation stepping, TTL dismissals
    /// and non-blocking channel drains. True when the tick produced a visible
    /// change that needs a redraw.
    pub(crate) fn refresh_fast(&mut self) -> bool {
        let hub_dirty = self.poll_session_hub();
        self.animation_tick = self.animation_tick.wrapping_add(1);
        // The project type-ahead only ever applies to the Knowledge layer's
        // list, so it is dropped here — the one place every way of leaving
        // that layer (arrows, F2, click, Home) passes through.
        self.sync_project_filter_to_layer();
        let sessions_before = session_snapshot(self);
        self.poll_interactive_agents();
        self.poll_terminal_agents();
        let sessions_changed = sessions_before != session_snapshot(self);
        self.tick_banner_animation();
        self.ensure_sidebar_brain();
        self.auto_hide_sidebar();
        self.system_monitor_active
            .store(self.sidebar_visible, Ordering::Relaxed);
        let copied_cleared = self.dismiss_copied();
        self.update_whimsg_context();
        self.tick_atmosphere();
        self.resize_interactive_agents();
        let graph_action_changed = self.poll_graph_action();
        let tail_before = tail_snapshot(self);
        self.poll_node_tail_dialog();
        let tail_changed = tail_before != tail_snapshot(self);
        let graph_message_cleared = self.dismiss_graph_action_message();
        let shelf_cleared = self.dismiss_shelf_toast();
        let panel_before = (
            self.panel_face,
            self.panel_dwell_face,
            self.panel_last_reason.clone(),
        );
        self.tick_panel_face();
        let panel_changed = panel_before
            != (
                self.panel_face,
                self.panel_dwell_face,
                self.panel_last_reason.clone(),
            );
        if let Some(dialog) = self.simple_prompt_dialog.as_mut() {
            dialog.tick_at_picker();
        }
        let ctrl_c_cleared = self.clear_expired_ctrl_c();

        // Non-blocking check for updated system info from background thread
        while let Ok(info) = self.system_info_rx.try_recv() {
            self.system_info_target = info;
            self.last_system_update = Instant::now();
        }
        if system_settling(&self.system_info, &self.system_info_target) {
            self.interpolate_system_info();
        } else {
            // Settled: snap to the target (killing asymptotic residue so the
            // settling check stays false) and keep the frame clock fresh so
            // the next sample blends smoothly instead of jumping.
            self.last_system_frame_at = Instant::now();
            self.system_info = self.system_info_target.clone();
        }

        // CT14: correct stale sidebar indices on the existing refresh tick
        // (pure state, no extra redraw) so navigation works after data
        // shrinks between ticks.
        self.normalize_automation_kind();
        self.normalize_agent_section_focus();
        self.clamp_sidebar_selection();

        hub_dirty
            || sessions_changed
            || copied_cleared
            || graph_action_changed
            || tail_changed
            || graph_message_cleared
            || shelf_cleared
            || panel_changed
            || ctrl_c_cleared
    }

    /// Throttled work (FR2): every database read, daemon round-trip and port
    /// probe the old per-tick `refresh` performed. Runs at most once per
    /// second, or when [`App::slow_force`] is armed.
    pub(crate) fn refresh_slow(&mut self) -> Result<()> {
        self.last_slow_refresh = Instant::now();
        self.slow_force = false;
        if self.update_available.is_none() {
            self.update_available = crate::autoupdate::update_available_tag();
        }
        self.refresh_daemon_status();
        self.refresh_agents()?;
        self.refresh_projects()?;
        self.refresh_graphs()?;
        self.refresh_project_graph().ok();
        self.refresh_rag_state()?;
        self.refresh_active_runs()?;
        self.refresh_log();
        self.tick_missions()?;
        self.poll_playground_search();
        self.refresh_playground_search()?;
        // CT30: apply any finished Crews-face fetch/write, then start a new
        // one if the face is visible and its throttle allows (D5).
        self.poll_crews_face();
        self.poll_crews_write();
        self.refresh_crews_face();
        // CM51 (D4): a `config set` from anywhere, or a hand edit the
        // daemon's watcher saw, publishes a settings revision. When it moves,
        // re-read the settings this TUI renders — no restart needed.
        self.poll_settings_revision();
        // CT31: apply any finished Settings-tab fetch/write, then start a new
        // one if the hub's Settings tab is visible and the throttle allows.
        self.poll_settings_rows();
        self.poll_settings_write();
        self.refresh_settings_rows();
        self.dismiss_settings_message();
        // The Remote tab's cached status, its writes and its device flow;
        // the status also refreshes off-tab so the sidebar indicator stays
        // current.
        self.poll_remote_status();
        self.poll_remote_write();
        self.poll_remote_login();
        self.refresh_remote_status();
        self.dismiss_remote_message();
        // Same CT14 defensives as the fast path: slow data can shrink too.
        self.normalize_automation_kind();
        self.normalize_agent_section_focus();
        self.clamp_sidebar_selection();
        Ok(())
    }

    /// Clear an expired Ctrl+C notice/confirm window: expiry is otherwise
    /// only observed at draw time, which would leave it painted past its TTL
    /// on an idle tick that performs no draw. True when something cleared.
    fn clear_expired_ctrl_c(&mut self) -> bool {
        let mut cleared = false;
        if self.ctrl_c.notice.is_some() && self.ctrl_c.notice().is_none() {
            self.ctrl_c.notice = None;
            cleared = true;
        }
        if self.ctrl_c.armed_at.is_some() && !self.ctrl_c.armed() {
            self.ctrl_c.armed_at = None;
            cleared = true;
        }
        cleared
    }
}
