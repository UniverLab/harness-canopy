//! Child-process supervision for hub sessions: hangup signaling, the PTY
//! reader thread, exit-code polling, and the nonzero-exit detail text.
//! Split from `hub.rs` for the file-size gate.

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::db::session::SessionRegistration;
use crate::db::Database;
use crate::domain::models::Cli;

use super::{HubSession, OutputState, SessionHub, SessionKind, SpawnRequest};

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

/// Write the `interactive_sessions` / `terminal_sessions` row the TUI
/// writes for the same kind of launch.
pub(super) fn insert_db_row(
    db: &Database,
    req: &SpawnRequest,
    session: &HubSession,
    args: Option<&str>,
    generated_cid: Option<&str>,
) {
    match req.kind {
        SessionKind::Agent => {
            let boot_id = crate::system::boot_id();
            let session_name = session.name();
            let harness_conversation_id = req
                .harness_conversation_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .or(generated_cid);
            let registration = SessionRegistration {
                id: &session.id,
                name: &session_name,
                cli: req.cli.as_str(),
                working_dir: req.workdir.as_str(),
                args,
                pid: session.pid,
                session_type: "interactive",
                boot_id: boot_id.as_deref(),
                harness_conversation_id,
                preserve_args: req.preserve_args,
            };
            if let Err(e) = db.register_interactive_session(&registration) {
                tracing::warn!("Failed to register hub session: {e}");
            }
        }
        SessionKind::Terminal => {
            let session_name = session.name();
            let _ = db.insert_terminal_session(
                &session.id,
                &session_name,
                req.cli.as_str(),
                req.workdir.as_str(),
            );
        }
    }
}

/// A fresh `muse` or `antigravity` spawn is a new conversation: discover
/// the harness's own id for it on a background thread, so a later restart
/// resumes this conversation. Done here, not by each launcher, so every
/// spawn path (TUI, remote, scheduler) records it.
pub(super) fn record_harness_conversation(
    db: Arc<Database>,
    req: &SpawnRequest,
    session: &HubSession,
    since: SystemTime,
    muse_store_override: Option<PathBuf>,
    antigravity_history_override: Option<PathBuf>,
) {
    if req.kind != SessionKind::Agent
        || req.preserve_args
        || req
            .harness_conversation_id
            .as_deref()
            .is_some_and(|s| !s.is_empty())
    {
        return;
    }
    match Cli::from_str(&req.cli).as_str() {
        "muse" => {
            let store =
                muse_store_override.or_else(crate::sessions::muse_conversation::muse_store_dir);
            crate::sessions::muse_conversation::record_fresh_conversation(
                db,
                session.id.clone(),
                req.workdir.clone(),
                since,
                session.pid,
                store,
            );
        }
        "antigravity" => {
            crate::sessions::antigravity_conversation::record_fresh_conversation(
                db,
                session.id.clone(),
                req.workdir.clone(),
                since,
                session.pid,
                antigravity_history_override,
            );
        }
        _ => {}
    }
}
