//! Rendering a hub session's vt100 state into the snapshot bytes a client
//! replays, and the test-only views of that state. Split from `hub.rs` for
//! the file-size gate.

#[cfg(test)]
use super::{HubSession, SessionStatus};
use super::{OutputState, Snapshot};

/// Render every scrollback row, oldest first, each with its attributes and
/// followed by `\r\n`. A trailing home + Scroll Up parks exactly those rows
/// back into a fresh parser's scrollback, so feeding the result to one
/// reproduces this scrollback (a plain dump would leave a short scrollback on
/// the visible screen, where the snapshot's visible part wipes it). Empty when
/// there is none. Pages with `set_scrollback` and leaves the view at the live
/// screen, using only vt100's public API so the published crate builds without
/// the vendored patch.
pub(crate) fn scrollback_formatted(screen: &mut vt100::Screen) -> Vec<u8> {
    screen.set_scrollback(usize::MAX);
    let total = screen.scrollback();
    let mut contents = Vec::new();
    if total == 0 {
        return contents;
    }
    let (rows, cols) = screen.size();
    let page = usize::from(rows).max(1);
    let mut remaining = total;
    while remaining > 0 {
        screen.set_scrollback(remaining);
        let take = remaining.min(page);
        for row in screen.rows_formatted(0, cols).take(take) {
            contents.extend(row);
            // Each row is rendered from default attributes, so none may leak
            // into the next.
            contents.extend(b"\x1b[m\r\n");
        }
        remaining -= take;
    }
    screen.set_scrollback(0);
    let parked = total.min(page.saturating_sub(1).max(1));
    contents.extend(b"\x1b[H");
    contents.extend(format!("\x1b[{parked}S").as_bytes());
    contents
}

/// Render a snapshot from already-locked output state: the scrollback, the
/// visible screen and the input modes, plus the `seq` they cover. A session
/// on the alternate screen (a full-screen app) renders the main buffer
/// first, then `?1049h`, then the alternate screen, so the client paints
/// the app on its own alternate screen and keeps the main buffer for the
/// app's `?1049l`.
pub(super) fn snapshot_locked(out: &mut OutputState) -> Snapshot {
    let mut data = if out.parser.screen().alternate_screen() {
        alternate_formatted(out.parser.screen())
    } else {
        main_formatted(out.parser.screen_mut())
    };
    data.extend(out.parser.screen().input_mode_formatted());
    Snapshot {
        seq: out.seq,
        cols: out.cols,
        rows: out.rows,
        data,
    }
}

/// The main buffer: its scrollback, then its visible screen.
fn main_formatted(screen: &mut vt100::Screen) -> Vec<u8> {
    let mut data = scrollback_formatted(screen);
    data.extend(screen.contents_formatted());
    data
}

/// A screen on the alternate buffer: the main buffer (scrollback and
/// screen), `?1049h`, then the alternate screen. vt100's public API only
/// reads the active buffer, so the main one is read from a clone switched
/// back with `?47l` (mode only: no cursor restore, no clear) inside a fresh
/// parser; the live parser — possibly mid-escape between two PTY chunks —
/// is never fed anything.
fn alternate_formatted(screen: &vt100::Screen) -> Vec<u8> {
    let (rows, cols) = screen.size();
    let mut main = vt100::Parser::new(rows, cols, 0);
    *main.screen_mut() = screen.clone();
    main.process(b"\x1b[?47l");
    let mut data = main_formatted(main.screen_mut());
    data.extend(b"\x1b[?1049h");
    data.extend(screen.contents_formatted());
    data
}

#[cfg(test)]
impl HubSession {
    /// Force the status to exited without removing the session: a test-only
    /// stand-in for the instant between a child dying and the hub reaping it,
    /// so handlers' `session has exited` branch is reachable.
    pub(crate) fn mark_exited_for_test(&self, code: i32) {
        if let Ok(mut status) = self.status.lock() {
            *status = SessionStatus::Exited(code);
        }
    }

    /// Plain-text visible screen, for tests asserting client parity.
    pub(crate) fn test_contents(&self) -> Option<String> {
        self.state
            .lock()
            .ok()
            .map(|out| out.parser.screen().contents())
    }

    /// Plain-text scrollback at maximum scroll position, for tests.
    pub(crate) fn test_scrollback_contents(&self) -> Option<String> {
        self.state.lock().ok().map(|mut out| {
            out.parser.screen_mut().set_scrollback(usize::MAX);
            let contents = out.parser.screen().contents();
            out.parser.screen_mut().set_scrollback(0);
            contents
        })
    }

    /// All text reachable by paging from the oldest scrollback row to the
    /// live view, for tests asserting no shrink dropped a row. A single
    /// window (live view plus maximum scrollback) misses the middle rows
    /// whenever the scrollback outgrows the screen.
    pub(crate) fn test_all_text(&self) -> Option<String> {
        self.state.lock().ok().map(|mut out| {
            let step = usize::from(out.rows).max(1);
            out.parser.screen_mut().set_scrollback(usize::MAX);
            let max = out.parser.screen().scrollback();
            let mut all = String::new();
            let mut position = max;
            loop {
                out.parser.screen_mut().set_scrollback(position);
                all.push_str(&out.parser.screen().contents());
                all.push('\n');
                if position == 0 {
                    break;
                }
                position = position.saturating_sub(step);
            }
            out.parser.screen_mut().set_scrollback(0);
            all
        })
    }
}
