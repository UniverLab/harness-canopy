//! `graph_control` parser arm: pause or continue one graph.
//! Split from `protocol.rs` for the file-size gate.

use serde_json::Value;

use super::{bad_request, id_field, req_field, ClientFrame, FrameError};

/// Which way `graph_control` moves the graph: pause it, or continue it by
/// retrying the node that was mid-flight when it paused. Continue never
/// skips: skipping abandons a spec, so the remote only retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphControlAction {
    Pause,
    Continue,
}

/// Parse one `graph_control` client frame; the caller has already matched
/// `t`. `action` is `"pause"` or `"continue"`; anything else (or a missing
/// field) answers `bad_request` naming the frame and the field.
pub(super) fn parse(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let req = req_field(value, frame)?;
    let id = id_field(value, frame)?;
    let action = match value.get("action") {
        Some(Value::String(action)) if action == "pause" => GraphControlAction::Pause,
        Some(Value::String(action)) if action == "continue" => GraphControlAction::Continue,
        _ => {
            return Err(bad_request(
                Some(req),
                format!("{frame}: field 'action' must be 'pause' or 'continue'"),
            ));
        }
    };
    Ok(ClientFrame::GraphControl { req, id, action })
}
