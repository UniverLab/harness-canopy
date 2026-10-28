//! Session protocol v1: JSON text frames over the daemon WebSocket.
//!
//! Every frame carries `t`. Client frames are parsed by
//! [`parse_client_frame`]; host frames are built by the `*_frame` helpers.
//! An unknown `t` or a missing field answers `error{code:"bad_request"}`
//! naming the frame and the field.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::prompt_draft::PromptDraft;

pub use super::host_frame::{parse_host_frame, HostFrame};

/// Protocol version the hub speaks.
pub const PROTO_VERSION: u32 = 1;

/// Error codes for `error` frames.
pub const ERR_BAD_REQUEST: &str = "bad_request";
pub const ERR_NOT_FOUND: &str = "not_found";
pub const ERR_FORBIDDEN: &str = "forbidden";
pub const ERR_SPAWN_FAILED: &str = "spawn_failed";
pub const ERR_DELIVERY_FAILED: &str = "delivery_failed";
pub const ERR_INTERNAL: &str = "internal";
pub const ERR_BUSY: &str = "busy";

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
    Resize {
        id: String,
        cols: u16,
        rows: u16,
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
    /// `compose{req,draft}`: build the text hub-side and answer `composed`.
    Compose {
        req: String,
        draft: PromptDraft,
    },
    /// `catalog{req}`: answer the builder `catalog`.
    Catalog {
        req: String,
    },
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
        "resize" => {
            let id = id_field(&value, &frame)?;
            let cols = size_field(&value, &frame, "cols")?;
            let rows = size_field(&value, &frame, "rows")?;
            Ok(ClientFrame::Resize { id, cols, rows })
        }
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
        "compose" => {
            let req = req_field(&value, &frame)?;
            let draft = draft_field(&value, &frame)?;
            Ok(ClientFrame::Compose { req, draft })
        }
        "catalog" => {
            let req = req_field(&value, &frame)?;
            Ok(ClientFrame::Catalog { req })
        }
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
    let cli = str_field(value, frame, "cli")?;
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
        }),
    })
}

fn req_field(value: &Value, frame: &str) -> Result<String, FrameError> {
    str_field(value, frame, "req")
}

