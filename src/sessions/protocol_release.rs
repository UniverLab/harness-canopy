//! `resize_release` parser arm. Split from `protocol.rs` for the file-size
//! gate, following `protocol_schedule.rs`.

use serde_json::Value;

use super::{id_field, ClientFrame, FrameError};

/// Parse one `resize_release{id}` frame; the caller already matched `t`.
pub(super) fn parse(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let id = id_field(value, frame)?;
    Ok(ClientFrame::ResizeRelease { id })
}
