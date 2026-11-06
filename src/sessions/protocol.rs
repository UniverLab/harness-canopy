//! Session protocol v2: JSON text frames over the daemon WebSocket.
//!
//! Every frame carries `t`. Client frames are parsed by
//! [`parse_client_frame`]; host frames are built by the `*_frame` helpers.
//! An unknown `t` or a missing field answers `error{code:"bad_request"}`
//! naming the frame and the field.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::prompt_draft::PromptDraft;

pub use super::host_frame::{parse_host_frame, HostFrame};

#[path = "protocol_frames.rs"]
mod protocol_frames;
#[path = "protocol_graph.rs"]
mod protocol_graph;
#[path = "protocol_put.rs"]
mod protocol_put;
#[path = "protocol_release.rs"]
mod protocol_release;
#[path = "protocol_schedule.rs"]
mod protocol_schedule;
pub use super::protocol_dir::{dir_list_frame, DirEntry};
#[cfg(test)]
pub use super::protocol_resume::{resumable_frame, ResumableItem};
#[cfg(test)]
pub(crate) use protocol_frames::harnesses_in;
pub use protocol_frames::{
    attach_frame, detach_frame, error_frame, exited_frame, file_frame, hello_frame, input_frame,
    installed_harnesses, kill_frame, list_frame, ok_frame, out_frame, prompt_frame, resize_frame,
    resized_frame, session_added_frame, session_removed_frame, sessions_frame, shelf_added_frame,
    shelf_delete_frame, shelf_frame, shelf_get_frame, shelf_list_frame, shelf_missing_frame,
    shelf_removed_frame, size_released_frame, snapshot_frame, snapshot_more_frame, spawn_frame,
    spawn_frame_full, spawned_frame, SpawnFrameArgs,
};
#[cfg(test)]
pub use protocol_frames::{resumable_list_frame, session_resume_frame, OWNER_FEATURES};
pub use protocol_graph::GraphControlAction;
#[cfg(test)]
pub use protocol_put::{
    shelf_put_abort_frame, shelf_put_begin_frame, shelf_put_end_frame, shelf_put_part_frame,
};
pub use protocol_put::{shelf_put_ack_frame, shelf_put_ready_frame};

/// Protocol version the hub speaks.
pub const PROTO_VERSION: u32 = 2;

/// Error codes for `error` frames.
pub const ERR_BAD_REQUEST: &str = "bad_request";
pub const ERR_NOT_FOUND: &str = "not_found";
pub const ERR_FORBIDDEN: &str = "forbidden";
pub const ERR_SPAWN_FAILED: &str = "spawn_failed";
pub const ERR_DELIVERY_FAILED: &str = "delivery_failed";
pub const ERR_INTERNAL: &str = "internal";
pub const ERR_BUSY: &str = "busy";
pub const ERR_REVOKED: &str = "revoked";
pub const ERR_TOO_LARGE: &str = "too_large";
pub const ERR_CHECKSUM_MISMATCH: &str = "checksum_mismatch";

/// One shelf file, as carried by `shelf` / `shelf_added` (FR3).
/// `session_id: null` is the host shelf; the wire key for the sender label
/// is `from`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShelfItem {
    pub file_id: String,
    pub session_id: Option<String>,
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub caption: Option<String>,
    #[serde(rename = "from")]
    pub from_label: String,
    pub created_at: String,
}

/// Session metadata row for `sessions` / `session_added` frames.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionItem {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub cli: String,
    pub model: Option<String>,
    pub workdir: String,
    pub started_at: String,
    pub pid: Option<i64>,
    pub cols: u16,
    pub rows: u16,
    pub last_output_at: String,
    pub status: String,
    pub exit_code: Option<i32>,
}

