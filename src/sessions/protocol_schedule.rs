//! `prompt_schedule`, `scheduled_list` and `scheduled_cancel` parser arms.
//! Split from `protocol.rs` for the file-size gate.

use serde_json::Value;

use super::{bad_request, draft_field, id_field, req_field, str_field, ClientFrame, FrameError};

/// Parse one schedule client frame; the caller has already matched `t`.
pub(super) fn parse(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let req = req_field(value, frame)?;
    let id = id_field(value, frame)?;
    match frame {
        "prompt_schedule" => Ok(ClientFrame::PromptSchedule {
            req,
            id,
            draft: draft_field(value, frame)?,
            fire_at: str_field(value, frame, "fire_at")?,
        }),
        "scheduled_list" => Ok(ClientFrame::ScheduledList { req, id }),
        "scheduled_cancel" => Ok(ClientFrame::ScheduledCancel {
            req,
            id,
            schedule_id: str_field(value, frame, "schedule_id")?,
        }),
        _ => Err(bad_request(None, format!("unknown frame '{frame}'"))),
    }
}
