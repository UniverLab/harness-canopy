//! Resuming a recorded conversation: the picker's `resumable_list` and the
//! `session_resume` that relaunches one row. Both are owner-only on the
//! device bridge and ungated on the local socket.
//!
//! The list applies the same rules the New dialog's resume picker uses,
//! filtered so every row it offers can actually come back: a row already
//! live in the hub, a harness with no resume machinery at all, and a
//! conversation already listed under a newer row are all dropped.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::db::session::InteractiveSession;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::models::Cli;
use crate::sessions::protocol::{
    error_frame, id_field, req_field, spawned_frame, ClientFrame, FrameError, ERR_INTERNAL,
    ERR_NOT_FOUND, ERR_SPAWN_FAILED,
};
use crate::sessions::resume::resume_recorded_session;
use crate::sessions::SessionHub;

use super::ws::{OutTx, SessionsState};

/// Parse one `resumable_list` or `session_resume` client frame; the caller
/// already matched `t`. A missing `req` (or `id` for `session_resume`)
/// answers `bad_request` naming the frame and the field.
pub(super) fn parse(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let req = req_field(value, frame)?;
    if frame == "resumable_list" {
        return Ok(ClientFrame::ResumableList { req });
    }
    let id = id_field(value, frame)?;
    Ok(ClientFrame::SessionResume { req, id })
}

/// How many rows the database scan asks for: the filters below only ever
/// narrow this set, so it is read once and capped afterwards.
const RESUME_SCAN: usize = 100;
/// Hard cap on the `resumable` answer, after filtering.
const RESUME_LIST_CAP: usize = 50;

/// One row of the `resumable` answer: a finished conversation the remote may
/// bring back.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ResumableItem {
    pub id: String,
    pub name: String,
    pub cli: String,
    /// The absolute workdir the session ran in.
    pub workdir: String,
    /// The row's `started_at`, RFC3339.
    pub started_at: String,
    /// Whether the row carries a harness conversation id, so a resume lands
    /// on the same conversation instead of the harness's newest one.
    pub has_conversation: bool,
}

/// `resumable{req,sessions}`, the answer to `resumable_list`, newest first.
pub fn resumable_frame(req: &str, sessions: &[ResumableItem]) -> String {
    serde_json::json!({"t": "resumable", "req": req, "sessions": sessions}).to_string()
}

/// The resumable picker's rows: the database's newest 100 finished
/// sessions, minus those that cannot come back the same way, capped at 50.
///
/// Dropped: a row whose id is live in the hub (resuming it would mint a
/// second process fighting the first); a row whose harness has neither
/// `resume_args` nor `session_resume_cmd` configured (it could only start
/// fresh, so it is not a conversation to resume); and a repeat of a
/// non-blank `harness_conversation_id` already listed, keeping the newest
/// row for that conversation.
pub(crate) fn resumable_candidates(hub: &SessionHub, config: &CanopyConfig) -> Vec<ResumableItem> {
    let Ok(rows) = hub.db().get_resumable_sessions(RESUME_SCAN) else {
        tracing::warn!("Failed to read the resumable sessions");
        return Vec::new();
    };
    let mut conversations: HashSet<String> = HashSet::new();
    let mut items = Vec::new();
    for row in rows {
        if items.len() >= RESUME_LIST_CAP {
            break;
        }
        if hub.get(&row.id).is_some() || !harness_can_resume(&row.cli, config) {
            continue;
        }
        let conversation = non_blank(row.harness_conversation_id.as_deref());
        if let Some(conversation) = conversation {
            if !conversations.insert(conversation.to_string()) {
                continue;
            }
        }
        items.push(ResumableItem {
            id: row.id,
            name: row.name,
            cli: row.cli,
            workdir: row.working_dir,
            started_at: row.started_at,
            has_conversation: conversation.is_some(),
        });
    }
    items
}

/// Whether the configured harness can relaunch a recorded conversation: it
/// must define `resume_args` (the most-recent fallback) or
/// `session_resume_cmd` (resume by identity). A harness absent from the
/// config counts as unable.
fn harness_can_resume(cli: &str, config: &CanopyConfig) -> bool {
    config
        .get_cli(Cli::from_str(cli).as_str())
        .is_some_and(|entry| {
            non_blank(entry.resume_args.as_deref()).is_some()
                || non_blank(entry.session_resume_cmd.as_deref()).is_some()
        })
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// `resumable_list`: the picker's rows, read off the executor like every
/// other blocking hub call.
pub(crate) fn handle_resumable_list(state: &Arc<SessionsState>, out_tx: &OutTx, req: String) {
    let hub = state.hub.clone();
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let listed = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<ResumableItem>> {
            let data_dir = crate::ensure_data_dir()?;
            let config = CanopyConfig::load(&data_dir);
            Ok(resumable_candidates(&hub, &config))
        })
        .await;
        match listed {
            Ok(Ok(items)) => {
                let _ = out_tx.send(resumable_frame(&req, &items));
            }
            Ok(Err(e)) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    &format!("resumable list failed: {e}"),
                ));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    "resumable list task failed",
                ));
            }
        }
    });
}

/// `session_resume`: relaunch one recorded row under its own id. A row
/// already live in the hub answers `spawned` and relaunches nothing, so a
/// double tap is harmless. A row that is not a finished non-bridge session
/// answers `not_found`; a spawn that starts no process answers
/// `error{code:"spawn_failed"}`. The hub's `session_added` announces the
/// session, as it does for any spawn.
pub(crate) fn handle_session_resume(
    state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    id: String,
) {
    if state.hub.get(&id).is_some() {
        let _ = out_tx.send(spawned_frame(&req, &id));
        return;
    }
    let hub = state.hub.clone();
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let row = tokio::task::spawn_blocking({
            let db = Arc::clone(hub.db());
            let id = id.clone();
            move || db.get_interactive_session(&id)
        })
        .await;
        let session = match row {
            Ok(Ok(Some(session))) if resume_eligible(&session) => session,
            Ok(Ok(_)) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_NOT_FOUND,
                    &format!("no resumable session '{id}'"),
                ));
                return;
            }
            Ok(Err(e)) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    &format!("session lookup failed: {e}"),
                ));
                return;
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    "session lookup task failed",
                ));
                return;
            }
        };
        let resumed = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let data_dir = crate::ensure_data_dir()?;
            let config = CanopyConfig::load(&data_dir);
            let boot_id = crate::system::boot_id();
            resume_recorded_session(&hub, &session, &config, boot_id.as_deref())
        })
        .await;
        match resumed {
            Ok(Ok(())) => {
                let _ = out_tx.send(spawned_frame(&req, &id));
            }
            Ok(Err(e)) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_SPAWN_FAILED, &e.to_string()));
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(Some(&req), ERR_INTERNAL, "resume task failed"));
            }
        }
    });
}

/// Whether a row may be resumed: it finished (`completed` or `error`) and is
/// not a bridge sidecar, which is a proxy and never a conversation.
fn resume_eligible(session: &InteractiveSession) -> bool {
    matches!(session.status.as_str(), "completed" | "error") && session.session_type != "bridge"
}