fn id_field(value: &Value, frame: &str) -> Result<String, FrameError> {
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

fn opt_str_field(value: &Value, frame: &str, field: &str) -> Result<Option<String>, FrameError> {
    match value.get(field) {
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(bad_request(
            None,
            format!("{frame}: field '{field}' must be a string"),
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

/// `hello{proto,host_label,features}`, sent on connect. `features`
/// advertises the optional extensions this host speaks — currently just
/// `shelf` — so a client can tell an older host from a newer one without
/// probing.
pub fn hello_frame(host_label: &str) -> String {
    serde_json::json!({
        "t": "hello", "proto": PROTO_VERSION, "host_label": host_label,
        "features": ["shelf"],
    })
    .to_string()
}

/// `sessions{req,items}`, the answer to `list`.
pub fn sessions_frame(req: &str, items: &[SessionItem]) -> String {
    serde_json::json!({"t": "sessions", "req": req, "items": items}).to_string()
}

/// `snapshot{id,cols,rows,seq,data}`, sent on `attach` (and on lag resync).
pub fn snapshot_frame(id: &str, cols: u16, rows: u16, seq: u64, data: &[u8]) -> String {
    serde_json::json!({
        "t": "snapshot", "id": id, "cols": cols, "rows": rows, "seq": seq, "data": encode_b64(data),
    })
    .to_string()
}

/// `out{id,seq,data}`: one output chunk after the snapshot's `seq`.
pub fn out_frame(id: &str, seq: u64, chunk: &[u8]) -> String {
    serde_json::json!({"t": "out", "id": id, "seq": seq, "data": encode_b64(chunk)}).to_string()
}

/// `snapshot_more{id,data,last}`: one continuation part of a snapshot whose
/// first part travelled in the `snapshot` frame. Remote peers use it because
/// a data-channel frame is size-bounded.
pub fn snapshot_more_frame(id: &str, data: &[u8], last: bool) -> String {
    serde_json::json!({
        "t": "snapshot_more", "id": id, "data": encode_b64(data), "last": last,
    })
    .to_string()
}

/// `exited{id,code}`.
pub fn exited_frame(id: &str, code: i32) -> String {
    serde_json::json!({"t": "exited", "id": id, "code": code}).to_string()
}

/// `session_added{item}`, broadcast to every connection.
pub fn session_added_frame(item: &SessionItem) -> String {
    serde_json::json!({"t": "session_added", "item": item}).to_string()
}

/// `session_removed{id}`, broadcast to every connection.
pub fn session_removed_frame(id: &str) -> String {
    serde_json::json!({"t": "session_removed", "id": id}).to_string()
}

/// `spawned{req,id}`, the answer to `spawn`.
pub fn spawned_frame(req: &str, id: &str) -> String {
    serde_json::json!({"t": "spawned", "req": req, "id": id}).to_string()
}

/// `ok{req}`, the answer to `prompt` and `kill`.
pub fn ok_frame(req: &str) -> String {
    serde_json::json!({"t": "ok", "req": req}).to_string()
}

/// `error{req?,code,message}`; `req` is omitted when the frame carried none.
pub fn error_frame(req: Option<&str>, code: &str, message: &str) -> String {
    let mut frame = serde_json::json!({"t": "error", "code": code, "message": message});
    if let Some(req) = req {
        frame["req"] = Value::String(req.to_string());
    }
    frame.to_string()
}

/// `shelf{req,id?,items}`: the answer to `shelf_list`, newest first.
pub fn shelf_frame(req: &str, id: Option<&str>, items: &[ShelfItem]) -> String {
    serde_json::json!({"t": "shelf", "req": req, "id": id, "items": items}).to_string()
}

/// `shelf_added{item}`, broadcast to every connection when a shelf gains a
/// file.
pub fn shelf_added_frame(item: &ShelfItem) -> String {
    serde_json::json!({"t": "shelf_added", "item": item}).to_string()
}

/// `shelf_removed{file_id}`, broadcast to every connection when a shelf
/// loses a file (delete, prune, retention or orphan sweep).
pub fn shelf_removed_frame(file_id: &str) -> String {
    serde_json::json!({"t": "shelf_removed", "file_id": file_id}).to_string()
}

/// `file{req,file_id,part,data,last}`: one base64 chunk of a `shelf_get`
/// answer, at most [`crate::sessions::shelf::READ_CHUNK_BYTES`] raw bytes
/// per part; `last` marks the final chunk (an empty file is one empty part
/// with `last: true`).
pub fn file_frame(req: &str, file_id: &str, part: u32, data: &[u8], last: bool) -> String {
    serde_json::json!({
        "t": "file", "req": req, "file_id": file_id, "part": part,
        "data": encode_b64(data), "last": last,
    })
    .to_string()
}

// ── Client frame builders ─────────────────────────────────────────────
// The TUI builds its outbound frames with these; each one round-trips
// through `parse_client_frame`.

/// `list{req}`: ask for the `sessions` listing.
pub fn list_frame(req: &str) -> String {
    serde_json::json!({"t": "list", "req": req}).to_string()
}

/// `attach{id}`: subscribe to one session's snapshot plus `out` frames.
pub fn attach_frame(id: &str) -> String {
    serde_json::json!({"t": "attach", "id": id}).to_string()
}

/// `detach{id}`: drop one session subscription.
pub fn detach_frame(id: &str) -> String {
    serde_json::json!({"t": "detach", "id": id}).to_string()
}

/// `input{id,data}`: raw bytes for one session's PTY stdin.
pub fn input_frame(id: &str, data: &[u8]) -> String {
    serde_json::json!({"t": "input", "id": id, "data": encode_b64(data)}).to_string()
}

/// `prompt{req,id,text}`: one submitted prompt through the paste path.
pub fn prompt_frame(req: &str, id: &str, text: &str) -> String {
    serde_json::json!({"t": "prompt", "req": req, "id": id, "text": text}).to_string()
}

/// `resize{id,cols,rows}`: resize one session's PTY and parser.
pub fn resize_frame(id: &str, cols: u16, rows: u16) -> String {
    serde_json::json!({"t": "resize", "id": id, "cols": cols, "rows": rows}).to_string()
}

/// `spawn{req,kind,cli,model?,workdir,name?,prompt?,interactive_args?,fallback_args?,model_flag?,id?,seed_id?,accent?}`:
/// spawn a hub session. The agent launch fields mirror what the TUI passes to
/// `InteractiveAgent::spawn` today: explicit args (resume/yolo included),
/// the resume identity, the seed and the accent (`"r,g,b"`). `None` values
/// serialize as `null`, which the hub reads as absent and falls back to the
/// registry for.
pub fn spawn_frame(
    req: &str,
    kind: &str,
    cli: &str,
    model: Option<&str>,
    workdir: &str,
    name: Option<&str>,
    prompt: Option<&str>,
) -> String {
    spawn_frame_full(&SpawnFrameArgs {
        req,
        kind,
        cli,
        model,
        workdir,
        name,
        prompt,
        ext: &SpawnExt::default(),
    })
}

/// All fields of a `spawn` frame; the launch extras travel boxed in `ext`.
pub struct SpawnFrameArgs<'a> {
    pub req: &'a str,
    pub kind: &'a str,
    pub cli: &'a str,
    pub model: Option<&'a str>,
    pub workdir: &'a str,
    pub name: Option<&'a str>,
    pub prompt: Option<&'a str>,
    pub ext: &'a SpawnExt,
}

/// Full `spawn` frame with the agent launch fields.
pub fn spawn_frame_full(args: &SpawnFrameArgs<'_>) -> String {
    let ext = args.ext;
    serde_json::json!({
        "t": "spawn", "req": args.req, "kind": args.kind, "cli": args.cli,
        "model": args.model, "workdir": args.workdir, "name": args.name, "prompt": args.prompt,
        "interactive_args": ext.interactive_args, "fallback_args": ext.fallback_args,
        "model_flag": ext.model_flag, "id": ext.id, "seed_id": ext.seed_id, "accent": ext.accent,
    })
    .to_string()
}

/// `kill{req,id}`: end one session.
pub fn kill_frame(req: &str, id: &str) -> String {
    serde_json::json!({"t": "kill", "req": req, "id": id}).to_string()
}

/// `shelf_list{req,id?}`: ask for one shelf's items; `id` absent asks for
/// the host shelf.
pub fn shelf_list_frame(req: &str, id: Option<&str>) -> String {
    serde_json::json!({"t": "shelf_list", "req": req, "id": id}).to_string()
}

/// `shelf_get{req,file_id}`: ask for one file's bytes as `file` parts.
pub fn shelf_get_frame(req: &str, file_id: &str) -> String {
    serde_json::json!({"t": "shelf_get", "req": req, "file_id": file_id}).to_string()
}

/// `shelf_delete{req,file_id}`: ask the host to remove one shelf file.
pub fn shelf_delete_frame(req: &str, file_id: &str) -> String {
    serde_json::json!({"t": "shelf_delete", "req": req, "file_id": file_id}).to_string()
}