/// A parsed client frame.
#[derive(Debug)]
pub enum ClientFrame {
    List {
        req: String,
    },
    Attach {
        id: String,
    },
    Detach {
        id: String,
    },
    Input {
        id: String,
        data: Vec<u8>,
    },
    Prompt {
        req: String,
        id: String,
        text: String,
    },
    /// `prompt{req,id,draft}`: build the text hub-side, then deliver it like
    /// [`ClientFrame::Prompt`].
    PromptDraft {
        req: String,
        id: String,
        draft: PromptDraft,
    },
    /// `prompt_schedule{req,id,draft,fire_at}`: build the text hub-side and
    /// store it for delivery at `fire_at` (RFC 3339); the host answers
    /// `scheduled`.
    PromptSchedule {
        req: String,
        id: String,
        draft: PromptDraft,
        fire_at: String,
    },
    /// `scheduled_list{req,id}`: the session's pending schedules, soonest
    /// first; the host answers `scheduled_list`.
    ScheduledList {
        req: String,
        id: String,
    },
    /// `scheduled_cancel{req,id,schedule_id}`: drop one pending schedule;
    /// the host answers `ok`.
    ScheduledCancel {
        req: String,
        id: String,
        schedule_id: String,
    },
    Resize {
        id: String,
        cols: u16,
        rows: u16,
    },
    /// `resize_release{id}`: this device stops sizing the session (its pane
    /// is gone or it is leaving); the host re-asserts its own size. No
    /// answer frame.
    ResizeRelease {
        id: String,
    },
    Spawn {
        req: String,
        kind: String,
        cli: String,
        model: Option<String>,
        workdir: String,
        name: Option<String>,
        prompt: Option<String>,
        ext: Box<SpawnExt>,
    },
    Kill {
        req: String,
        id: String,
    },
    /// `shelf_list{req,id?}`: one shelf's items (`id` absent = host shelf).
    ShelfList {
        req: String,
        id: Option<String>,
    },
    /// `shelf_get{req,file_id}`: stream one file back as `file` parts.
    ShelfGet {
        req: String,
        file_id: String,
    },
    /// `shelf_delete{req,file_id}`: remove one file from its shelf; the host
    /// answers `ok` and broadcasts `shelf_removed`.
    ShelfDelete {
        req: String,
        file_id: String,
    },
    /// `shelf_put_begin{req,id?,name,size,mime?,sha256}`: reserve an upload
    /// slot; the host answers `shelf_put_ready{req,file_id}` or an error.
    ShelfPutBegin {
        req: String,
        id: Option<String>,
        name: String,
        size: u64,
        mime: Option<String>,
        sha256: String,
    },
    /// `shelf_put_part{file_id,part,data}`: one base64 chunk, numbered from
    /// 0; the host acknowledges every eighth part.
    ShelfPutPart {
        file_id: String,
        part: u64,
        data: Vec<u8>,
    },
    /// `shelf_put_end{file_id}`: verify the checksum and commit the file.
    ShelfPutEnd {
        file_id: String,
    },
    /// `shelf_put_abort{file_id}`: delete the partial upload, no answer.
    ShelfPutAbort {
        file_id: String,
    },
    /// `compose{req,draft}`: build the text hub-side and answer `composed`.
    Compose {
        req: String,
        draft: PromptDraft,
    },
    /// `catalog{req}`: answer the builder `catalog`.
    Catalog {
        req: String,
    },
    /// `graphs{req}`: the remote graph list (`graphs` answers).
    Graphs {
        req: String,
    },
    /// `graph_watch{req,id}`: follow one graph — answers `graph` with its
    /// export document and state, then pushes `graph_state` while the state
    /// differs. At most four watches live per connection.
    GraphWatch {
        req: String,
        id: String,
    },
    /// `graph_unwatch{id}`: stop one watch; no answer frame.
    GraphUnwatch {
        id: String,
    },
    /// `graph_control{req,id,action}`: pause or continue one graph
    /// (`action` is `"pause"` or `"continue"`). Owner-only on the remote
    /// bridge; the local socket answers every peer. Success is `ok`; the
    /// state change itself reaches watchers through the `graph_state` poll.
    GraphControl {
        req: String,
        id: String,
        action: GraphControlAction,
    },
    /// `resumable_list{req}`: the recorded conversations the picker offers,
    /// newest first. Owner-only on the remote bridge.
    ResumableList {
        req: String,
    },
    /// `session_resume{req,id}`: relaunch one recorded conversation under its
    /// own id. Owner-only on the remote bridge.
    SessionResume {
        req: String,
        id: String,
    },
    /// `dir_list{req,path?}`: list one directory's subdirectories for the
    /// New dialog's folder picker. `path` absent, empty, `~` or `~/…` means
    /// the host home or a path under it. Owner-only on the remote bridge.
    DirList {
        req: String,
        path: Option<String>,
    },
    /// `host.metrics{req}`: one host-metrics frame. Remote-only: the local
    /// socket answers `error{code:"forbidden"}`.
    HostMetrics {
        req: String,
    },
    /// `host.metrics.watch{interval_s?}`: push the metrics frame every N
    /// seconds (clamped to 5..=60, default 10). Remote-only.
    HostMetricsWatch {
        interval_s: Option<u64>,
    },
    /// `host.metrics.unwatch`: stop the metrics push; no answer frame.
    /// Remote-only.
    HostMetricsUnwatch,
}

