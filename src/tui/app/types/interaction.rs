//! Terminal-interaction state shared by the TUI event layer and `App`:
//! mouse text selection over a PTY pane, and the CT26 copy-reflex Ctrl+C
//! guard. Lives here (rather than in `event/`) so both `app::types::App`
//! and `event::mouse` can name one canonical type.

/// Mouse text selection over the focused agent's PTY pane. Coordinates are
/// pane-relative `(row, col)` cells matching the rendered `ScreenSnapshot`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminalSelection {
    /// Selected agent this selection belongs to: (is_terminal, index).
    pub agent: (bool, usize),
    pub start: (u16, u16),
    pub end: (u16, u16),
    pub dragging: bool,
}

impl TerminalSelection {
    /// Selection endpoints in linear (reading) order: start ≤ end.
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        if self.end < self.start {
            (self.end, self.start)
        } else {
            (self.start, self.end)
        }
    }
}

/// CT26: the copy-reflex Ctrl+C guard's state — the one-line sentence the
/// pane shows, and the confirm window that decides whether the next Ctrl+C is
/// forwarded on a platform that exits on the first one. One instance lives on
/// `App`.
#[derive(Clone)]
pub(crate) struct CtrlCState {
    /// The sentence to show, if any.
    pub(crate) notice: Option<String>,
    /// When `notice` was set; drives its TTL.
    pub(crate) notice_at: std::time::Instant,
    /// When the first swallowed Ctrl+C arrived on a `ctrl_c_exits` platform;
    /// `None` when no confirm window is open.
    pub(crate) armed_at: Option<std::time::Instant>,
}

impl CtrlCState {
    /// A fresh guard: nothing to show, nothing armed (`notice_at` is backdated
    /// so an expired `None` notice can never be drawn).
    pub fn initial() -> Self {
        Self {
            notice: None,
            notice_at: std::time::Instant::now() - std::time::Duration::from_secs(10),
            armed_at: None,
        }
    }

    pub fn set_notice(&mut self, text: &str) {
        self.notice = Some(text.to_string());
        self.notice_at = std::time::Instant::now();
    }

    /// The notice to draw, or `None` once its TTL has passed.
    pub fn notice(&self) -> Option<&str> {
        if self.notice_at.elapsed() <= crate::tui::event::ctrl_c::NOTICE_TTL {
            self.notice.as_deref()
        } else {
            None
        }
    }

    pub fn arm(&mut self) {
        self.armed_at = Some(std::time::Instant::now());
    }

    pub fn clear_arm(&mut self) {
        self.armed_at = None;
    }

    /// True only inside the confirm window, so a late second press re-arms
    /// instead of forwarding.
    pub fn armed(&self) -> bool {
        self.armed_at
            .is_some_and(|at| at.elapsed() <= crate::tui::event::ctrl_c::CTRL_C_CONFIRM_WINDOW)
    }
}

/// CT26: the platform slugs (`CliConfig::name`) whose registry entry sets
/// `ctrl_c_exits = true` — the set the Ctrl+C guard consults for the focused
/// interactive session's platform. Read from the persisted `[[clis]]` table
/// (the same table `merge_cli_fields` refreshes from the registry), never
/// inferred from a CLI's name.
pub(crate) fn ctrl_c_exits_clis(
    config: &crate::domain::canopy_config::CanopyConfig,
) -> std::collections::HashSet<String> {
    config
        .clis
        .iter()
        .filter(|cli| cli.ctrl_c_exits)
        .map(|cli| cli.name.clone())
        .collect()
}
