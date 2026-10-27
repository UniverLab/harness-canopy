//! Session protocol v1: JSON text frames over the daemon WebSocket.
//!
//! Every frame carries `t`. Client frames are parsed by
//! [`parse_client_frame`]; host frames are built by the `*_frame` helpers.
//! An unknown `t` or a missing field answers `error{code:"bad_request"}`
//! naming the frame and the field.

use serde::Serialize;
use serde_json::Value;

/// Protocol version the hub speaks.
pub const PROTO_VERSION: u32 = 1;

/// Error codes for `error` frames.
pub const ERR_BAD_REQUEST: &str = "bad_request";
pub const ERR_NOT_FOUND: &str = "not_found";
pub const ERR_FORBIDDEN: &str = "forbidden";
pub const ERR_SPAWN_FAILED: &str = "spawn_failed";
pub const ERR_DELIVERY_FAILED: &str = "delivery_failed";
pub const ERR_INTERNAL: &str = "internal";

/// Session metadata row for `sessions` / `session_added` frames.
#[derive(Debug, Clone, Serialize)]
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
    },
    Kill {
        req: String,
        id: String,
    },
}

/// A rejected client frame: the request id to echo (when the frame carried
/// a usable one) and the `bad_request` message naming frame and field.
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
        "prompt" => {
            let req = req_field(&value, &frame)?;
            let id = id_field(&value, &frame)?;
            let text = str_field(&value, &frame, "text")?;
            Ok(ClientFrame::Prompt { req, id, text })
        }
        "resize" => {
            let id = id_field(&value, &frame)?;
            let cols = size_field(&value, &frame, "cols")?;
            let rows = size_field(&value, &frame, "rows")?;
            Ok(ClientFrame::Resize { id, cols, rows })
        }
        "spawn" => {
            let req = req_field(&value, &frame)?;
            let kind = str_field(&value, &frame, "kind")?;
            let cli = str_field(&value, &frame, "cli")?;
            let model = opt_str_field(&value, &frame, "model")?;
            let workdir = str_field(&value, &frame, "workdir")?;
            let name = opt_str_field(&value, &frame, "name")?;
            let prompt = opt_str_field(&value, &frame, "prompt")?;
            Ok(ClientFrame::Spawn {
                req,
                kind,
                cli,
                model,
                workdir,
                name,
                prompt,
            })
        }
        "kill" => {
            let req = req_field(&value, &frame)?;
            let id = id_field(&value, &frame)?;
            Ok(ClientFrame::Kill { req, id })
        }
        _ => Err(bad_request(None, format!("unknown frame '{frame}'"))),
    }
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

fn encode_b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

/// `hello{proto,host_label}`, sent on connect.
pub fn hello_frame(host_label: &str) -> String {
    serde_json::json!({"t": "hello", "proto": PROTO_VERSION, "host_label": host_label}).to_string()
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
