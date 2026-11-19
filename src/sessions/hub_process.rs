//! Child-process supervision for hub sessions: hangup signaling, the PTY
//! reader thread, exit-code polling, and the nonzero-exit detail text.
//! Split from `hub.rs` for the file-size gate.

use std::io::Read;
use std::sync::Mutex;
use std::time::Duration;

use super::{HubSession, OutputState, SessionHub};

/// Send `SIGHUP` to the child process group the way closing a session in
/// the TUI does, so the shell and its children all observe the hangup.
#[cfg(unix)]
pub(super) fn send_sighup_to_group(child: &mut dyn portable_pty::Child) {
    let Some(pid) = child.process_id().map(|pid| pid as i32) else {
        return;
    };
    let _ = crate::daemon::process::send_signal_to_group(pid, libc::SIGHUP);
}

/// The last `max_bytes` of `text`, cut on a char boundary.
#[cfg(test)]
pub(crate) fn tail_chars(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let start = text.len() - max_bytes;
    let boundary = (start..=text.len())
        .find(|at| text.is_char_boundary(*at))
        .unwrap_or(text.len());
    text[boundary..].to_string()
}

/// Nonzero-exit detail: output head and tail, or the bare code when silent.
pub(super) fn exit_detail_text(state: &Mutex<OutputState>, code: i32) -> String {
    let full = state
        .lock()
        .ok()
        .map(|out| out.parser.screen().contents().trim_end().to_string());
    match full {
        Some(full) if !full.trim().is_empty() => {
            crate::sessions::hub_command::format_exit_detail(&full)
        }
        _ => format!("exited with code {code}"),
    }
}

/// PTY reader thread: ingest chunks until EOF, then wait for the child to
/// exit and record the natural exit.
pub(super) fn spawn_reader_thread(
    hub: SessionHub,
    session: std::sync::Arc<HubSession>,
    mut reader: Box<dyn Read + Send>,
) {
    std::thread::spawn(move || {
        let mut tmp = [0u8; 4096];
        loop {
            match reader.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => session.ingest_chunk(&tmp[..n]),
            }
        }
        let code = loop {
            if let Some(code) = session.status().exit_code() {
                break code;
            }
            if let Some(code) = session.poll_exit_code() {
                break code;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        hub.record_natural_exit(&session, code);
    });
}

/// Poll the child for its exit code (`100` polls of `20` ms); `None` when
/// somehow still alive afterwards.
pub(super) fn wait_for_exit_code(session: &HubSession) -> Option<i32> {
    for _ in 0..100 {
        if let Some(code) = session.poll_exit_code() {
            return Some(code);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    session.poll_exit_code()
}
