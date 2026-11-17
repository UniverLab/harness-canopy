//! Typing state for hub sessions: tracks whether the user is composing a
//! prompt so automated deliveries can wait instead of clobbering the line.

use std::time::{Duration, Instant};

/// Keystroke recency window: a session counts as composing this long after
/// its last keystroke, even with a clean line.
pub(crate) const COMPOSE_RECENCY_WINDOW: Duration = Duration::from_secs(2);

/// Outcome of an idle-gated delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The prompt was written to the PTY.
    Delivered,
    /// The user is composing; nothing was written, the send stays queued.
    Held,
}

/// Per-session typing state: the last keystroke time plus whether the
/// current line holds unsubmitted input. Starts empty so a fresh session is
/// never composing.
#[derive(Debug)]
pub(crate) struct ComposeState {
    last: Option<Instant>,
    dirty: bool,
}

impl ComposeState {
    pub(crate) fn new() -> Self {
        Self {
            last: None,
            dirty: false,
        }
    }

    /// Fold one batch of user keystrokes into the state. Cursor and report
    /// sequences (CSI, SS3, bracketed-paste markers) are ignored; everything
    /// else counts as activity. Printable bytes (UTF-8 multibyte included)
    /// and DEL dirty the line; submit/clear bytes clean it.
    pub(crate) fn observe(&mut self, data: &[u8]) {
        let mut rest = data;
        while let Some((step, kind)) = next_keystroke(rest) {
            if kind != Keystroke::Ignored {
                self.last = Some(Instant::now());
            }
            if kind == Keystroke::Dirties {
                self.dirty = true;
            } else if kind == Keystroke::Clears {
                self.dirty = false;
            }
            rest = &rest[step..];
        }
    }

    /// Conservative composing check: a dirty line always holds, and so does
    /// any keystroke inside the recency window.
    pub(crate) fn is_composing(&self) -> bool {
        self.dirty
            || self
                .last
                .is_some_and(|at| at.elapsed() < COMPOSE_RECENCY_WINDOW)
    }
}

/// What one input byte (or escape sequence) means for the composing state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keystroke {
    /// Cursor/report sequences: neither activity nor dirt.
    Ignored,
    /// Other control bytes and lone ESC: activity, line unchanged.
    Activity,
    /// Enter, Ctrl-C, Ctrl-U: the line is submitted or cleared.
    Clears,
    /// Printable bytes and DEL: the line holds unsubmitted input.
    Dirties,
}

/// Split the next keystroke off `data`: its byte length plus classification.
fn next_keystroke(data: &[u8]) -> Option<(usize, Keystroke)> {
    let first = *data.first()?;
    if first != 0x1b {
        return Some((1, classify_byte(first)));
    }
    match escape_len(data) {
        Some(len) => Some((len, Keystroke::Ignored)),
        None => Some((1, Keystroke::Activity)),
    }
}

/// Classify a non-ESC byte: submit/clear bytes clean the line, other
/// controls are activity only, and everything else (printable ASCII, DEL,
/// every UTF-8 multibyte byte) dirties it.
fn classify_byte(byte: u8) -> Keystroke {
    match byte {
        0x0d | 0x0a | 0x03 | 0x15 => Keystroke::Clears,
        0x00..=0x1f => Keystroke::Activity,
        _ => Keystroke::Dirties,
    }
}

/// Length of the CSI/SS3 sequence starting at `data[0] == ESC`, or `None`
/// for a lone or truncated ESC. Bracketed-paste markers are plain CSI
/// sequences, so they are ignored while the pasted bytes between them
/// classify normally.
fn escape_len(data: &[u8]) -> Option<usize> {
    let second = *data.get(1)?;
    if second == b'O' {
        return if data.len() >= 3 { Some(3) } else { None };
    }
    if second != b'[' {
        return None;
    }
    let mut end = 2;
    while let Some(byte) = data.get(end) {
        match byte {
            0x40..=0x7e => return Some(end + 1),
            0x20..=0x3f => end += 1,
            _ => return None,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_state_is_never_composing() {
        assert!(!ComposeState::new().is_composing());
    }

    #[test]
    fn printable_bytes_dirty_the_line() {
        let mut state = ComposeState::new();
        state.observe(b"abc");
        assert!(state.dirty);
        assert!(state.is_composing());
    }

    #[test]
    fn multibyte_text_dirties_the_line() {
        let mut state = ComposeState::new();
        state.observe("héllo世".as_bytes());
        assert!(state.dirty);
    }

    #[test]
    fn del_dirties_the_line() {
        let mut state = ComposeState::new();
        state.observe(&[0x7f]);
        assert!(state.dirty);
    }

    #[test]
    fn submit_bytes_clear_the_line() {
        for byte in [0x0d, 0x0a, 0x03, 0x15] {
            let mut state = ComposeState::new();
            state.observe(b"abc");
            state.observe(&[byte]);
            assert!(!state.dirty, "byte {byte:#x} must clear the line");
        }
    }

    #[test]
    fn cursor_and_report_sequences_are_ignored() {
        let mut state = ComposeState::new();
        state.observe(b"\x1b[A\x1b[B\x1bOC\x1bOD\x1b[I\x1b[O\x1b[<0;10;20M\x1b[200~\x1b[201~");
        assert!(state.last.is_none());
        assert!(!state.is_composing());
    }

    #[test]
    fn pasted_text_between_markers_counts() {
        let mut state = ComposeState::new();
        state.observe(b"\x1b[200~hi\x1b[201~");
        assert!(state.dirty);
    }

    #[test]
    fn other_control_bytes_are_activity_only() {
        let mut state = ComposeState::new();
        state.observe(&[0x01]);
        assert!(!state.dirty);
        assert!(state.is_composing());
    }
}