/// Explicit agent launch fields from the New dialog (boxed so the `Spawn`
/// variant stays small). `None` falls back to the registry hub-side.
#[derive(Debug, Default)]
pub struct SpawnExt {
    pub interactive_args: Option<String>,
    pub fallback_args: Option<String>,
    pub model_flag: Option<String>,
    pub id: Option<String>,
    pub seed_id: Option<String>,
    pub accent: Option<String>,
    /// The harness's own conversation identifier for this launch, when the
    /// launcher knows it (a resume of a recorded conversation). The hub
    /// records it on the session row so the next resume of the same
    /// conversation resolves back to this session.
    pub harness_conversation_id: Option<String>,
    /// Skip the harness's approval prompts: the hub appends the registry's
    /// `yolo_flag` to the interactive args. Only for `kind:"agent"`, and only
    /// for a harness that defines one.
    pub yolo: bool,
    /// A resume relaunching an existing row: reactivate it without rewriting
    /// its recorded launch args. Absent or `null` reads as `false`.
    pub preserve_args: bool,
}

/// A rejected client frame: the request id to echo (when the frame carried
/// a usable one) and the `bad_request` message naming frame and field.
#[derive(Debug)]
pub struct FrameError {
    pub req: Option<String>,
    pub message: String,
}

fn bad_request(req: Option<String>, message: String) -> FrameError {
    FrameError { req, message }
}

