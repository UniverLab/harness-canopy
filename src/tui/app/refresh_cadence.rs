//! Refresh cadence split: every-tick cheap work vs. throttled slow work.
//!
//! [`App::refresh_fast`] runs on every 50 ms tick and only touches in-memory
//! state plus non-blocking channel polls. [`App::refresh_slow`] runs at most
//! once per second (or when [`App::slow_force`] is armed by a pushed change)
//! and owns every database read, daemon round-trip and port probe the old
//! per-tick `refresh` performed. [`SlowFingerprint`](super::SlowFingerprint)
//! captures the slow outputs so the loop redraws for the slow path only when
//! something in it actually changed.

use std::time::{Duration, Instant};

use super::super::agent::AgentStatus;
use super::system_monitor::system_settling;
use super::*;
use crate::domain::graphs::GraphRunStatus;
// ── Fast-path change snapshots ──────────────────────────────────────

/// Session identities, statuses, connection flags and activity pulses across
/// both PTY-backed lists: compared around the fast polls so an exit or
/// disconnect (which `output_seq` does not observe — the `exited` frame
/// carries no `seq`) dirties the tick, and so the sidebar pulse going solid
/// 12 s after the last bytes dirties it too (time-based, otherwise invisible
/// with animations off).
fn session_snapshot(app: &App) -> Vec<(String, AgentStatus, bool, bool)> {
    app.interactive_agents
        .iter()
        .chain(app.terminal_agents.iter())
        .map(|agent| {
            (
                agent.id.clone(),
                agent.status,
                agent.is_connected(),
                agent.has_recent_activity(),
            )
        })
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
        let sidebar_before = self.sidebar_visible;
        self.auto_hide_sidebar();
        let sidebar_changed = sidebar_before != self.sidebar_visible;
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
        let settings_message_cleared = self.dismiss_settings_message();
        let remote_message_cleared = self.dismiss_remote_message();
        let at_picker_changed = self
            .simple_prompt_dialog
            .as_mut()
            .is_some_and(|dialog| dialog.tick_at_picker());
        let ctrl_c_cleared = self.clear_expired_ctrl_c();

        // Non-blocking check for updated system info from background thread
        while let Ok(info) = self.system_info_rx.try_recv() {
            self.system_info_target = info;
            self.last_system_update = Instant::now();
        }
        let clipboard_changed = self.drain_clipboard_images();
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
            || sidebar_changed
            || copied_cleared
            || graph_action_changed
            || tail_changed
            || graph_message_cleared
            || shelf_cleared
            || settings_message_cleared
            || remote_message_cleared
            || at_picker_changed
            || ctrl_c_cleared
            || clipboard_changed
    }

    /// Drain off-thread clipboard-image results into the open builder: a
    /// `None` payload (no image on the Windows clipboard) is a no-op, bytes
    /// attach as PNG. True when a result landed on an open builder, so the
    /// tick redraws the chips row.
    fn drain_clipboard_images(&mut self) -> bool {
        let mut changed = false;
        while let Ok(payload) = self.clipboard_image_rx.try_recv() {
            self.clipboard_image_pending = false;
            let Some(bytes) = payload else { continue };
            let db = self.db.clone();
            let shelf_dir = self.data_dir.join("shelf");
            let session_key = self.current_prompt_session_key();
            if let Some(dialog) = self.simple_prompt_dialog.as_mut() {
                match crate::tui::app::dialog::attach_image_bytes(
                    &db,
                    &shelf_dir,
                    &session_key,
                    &mut dialog.attachments,
                    &bytes,
                    "png",
                ) {
                    Ok(()) => dialog.attach_error = None,
                    Err(error) => dialog.attach_error = Some(error),
                }
                changed = true;
            }
        }
        changed
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
        // (The TTL dismissal lives on the fast path so it clears promptly.)
        self.poll_settings_rows();
        self.poll_settings_write();
        self.refresh_settings_rows();
        // The Remote tab's cached status, its writes and its device flow;
        // the status also refreshes off-tab so the sidebar indicator stays
        // current. (Its TTL dismissal is fast-path too.)
        self.poll_remote_status();
        self.poll_remote_write();
        self.poll_remote_login();
        self.refresh_remote_status();
        // Same CT14 defensives as the fast path: slow data can shrink too.
        self.normalize_automation_kind();
        self.normalize_agent_section_focus();
        self.clamp_sidebar_selection();
        // CT1 multi-face panel (FR2): the face tick runs two MAX queries per
        // call, so it lives here on the one-second cadence — after the data
        // refreshes it reads — and the fingerprint below carries the face, so
        // a switch still draws.
        self.tick_panel_face();
        // Draw-time database reads have no refresh output to fingerprint, so
        // the slow tick re-runs them and stores content digests: a new
        // activity entry, session summary, hub count or pending send moves
        // the fingerprint and draws without another interaction.
        self.refresh_draw_digests();
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

    /// Re-run the draws' own database reads and store content digests for
    /// [`SlowFingerprint`]: the activity panel's log entries for the drawn
    /// workdirs, the project overview's recent-session summaries, the hub
    /// legend's session/run counts, and the selected session's pending
    /// scheduled sends. One second of staleness at most; without this a new
    /// entry would sit invisible until some other change drew.
    fn refresh_draw_digests(&mut self) {
        self.activity_digest = self.activity_draw_digest();
        self.overview_sessions_digest = self.overview_sessions_draw_digest();
        self.hub_counts_digest = (
            self.db.count_interactive_sessions().unwrap_or(0),
            self.db.count_terminal_sessions().unwrap_or(0),
            self.db.count_background_agents().unwrap_or(0),
            self.db.count_runs().unwrap_or(0),
        );
        self.pending_sends_digest = self.pending_sends_draw_digest();
    }

    /// Hash of the activity rows the panel draws: the selected workdir's log
    /// entries (raw rows — name resolution is deterministic from them) plus
    /// its active agent ids, and the same for the selected project when it
    /// differs (the overview draws that workdir's activity too).
    fn activity_draw_digest(&self) -> u64 {
        let mut workdirs = Vec::new();
        if let Some(workdir) = self.selected_activity_workdir() {
            workdirs.push(workdir.to_string());
        }
        if let Some(project) = self.selected_project() {
            if !workdirs.iter().any(|w| w == &project.path) {
                workdirs.push(project.path.clone());
            }
        }
        if workdirs.is_empty() {
            return 0;
        }
        let limit = self.message_window_limit_for_scroll();
        let mut summary = String::new();
        for workdir in &workdirs {
            let entries = self
                .db
                .list_activity_log_entries(workdir, limit)
                .unwrap_or_default();
            let mut agent_ids = self
                .db
                .list_active_sync_agent_ids(workdir)
                .unwrap_or_default();
            agent_ids.sort();
            use std::fmt::Write as _;
            let _ = writeln!(summary, "{workdir}|{entries:?}|{agent_ids:?}");
        }
        calculate_log_hash(&summary)
    }

    /// Hash of the overview's recent-session summaries for the selected
    /// project: the same search the draw runs, filtered the same way, hashed
    /// as raw rows (the title/summary truncation is deterministic from them).
    fn overview_sessions_draw_digest(&self) -> u64 {
        let Some(project) = self.selected_project() else {
            return 0;
        };
        let nodes = self
            .db
            .search_operational_sessions(&project.path, 12)
            .unwrap_or_default();
        let mut summary = String::new();
        use std::fmt::Write as _;
        for node in nodes
            .iter()
            .filter(|node| {
                node.project_hash.as_deref() == Some(project.hash.as_str())
                    || node.metadata.as_deref().is_some_and(|metadata| {
                        serde_json::from_str::<serde_json::Value>(metadata)
                            .ok()
                            .and_then(|value| {
                                value
                                    .get("workdir")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string)
                            })
                            == Some(project.path.clone())
                    })
            })
            .take(3)
        {
            let _ = writeln!(summary, "{}|{}|{}", node.title, node.body, node.id);
        }
        calculate_log_hash(&summary)
    }

    /// Hash of the selected interactive session's pending scheduled sends —
    /// what the simple prompt's schedule panel draws.
    fn pending_sends_draw_digest(&self) -> u64 {
        let selected = self.selected_agent().and_then(|entry| match entry {
            types::AgentEntry::Interactive(idx) => self
                .interactive_agents
                .get(*idx)
                .map(|agent| agent.id.clone()),
            _ => None,
        });
        let Some(id) = selected else {
            return 0;
        };
        let pending = self
            .db
            .list_pending_scheduled_sends_for_session(&id)
            .unwrap_or_default();
        calculate_log_hash(&format!("{pending:?}"))
    }
}
