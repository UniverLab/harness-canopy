//! Persisting terminal scrollback when the daemon shuts down.
//!
//! The TUI used to write this when it closed a terminal session
//! (`tui::app::agents::session_close`); the hub owns the PTYs now, so the
//! daemon captures the same format, in the same place, on graceful shutdown
//! and `reconcile_at_startup` replays it.

use std::path::Path;

use crate::sessions::hub::{SessionHub, SessionKind};
use crate::tui::terminal_history::{load_history, save_history};

/// Rows captured per terminal — the same cap a TUI close uses.
const HISTORY_LINES: usize = 2000;

/// Save every live terminal hub session's scrollback to its per-session
/// history file.
///
/// A terminal still in alternate-screen mode is skipped: its visible frame is
/// a full-screen program's rendering, not shell history, so persisting it
/// would print a frozen UI back as literal text on the next start. Whatever
/// was saved before stays untouched.
pub fn save_terminal_histories(hub: &SessionHub, data_dir: &Path) {
    for session in hub.list() {
        if session.kind != SessionKind::Terminal || session.in_alternate_screen() {
            continue;
        }
        let scrollback = session.last_lines(HISTORY_LINES);
        let mut history = load_history(data_dir, &session.name);
        let lines: Vec<String> = scrollback.lines().map(str::to_string).collect();
        history.update_scrollback(&lines);
        save_history(data_dir, &session.name, &history);
    }
}