/// Parse one client text frame.
pub fn parse_client_frame(text: &str) -> Result<ClientFrame, FrameError> {
    let value: Value =
        serde_json::from_str(text).map_err(|_| bad_request(None, "invalid JSON".to_string()))?;
    let frame = match value.get("t") {
        Some(Value::String(t)) => t.clone(),
        Some(_) => {
            return Err(bad_request(None, "field 't' must be a string".to_string()));
        }
        None => return Err(bad_request(None, "missing field 't'".to_string())),
    };
    match frame.as_str() {
        "list" => {
            let req = req_field(&value, &frame)?;
            Ok(ClientFrame::List { req })
        }
        "attach" => {
            let id = id_field(&value, &frame)?;
            Ok(ClientFrame::Attach { id })
        }
        "detach" => {
            let id = id_field(&value, &frame)?;
            Ok(ClientFrame::Detach { id })
        }
        "input" => {
            let id = id_field(&value, &frame)?;
            let data = data_field(&value, &frame)?;
            Ok(ClientFrame::Input { id, data })
        }
        "prompt" => parse_prompt_frame(&value, &frame),
        "prompt_schedule" | "scheduled_list" | "scheduled_cancel" => {
            protocol_schedule::parse(&value, &frame)
        }
        "resize" => {
            let id = id_field(&value, &frame)?;
            let cols = size_field(&value, &frame, "cols")?;
            let rows = size_field(&value, &frame, "rows")?;
            Ok(ClientFrame::Resize { id, cols, rows })
        }
        "resize_release" => protocol_release::parse(&value, &frame),
        "spawn" => parse_spawn_frame(&value, &frame),
        "kill" => {
            let req = req_field(&value, &frame)?;
            let id = id_field(&value, &frame)?;
            Ok(ClientFrame::Kill { req, id })
        }
        "shelf_list" => {
            let req = req_field(&value, &frame)?;
            let id = opt_str_field(&value, &frame, "id")?;
            Ok(ClientFrame::ShelfList { req, id })
        }
        "shelf_get" => {
            let req = req_field(&value, &frame)?;
            let file_id = str_field(&value, &frame, "file_id")?;
            Ok(ClientFrame::ShelfGet { req, file_id })
        }
        "shelf_delete" => {
            let req = req_field(&value, &frame)?;
            let file_id = str_field(&value, &frame, "file_id")?;
            Ok(ClientFrame::ShelfDelete { req, file_id })
        }
        "shelf_put_begin" | "shelf_put_part" | "shelf_put_end" | "shelf_put_abort" => {
            protocol_put::parse(&value, &frame)
        }
        "compose" => {
            let req = req_field(&value, &frame)?;
            let draft = draft_field(&value, &frame)?;
            Ok(ClientFrame::Compose { req, draft })
        }
        "catalog" => {
            let req = req_field(&value, &frame)?;
            Ok(ClientFrame::Catalog { req })
        }
        "graphs" => {
            let req = req_field(&value, &frame)?;
            Ok(ClientFrame::Graphs { req })
        }
        "graph_watch" => {
            let req = req_field(&value, &frame)?;
            let id = id_field(&value, &frame)?;
            Ok(ClientFrame::GraphWatch { req, id })
        }
        "graph_unwatch" => {
            let id = id_field(&value, &frame)?;
            Ok(ClientFrame::GraphUnwatch { id })
        }
        "graph_control" => protocol_graph::parse(&value, &frame),
        "resumable_list" | "session_resume" => super::protocol_resume::parse(&value, &frame),
        "dir_list" => super::protocol_dir::parse(&value, &frame),
        "host.metrics" => {
            let req = req_field(&value, &frame)?;
            Ok(ClientFrame::HostMetrics { req })
        }
        "host.metrics.watch" => {
            let interval_s = opt_u64_field(&value, &frame, "interval_s")?;
            Ok(ClientFrame::HostMetricsWatch { interval_s })
        }
        "host.metrics.unwatch" => Ok(ClientFrame::HostMetricsUnwatch),
        _ => Err(bad_request(None, format!("unknown frame '{frame}'"))),
    }
}

/// Parse a `prompt` frame: `text` xor `draft`.
fn parse_prompt_frame(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let req = req_field(value, frame)?;
    let id = id_field(value, frame)?;
    match (present_field(value, "text"), present_field(value, "draft")) {
        (true, false) => {
            let text = str_field(value, frame, "text")?;
            Ok(ClientFrame::Prompt { req, id, text })
        }
        (false, true) => {
            let draft = draft_field(value, frame)?;
            Ok(ClientFrame::PromptDraft { req, id, draft })
        }
        (true, true) => Err(bad_request(
            None,
            format!("{frame}: fields 'text' and 'draft' are mutually exclusive"),
        )),
        (false, false) => Err(bad_request(
            None,
            format!("{frame}: missing field 'text' or 'draft'"),
        )),
    }
}

/// Parse a `spawn` frame with its agent launch extras.
fn parse_spawn_frame(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let req = req_field(value, frame)?;
    let kind = str_field(value, frame, "kind")?;
    let cli = opt_str_field(value, frame, "cli")?.unwrap_or_default();
    let model = opt_str_field(value, frame, "model")?;
    let workdir = str_field(value, frame, "workdir")?;
    let name = opt_str_field(value, frame, "name")?;
    let prompt = opt_str_field(value, frame, "prompt")?;
    let interactive_args = opt_str_field(value, frame, "interactive_args")?;
    let fallback_args = opt_str_field(value, frame, "fallback_args")?;
    let model_flag = opt_str_field(value, frame, "model_flag")?;
    let id = opt_str_field(value, frame, "id")?;
    let seed_id = opt_str_field(value, frame, "seed_id")?;
    let accent = opt_str_field(value, frame, "accent")?;
    let harness_conversation_id = opt_str_field(value, frame, "harness_conversation_id")?;
    let yolo = opt_bool_field(value, frame, "yolo")?;
    let preserve_args = opt_bool_field(value, frame, "preserve_args")?;
    Ok(ClientFrame::Spawn {
        req,
        kind,
        cli,
        model,
        workdir,
        name,
        prompt,
        ext: Box::new(SpawnExt {
            interactive_args,
            fallback_args,
            model_flag,
            id,
            seed_id,
            accent,
            harness_conversation_id,
            yolo,
            preserve_args,
        }),
    })
}

