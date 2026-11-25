use super::types::{AgentEntry, App, Focus};
use crate::tui::agent::InteractiveAgent;
use crate::tui::gamification::MissionEvent;
use regex::Regex;
use std::sync::LazyLock;
use std::time::Duration;

static ANSI_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1b\[[0-9;]*[A-Za-z]").expect("invalid ANSI regex"));

/// Strip ANSI escape sequences from a string for plain-text display.
fn strip_ansi_codes(s: &str) -> String {
    ANSI_RE.replace_all(s, "").into_owned()
}

pub(super) fn recent_output_snippet(agent: &InteractiveAgent, n: usize) -> String {
    let clean: Vec<String> = agent
        .last_output_lines(n)
        .into_iter()
        .map(|line| strip_ansi_codes(&line))
        .filter(|line| !line.is_empty())
        .collect();

    if clean.is_empty() {
        String::new()
    } else {
        format!("\n{}", clean.join("\n"))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SessionTarget {
    Interactive(usize),
    Terminal(usize),
}

pub(super) fn reverse_sorted_indices(mut indices: Vec<usize>) -> Vec<usize> {
    indices.sort_unstable();
    indices.reverse();
    indices
}

/// Selection index after a session-list mutation: keep the previously-selected
/// entry by identity when it survived the mutation, otherwise clamp into range.
/// Pure so the "navigate Down over a reaped session" fix is unit-testable.
pub(super) fn selection_after_mutation(
    entry_ids: &[&str],
    anchor: Option<&str>,
    prev_selected: usize,
) -> usize {
    if entry_ids.is_empty() {
        return 0;
    }
    if let Some(anchor) = anchor {
        if let Some(pos) = entry_ids.iter().position(|id| *id == anchor) {
            return pos;
        }
    }
    prev_selected.min(entry_ids.len() - 1)
}

fn poll_agents(agents: &mut [InteractiveAgent]) {
    for agent in agents {
        agent.poll();
    }
}

pub(super) fn remove_session_name(
    sessions: &mut Vec<InteractiveAgent>,
    idx: usize,
) -> Option<String> {
    let agent_name = sessions.get(idx).map(|agent| agent.name.clone())?;

    sessions.remove(idx);
    Some(agent_name)
}

pub(super) fn log_terminal_exit(name: &str, shell: &str, code: i32, output_snippet: &str) {
    tracing::warn!(
        "Terminal '{}' ({}) exited with code {code}.{}",
        name,
        shell,
        if output_snippet.is_empty() {
            ""
        } else {
            output_snippet
        }
    );
}

impl App {
    /// Seed Brian's Brain from a pointer move at absolute cell `(col, row)`.
    /// Each brain seeds only when the position is inside the rect where it was
    /// last drawn; otherwise its trail is reset so a re-entry plants a fresh
    /// cluster instead of a line across the field.
    pub fn notify_mouse_move(&mut self, col: u16, row: u16) {
        if let Some(rect) = self.home_brain_rect {
            if let Some(ref mut brain) = self.home_brain {
                if rect_contains(rect, col, row) {
                    brain.seed_pointer((col - rect.x) as usize, (row - rect.y) as usize);
                } else {
                    brain.reset_trail();
                }
            }
        }
        if let Some(rect) = self.sidebar_brain_rect {
            if let Some(ref mut brain) = self.sidebar_brain {
                if rect_contains(rect, col, row) {
                    brain.seed_pointer((col - rect.x) as usize, (row - rect.y) as usize);
                } else {
                    brain.reset_trail();
                }
            }
        }
    }

    /// Update atmosphere mouse context (call from event loop on MouseMove).
    pub fn notify_atmosphere_mouse(&mut self, col: u16, row: u16) {
        let (prev_col, prev_row) = self.atmosphere_last_mouse;
        let delta_col = col as i16 - prev_col as i16;
        let delta_row = row as i16 - prev_row as i16;
        self.atmosphere_ctx.mouse_col = col;
        self.atmosphere_ctx.mouse_row = row;
        self.atmosphere_ctx.mouse_delta_col = delta_col;
        self.atmosphere_ctx.mouse_delta_row = delta_row;
        self.atmosphere_last_mouse = (col, row);
        self.atmosphere.notify_mouse(col, row, delta_col, delta_row);
    }

    /// Hit-test a left click against the atmosphere: while the atmosphere is
    /// active, a click within catch range of a live firefly queues the catch.
    /// Call this before hiding the atmosphere for the click.
    pub fn atmosphere_left_click(&mut self, col: u16, row: u16) {
        if !crate::tui::ui::atmosphere_active(self) {
            return;
        }
        if self.atmosphere.catch_at(col, row) {
            self.queue_mission_event(MissionEvent::FireflyCaught);
        }
    }

    /// Advance the atmosphere engine one tick (called from App::refresh).
    pub(super) fn tick_atmosphere(&mut self) {
        use chrono::Timelike;
        self.atmosphere_ctx.hour = chrono::Local::now().hour() as u8;
        let scroll_active = self.last_scroll_at.elapsed().as_secs_f32() < 1.0;
        self.atmosphere_ctx.scroll_velocity = if scroll_active { 0.8 } else { 0.0 };
        self.atmosphere_ctx.typing_speed = if self.animation_tick.is_multiple_of(3) {
            0.1
        } else {
            0.0
        };
    }

    pub fn tick_banner_animation(&mut self) {
        // CM52: `tui.brians_brain = false` keeps the banner static; the
        // animations gate freezes it while paused or off (FR2/FR3).
        if self.tui_settings.brians_brain && crate::tui::ui::animations_enabled(self) {
            if let Some(ref mut brain) = self.home_brain {
                brain.step();
            }
        }

        if self.focus != Focus::Home {
            return;
        }

        let (cols, rows) = effective_brain_dims(self.last_panel_inner);
        if cols < 6 || rows < 3 {
            return;
        }

        if brain_needs_reinit(&self.home_brain, rows, cols) {
            self.home_brain = Some(make_brain(rows, cols));
        }
    }

    pub fn ensure_sidebar_brain(&mut self) {
        let (cols, rows) = match self.sidebar_brain_rect {
            Some(rect) => (rect.width as usize, rect.height as usize),
            None => {
                let (_tw, th) = ratatui::crossterm::terminal::size().unwrap_or((120, 40));
                let sidebar_h = th.saturating_sub(2);
                let dashboard_h = if sidebar_h >= 6 { 6 } else { 0 };
                (
                    (crate::tui::ui::SIDEBAR_WIDTH.saturating_sub(1)) as usize,
                    sidebar_h.saturating_sub(dashboard_h) as usize,
                )
            }
        };

        if cols < 6 || rows < 3 {
            return;
        }

        if brain_needs_reinit(&self.sidebar_brain, rows, cols) {
            self.sidebar_brain = Some(make_brain(rows, cols));
        }

        // CM52: `tui.brians_brain = false` keeps the sidebar brain static too;
        // the animations gate freezes it while paused or off (FR2/FR3).
        if self.tui_settings.brians_brain && crate::tui::ui::animations_enabled(self) {
            if let Some(ref mut brain) = self.sidebar_brain {
                brain.step();
            }
        }
    }

    pub fn dismiss_brain(&mut self) {
        if let Some(ref mut brain) = self.home_brain {
            *brain = super::super::brians_brain::BriansBrain::new(brain.rows, brain.cols);
        }
    }

    /// True when the badge cleared this tick (a visible change).
    pub(super) fn dismiss_copied(&mut self) -> bool {
        if self.show_copied && self.copied_at.elapsed() > Duration::from_secs(2) {
            self.show_copied = false;
            return true;
        }
        false
    }

    fn move_interactive_selection(&mut self, forward: bool) {
        let focusable = self.cycle_focus_indices();
        if focusable.is_empty() {
            return;
        }

        let current_pos = focusable
            .iter()
            .position(|&idx| idx == self.selected)
            .unwrap_or(0);

        let next_pos = crate::tui::selection::move_index(current_pos, focusable.len(), forward);

        self.selected = focusable[next_pos];
        self.focus = Focus::Agent;
        self.activate_selected_entry();
    }

    fn activate_interactive_session(&mut self, idx: usize) {
        let Some(agent) = self.interactive_agents.get(idx) else {
            return;
        };

        agent.mark_viewed();
    }

    fn activate_terminal_session(&mut self, idx: usize) {
        let Some(agent) = self.terminal_agents.get(idx) else {
            return;
        };

        agent.mark_viewed();
    }

    fn selected_session_target(&self) -> Option<SessionTarget> {
        let selected = self.selected_agent()?;
        match selected {
            AgentEntry::Interactive(idx) => Some(SessionTarget::Interactive(*idx)),
            AgentEntry::Terminal(idx) => Some(SessionTarget::Terminal(*idx)),
            _ => None,
        }
    }

    fn mark_selected_terminal_viewed(&mut self) {
        if !matches!(self.focus, Focus::Agent | Focus::Preview) {
            return;
        }

        let Some(AgentEntry::Terminal(idx)) = self.agents.get(self.selected) else {
            return;
        };
        let Some(agent) = self.terminal_agents.get(*idx) else {
            return;
        };

        agent.mark_viewed();
    }

    fn mark_selected_interactive_viewed(&mut self) {
        if !matches!(self.focus, Focus::Agent | Focus::Preview) {
            return;
        }

        let Some(AgentEntry::Interactive(idx)) = self.agents.get(self.selected) else {
            return;
        };
        let Some(agent) = self.interactive_agents.get(*idx) else {
            return;
        };

        agent.mark_viewed();
    }

    fn terminate_selected_session(&mut self) -> bool {
        let Some(target) = self.selected_session_target() else {
            return false;
        };

        self.close_session_target(target, 0)
    }

    fn sync_selection_after_session_mutation(&mut self) {
        if self.agents.is_empty() {
            self.selected = 0;
            return;
        }

        if self.selected >= self.agents.len() {
            self.selected = self.agents.len() - 1;
        }
    }

    fn reset_focus_after_session_mutation(&mut self) {
        if self.focus == Focus::Agent
            || matches!(self.focus, Focus::ContextTransfer | Focus::RagTransfer)
        {
            self.focus = Focus::Preview;
        }
    }

    pub fn next_interactive(&mut self) {
        self.move_interactive_selection(true);
    }

    pub fn prev_interactive(&mut self) {
        self.move_interactive_selection(false);
    }

    /// Activate the currently selected session entry.
    fn activate_selected_entry(&mut self) {
        let Some(entry) = self.agents.get(self.selected) else {
            return;
        };

        match entry {
            AgentEntry::Interactive(idx) => self.activate_interactive_session(*idx),
            AgentEntry::Terminal(idx) => self.activate_terminal_session(*idx),
            _ => {}
        }
    }

    pub(crate) fn resize_interactive_agents(&mut self) {
        // While the panel border is dragged the PTYs wait for the release:
        // resizing on every drag step would churn the child on each column.
        if self.activity_panel_dragging {
            return;
        }
        let (cols, rows) = self.last_panel_inner;
        if cols == 0 || rows == 0 {
            return;
        }

        for agent in &mut self.interactive_agents {
            // A remote device owns this size: answering `resize` would fight it.
            if agent.remote_size.is_some() {
                continue;
            }
            if agent.last_pty_cols != cols || agent.last_pty_rows != rows {
                agent.resize(cols, rows);
            }
        }
        for agent in &mut self.terminal_agents {
            // A remote device owns this size: answering `resize` would fight it.
            if agent.remote_size.is_some() {
                continue;
            }
            // Warp-mode terminals lose 1 row for the single-line input box.
            // This must stay equal to `pty_area.height.saturating_sub(1)`
            // where `pty_area` is what `draw_terminal_warp_mode` stores in
            // `last_panel_inner` (panel inner minus input_height, no gap);
            // a future warp-height change that drifts from it would silently
            // reintroduce completion-timed shrinks (CT15).
            // CT16: when the child holds the alternate screen it owns the
            // full pane, so do not reserve the input-box rows. Keep the
            // constant in sync with `panel/mod.rs::split_warp_areas`.
            let effective_rows = if agent.warp_mode && !agent.in_alternate_screen() {
                rows.saturating_sub(1)
            } else {
                rows
            };
            debug_assert!(effective_rows <= rows);
            if agent.last_pty_cols != cols || agent.last_pty_rows != effective_rows {
                agent.resize(cols, effective_rows);
            }
        }
    }

    /// Poll terminal agent processes for exit status.
    pub(super) fn poll_terminal_agents(&mut self) {
        self.mark_selected_terminal_viewed();
        poll_agents(&mut self.terminal_agents);

        let exited_indices = self.exited_terminal_indices();
        if exited_indices.is_empty() {
            return;
        }

        for &idx in &exited_indices {
            self.handle_terminal_exit(idx);
        }
        self.remove_terminal_sessions(exited_indices);
        self.finish_session_mutation();
    }

    pub(super) fn poll_interactive_agents(&mut self) {
        self.mark_selected_interactive_viewed();
        poll_agents(&mut self.interactive_agents);

        let exited_indices = self.exited_interactive_indices();
        for &idx in &exited_indices {
            self.handle_interactive_exit(idx);
        }

        let removed_indices = self.successful_interactive_exit_indices();
        if removed_indices.is_empty() {
            return;
        }

        let anchor = self.selected_entry_identity();
        self.remove_interactive_sessions(removed_indices);
        self.finish_session_mutation_preserving(anchor.as_deref());
    }

    pub fn rerun_selected(&self) -> anyhow::Result<()> {
        let Some(agent) = self.agents.get(self.selected) else {
            return Ok(());
        };
        match agent {
            AgentEntry::Interactive(_) | AgentEntry::Terminal(_) => Ok(()),
            _ => {
                use crate::application::ports::StateRepository;
                let port = self
                    .db
                    .get_state("port")?
                    .unwrap_or_else(|| "7755".to_string());
                super::send_mcp_task_run(&port, agent.id(self))
            }
        }
    }

    #[allow(dead_code)]
    pub fn kill_selected_agent(&mut self) {
        let Some(AgentEntry::Interactive(idx)) = self.agents.get(self.selected) else {
            return;
        };
        if self.close_interactive_session_at(*idx, 0) {
            self.finish_session_mutation();
        }
    }

    pub fn cleanup(&mut self) {
        // A session closed but still inside its shadow-summary linger would
        // die with this process before its thread sent `kill`, letting the
        // closed session survive in the daemon and come back on the next
        // start: send those kills now and give the frames a beat to reach
        // the hub before the sockets close with the process.
        if !self.pending_closes.is_empty() {
            for stream in std::mem::take(&mut self.pending_closes) {
                stream.kill();
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        // Quitting the TUI only closes the hub WebSocket; every session keeps
        // running in the daemon and reattaches on the next start.
        self.close_session_hub();
        // Clear any lingering toast notifications from the Windows Action Center
        crate::domain::notification::clear_notifications_on_exit();
    }
}

mod exits;
mod reindex;
mod session_close;
#[cfg(test)]
mod session_reindex_tests;

// ── Brain helpers ─────────────────────────────────────────────────

fn make_brain(rows: usize, cols: usize) -> super::super::brians_brain::BriansBrain {
    super::super::brians_brain::BriansBrain::new(rows, cols)
}

/// Whether `(x, y)` sits inside `rect` (half-open on the far edges).
fn rect_contains(rect: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn brain_needs_reinit(
    brain: &Option<super::super::brians_brain::BriansBrain>,
    rows: usize,
    cols: usize,
) -> bool {
    brain
        .as_ref()
        .is_none_or(|b| b.rows != rows || b.cols != cols)
}

/// Resolve effective brain dimensions from panel size, falling back to terminal size.
fn effective_brain_dims(panel: (u16, u16)) -> (usize, usize) {
    let (pw, ph) = panel;
    if pw >= 6 && ph >= 3 {
        return (pw as usize, ph as usize);
    }
    let (tw, th) = ratatui::crossterm::terminal::size().unwrap_or((120, 40));
    let cols = (tw / 2).saturating_sub(2) as usize;
    let rows = th.saturating_sub(3) as usize;
    (cols, rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_after_mutation_follows_anchor_shifted_up() {
        // "Down jump" scenario: B was reaped while cursor had moved to C (old
        // index 2); C is now at index 1 and must remain selected.
        assert_eq!(selection_after_mutation(&["A", "C", "D"], Some("C"), 2), 1);
    }

    #[test]
    fn selection_after_mutation_up_navigation_unaffected() {
        assert_eq!(selection_after_mutation(&["A", "C", "D"], Some("A"), 0), 0);
    }

    #[test]
    fn selection_after_mutation_clamps_when_anchor_missing() {
        assert_eq!(selection_after_mutation(&["A", "C", "D"], Some("B"), 2), 2);
    }

    #[test]
    fn selection_after_mutation_clamps_to_last_when_prev_out_of_range() {
        assert_eq!(selection_after_mutation(&["A", "C"], Some("Z"), 5), 1);
    }

    #[test]
    fn selection_after_mutation_empty_list_is_zero() {
        assert_eq!(selection_after_mutation(&[], Some("A"), 3), 0);
    }

    #[test]
    fn strip_ansi_codes_plain_text() {
        assert_eq!(strip_ansi_codes("hello"), "hello");
    }

    #[test]
    fn strip_ansi_codes_with_escapes() {
        assert_eq!(strip_ansi_codes("\x1b[31mred\x1b[0m"), "red");
    }

    #[test]
    fn strip_ansi_codes_multiple_escapes() {
        assert_eq!(
            strip_ansi_codes("\x1b[1m\x1b[32mbold green\x1b[0m"),
            "bold green"
        );
    }

    #[test]
    fn strip_ansi_codes_empty() {
        assert_eq!(strip_ansi_codes(""), "");
    }

    #[test]
    fn strip_ansi_codes_no_escapes() {
        assert_eq!(strip_ansi_codes("no escapes here"), "no escapes here");
    }

    #[test]
    fn strip_ansi_codes_complex_sequence() {
        assert_eq!(strip_ansi_codes("\x1b[38;5;196mred256\x1b[0m"), "red256");
    }

    #[test]
    fn reverse_sorted_indices_basic() {
        assert_eq!(reverse_sorted_indices(vec![1, 3, 2]), vec![3, 2, 1]);
    }

    #[test]
    fn reverse_sorted_indices_empty() {
        assert_eq!(reverse_sorted_indices(vec![]), Vec::<usize>::new());
    }

    #[test]
    fn reverse_sorted_indices_single() {
        assert_eq!(reverse_sorted_indices(vec![5]), vec![5]);
    }

    #[test]
    fn reverse_sorted_indices_already_sorted() {
        assert_eq!(reverse_sorted_indices(vec![1, 2, 3]), vec![3, 2, 1]);
    }

    #[test]
    fn reverse_sorted_indices_duplicates() {
        assert_eq!(reverse_sorted_indices(vec![2, 2, 1, 3]), vec![3, 2, 2, 1]);
    }

    #[test]
    fn selection_after_mutation_no_anchor() {
        assert_eq!(selection_after_mutation(&["A", "B", "C"], None, 1), 1);
    }

    #[test]
    fn selection_after_mutation_no_anchor_clamps() {
        assert_eq!(selection_after_mutation(&["A", "B"], None, 5), 1);
    }

    #[test]
    fn selection_after_mutation_single_element() {
        assert_eq!(selection_after_mutation(&["A"], Some("A"), 0), 0);
    }

    #[test]
    fn selection_after_mutation_anchor_first() {
        assert_eq!(selection_after_mutation(&["X", "Y", "Z"], Some("X"), 2), 0);
    }

    #[test]
    fn selection_after_mutation_anchor_last() {
        assert_eq!(selection_after_mutation(&["X", "Y", "Z"], Some("Z"), 0), 2);
    }

    #[test]
    fn selection_after_mutation_no_anchor_empty() {
        assert_eq!(selection_after_mutation(&[], None, 0), 0);
    }

    #[test]
    fn brain_needs_reinit_none() {
        assert!(brain_needs_reinit(&None, 10, 10));
    }

    #[test]
    fn brain_needs_reinit_matching_dims() {
        let brain = make_brain(10, 10);
        assert!(!brain_needs_reinit(&Some(brain), 10, 10));
    }

    #[test]
    fn brain_needs_reinit_different_dims() {
        let brain = make_brain(10, 10);
        assert!(brain_needs_reinit(&Some(brain), 20, 20));
    }

    #[test]
    fn brain_needs_reinit_different_rows() {
        let brain = make_brain(10, 10);
        assert!(brain_needs_reinit(&Some(brain), 20, 10));
    }

    #[test]
    fn brain_needs_reinit_different_cols() {
        let brain = make_brain(10, 10);
        assert!(brain_needs_reinit(&Some(brain), 10, 20));
    }

    #[test]
    fn effective_brain_dims_small() {
        let (cols, rows) = effective_brain_dims((100, 200));
        assert!(rows > 0);
        assert!(cols > 0);
        assert!(rows <= 200);
        assert!(cols <= 100);
    }

    #[test]
    fn effective_brain_dims_zero_area() {
        let (cols, rows) = effective_brain_dims((0, 0));
        assert!(cols > 0);
        assert!(rows > 0);
    }

    #[test]
    fn effective_brain_dims_exactly_minimum() {
        let (cols, rows) = effective_brain_dims((6, 3));
        assert_eq!(cols, 6);
        assert_eq!(rows, 3);
    }

    #[test]
    fn effective_brain_dims_below_minimum() {
        let (cols, rows) = effective_brain_dims((5, 2));
        assert!(rows > 0);
        assert!(cols > 0);
    }

    #[test]
    fn effective_brain_dims_large() {
        let (cols, rows) = effective_brain_dims((500, 1000));
        assert_eq!(cols, 500);
        assert_eq!(rows, 1000);
    }

    #[test]
    fn selection_after_mutation_anchor_present_at_same_position() {
        assert_eq!(selection_after_mutation(&["A", "B", "C"], Some("A"), 0), 0);
    }

    #[test]
    fn selection_after_mutation_anchor_at_end() {
        assert_eq!(selection_after_mutation(&["A", "B", "C"], Some("C"), 0), 2);
    }

    #[test]
    fn selection_after_mutation_single_element_no_anchor() {
        assert_eq!(selection_after_mutation(&["X"], None, 0), 0);
    }

    #[test]
    fn strip_ansi_codes_nested_sequences() {
        assert_eq!(
            strip_ansi_codes("\x1b[1m\x1b[31mbold red\x1b[0m\x1b[0m"),
            "bold red"
        );
    }

    #[test]
    fn strip_ansi_codes_color256() {
        assert_eq!(strip_ansi_codes("\x1b[38;5;42mhello\x1b[0m"), "hello");
    }

    #[test]
    fn strip_ansi_codes_rgb_color() {
        assert_eq!(
            strip_ansi_codes("\x1b[38;2;255;128;0mcolored\x1b[0m"),
            "colored"
        );
    }

    #[test]
    fn brain_needs_reinit_none_vs_none() {
        assert!(brain_needs_reinit(&None, 5, 5));
    }

    #[test]
    fn brain_needs_reinit_exact_match_no_reinit() {
        let brain = make_brain(15, 25);
        assert!(!brain_needs_reinit(&Some(brain), 15, 25));
    }

    #[test]
    fn effective_brain_dims_width_exactly_minimum() {
        let (cols, _rows) = effective_brain_dims((6, 100));
        assert_eq!(cols, 6);
    }

    #[test]
    fn effective_brain_dims_height_exactly_minimum() {
        let (_cols, rows) = effective_brain_dims((100, 3));
        assert_eq!(rows, 3);
    }

    fn test_app() -> (App, tempfile::TempDir) {
        let path = crate::test_support::temp_db_path();
        let db = std::sync::Arc::new(crate::db::Database::new(&path).unwrap());
        let data_dir = tempfile::tempdir().unwrap();
        let app = App::new(
            db,
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .unwrap();
        (app, data_dir)
    }

    #[test]
    fn ensure_sidebar_brain_uses_the_drawn_rect() {
        use ratatui::layout::Rect;
        let (mut app, _data_dir) = test_app();
        app.sidebar_brain_rect = Some(Rect::new(0, 10, 32, 20));
        app.ensure_sidebar_brain();
        let brain = app.sidebar_brain.as_ref().expect("brain should exist");
        assert_eq!(brain.cols, 32);
        assert_eq!(brain.rows, 20);
        app.sidebar_brain_rect = Some(Rect::new(0, 10, 30, 20));
        app.ensure_sidebar_brain();
        let brain = app.sidebar_brain.as_ref().expect("brain should exist");
        assert_eq!(brain.cols, 30);
        assert_eq!(brain.rows, 20);
    }

    #[test]
    fn ensure_sidebar_brain_without_a_rect_uses_the_sidebar_width() {
        let (mut app, _data_dir) = test_app();
        app.sidebar_brain_rect = None;
        app.ensure_sidebar_brain();
        let brain = app.sidebar_brain.as_ref().expect("brain should exist");
        assert_eq!(brain.cols, crate::tui::ui::SIDEBAR_WIDTH as usize - 1);
    }
}

// ── handle_interactive_exit tests ───────────────────────────────────

#[cfg(test)]
mod handle_interactive_exit_tests {
    use super::*;
    use crate::db::Database;
    use crate::tui::agent::{AgentStatus, InteractiveAgent};
    use std::sync::Arc;
    use tempfile::tempdir;

    fn test_db() -> Arc<Database> {
        let path = crate::test_support::temp_db_path();
        Arc::new(Database::new(&path).expect("create test db"))
    }

    fn spawn_mock_interactive(name: &str) -> InteractiveAgent {
        InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some(name),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn mock interactive")
    }

    /// sandbox_merge_run_status returns "merged" for CleanMerge
    #[test]
    fn sandbox_merge_run_status_clean_merge() {
        let status =
            App::sandbox_merge_run_status(&crate::domain::sandbox::MergeOutcome::CleanMerge);
        assert_eq!(status, "merged");
    }

    /// sandbox_merge_run_status returns "merged" for ConflictResolution
    #[test]
    fn sandbox_merge_run_status_conflict_resolution() {
        let status = App::sandbox_merge_run_status(
            &crate::domain::sandbox::MergeOutcome::ConflictResolution,
        );
        assert_eq!(status, "merged");
    }

    /// sandbox_merge_run_status returns "failed" for MergeFailed
    #[test]
    fn sandbox_merge_run_status_merge_failed() {
        let status = App::sandbox_merge_run_status(
            &crate::domain::sandbox::MergeOutcome::MergeFailed("reason".into()),
        );
        assert_eq!(status, "failed");
    }

    /// merge_active_sandbox: no active sandbox is a no-op
    #[test]
    fn merge_active_sandbox_none() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.active_sandbox = None;
        // Should not panic
        app.merge_active_sandbox();
        assert!(app.active_sandbox.is_none());
    }

    /// announce_interactive_exit: code != 0 calls notify_failed_interactive_exit
    #[test]
    fn announce_interactive_exit_failure() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        // Just verify it doesn't panic - the actual notification is tested elsewhere
        app.announce_interactive_exit("agent-1", "claude", 1, "error output");
    }

    /// announce_interactive_exit: code == 0 with yolo args queues YoloTaskCompleted
    #[test]
    fn announce_interactive_exit_success_yolo() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        // The yolo check is internal; just verify no panic
        app.announce_interactive_exit("agent-yolo", "claude --yolo", 0, "success");
    }

    /// announce_interactive_exit: code == 0 without yolo is a no-op
    #[test]
    fn announce_interactive_exit_success_no_yolo() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.announce_interactive_exit("agent-normal", "claude", 0, "success");
    }

    /// handle_interactive_exit: early return if agent not found
    #[test]
    fn handle_interactive_exit_agent_not_found() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        app.interactive_agents.clear();
        // Should not panic
        app.handle_interactive_exit(0);
    }

    /// handle_interactive_exit: early return if exit_notified already true
    #[test]
    fn handle_interactive_exit_already_notified() {
        let db = test_db();
        let data_dir = tempdir().expect("create data dir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let mut agent = spawn_mock_interactive("test-agent");
        agent.status = AgentStatus::Exited(0);
        agent.exit_notified = true;
        app.interactive_agents.push(agent);

        // Should not panic and not double-process
        app.handle_interactive_exit(0);
    }

    /// handle_interactive_exit: early return if status is not Exited
    /// and marks the agent notified.
    #[test]
    fn cleanup_kills_a_session_closed_inside_its_shadow_linger() {
        let hub = &crate::sessions::client::test_client().hub;
        let path = crate::test_support::temp_db_path();
        let db = Arc::new(Database::new(&path).expect("create test db"));
        let data_dir = tempdir().expect("tempdir");
        let mut app = App::new(
            Arc::clone(&db),
            data_dir.path(),
            &crate::domain::canopy_config::CanopyConfig::default(),
        )
        .expect("create app");

        let agent = InteractiveAgent::spawn_terminal(
            "cat",
            "/tmp",
            80,
            24,
            Some("quit-during-linger"),
            &[],
            ratatui::style::Color::White,
        )
        .expect("spawn cat on the test hub");
        let id = agent.id.clone();
        app.interactive_agents.push(agent);

        assert!(app.close_interactive_session_at(0, 0));
        assert!(
            hub.get(&id).is_some(),
            "the hub still holds the session mid-linger"
        );
        assert!(
            !app.pending_closes.is_empty(),
            "the close kept no stream for the quit flush"
        );

        app.cleanup();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while hub.get(&id).is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            hub.get(&id).is_none(),
            "quitting during the shadow linger lost the kill"
        );
    }
}