pub(super) fn req_field(value: &Value, frame: &str) -> Result<String, FrameError> {
    str_field(value, frame, "req")
}

pub(super) fn id_field(value: &Value, frame: &str) -> Result<String, FrameError> {
    str_field(value, frame, "id")
}

fn str_field(value: &Value, frame: &str, field: &str) -> Result<String, FrameError> {
    match value.get(field) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Null) | None => Err(bad_request(
            None,
            format!("{frame}: missing field '{field}'"),
        )),
        Some(_) => Err(bad_request(
            None,
            format!("{frame}: field '{field}' must be a string"),
        )),
    }
}

pub(super) fn opt_str_field(
    value: &Value,
    frame: &str,
    field: &str,
) -> Result<Option<String>, FrameError> {
    match value.get(field) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(
            None,
            format!("{frame}: field '{field}' must be a string"),
        )),
    }
}

fn u64_field(value: &Value, frame: &str, field: &str) -> Result<u64, FrameError> {
    opt_u64_field(value, frame, field)?
        .ok_or_else(|| bad_request(None, format!("{frame}: missing field '{field}'")))
}

fn opt_u64_field(value: &Value, frame: &str, field: &str) -> Result<Option<u64>, FrameError> {
    match value.get(field) {
        Some(Value::Number(n)) => n
            .as_u64()
            .map(Some)
            .ok_or_else(|| bad_request(None, format!("{frame}: field '{field}' must be a number"))),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(
            None,
            format!("{frame}: field '{field}' must be a number"),
        )),
    }
}

/// An optional boolean field: absent or `null` reads as `false`.
fn opt_bool_field(value: &Value, frame: &str, field: &str) -> Result<bool, FrameError> {
    match value.get(field) {
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(Value::Null) | None => Ok(false),
        Some(_) => Err(bad_request(
            None,
            format!("{frame}: field '{field}' must be a boolean"),
        )),
    }
}

fn size_field(value: &Value, frame: &str, field: &str) -> Result<u16, FrameError> {
    match value.get(field) {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .filter(|v| *v >= 1)
            .ok_or_else(|| {
                bad_request(
                    None,
                    format!("{frame}: field '{field}' must be an integer 1..=65535"),
                )
            }),
        Some(Value::Null) | None => Err(bad_request(
            None,
            format!("{frame}: missing field '{field}'"),
        )),
        Some(_) => Err(bad_request(
            None,
            format!("{frame}: field '{field}' must be an integer 1..=65535"),
        )),
    }
}

fn data_field(value: &Value, frame: &str) -> Result<Vec<u8>, FrameError> {
    use base64::Engine as _;
    let encoded = str_field(value, frame, "data")?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| bad_request(None, format!("{frame}: field 'data' is not valid base64")))
}

/// Whether `field` carries a value (`null` counts as absent, matching
/// [`str_field`]).
fn present_field(value: &Value, field: &str) -> bool {
    matches!(value.get(field), Some(field_value) if !field_value.is_null())
}

fn draft_field(value: &Value, frame: &str) -> Result<PromptDraft, FrameError> {
    match value.get("draft") {
        Some(draft) if !draft.is_null() => serde_json::from_value(draft.clone())
            .map_err(|e| bad_request(None, format!("{frame}: field 'draft' is invalid: {e}"))),
        _ => Err(bad_request(None, format!("{frame}: missing field 'draft'"))),
    }
}

fn encode_b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}
