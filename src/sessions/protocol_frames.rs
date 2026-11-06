//! Frame builders for session protocol v2: one `*_frame` helper per
//! host or client frame, plus the harness inventory the `hello` frame
//! carries. Split from `protocol.rs` for the file-size gate and re-exported
//! there, so every existing `protocol::…` import keeps working.

use std::path::Path;

use serde_json::Value;

use crate::domain::canopy_config::CanopyConfig;

use super::{encode_b64, SessionItem, ShelfItem, SpawnExt, ERR_NOT_FOUND, PROTO_VERSION};

/// Names of the configured CLIs whose binary resolves right now, sorted
/// ascending. Resolution is the hub's own lookup
/// ([`crate::domain::cli_strategy::resolve_binary`]) plus an existence
/// check — absolute paths come back unchecked, so `.exists()` is mandatory.
pub fn installed_harnesses() -> Vec<String> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    harnesses_in(&home.join(".canopy"))
}

/// [`installed_harnesses`] against an explicit canopy dir, for tests.
pub(crate) fn harnesses_in(canopy_dir: &Path) -> Vec<String> {
    let config = CanopyConfig::load(canopy_dir);
    let mut names: Vec<String> = config
        .clis
        .iter()
        .filter(|c| {
            crate::domain::cli_strategy::resolve_binary(&c.binary).is_ok_and(|p| p.exists())
        })
        .map(|c| c.name.clone())
        .collect();
    names.sort();
    names
}

/// Features only the host owner's own devices are offered, on top of the
/// guest list.
pub const OWNER_FEATURES: &[&str] = &["graph_control", "session_resume", "dir_list"];

/// `hello{proto,host_label,features,harnesses}`, sent on connect. `features`
/// advertises the optional extensions this host speaks — `shelf` plus
/// `shelf_upload` for every peer, followed by [`OWNER_FEATURES`] for the host
/// owner's own devices — so a client can tell an older host from a newer one
/// without probing. `harnesses` lists the installed harness names (see
/// [`installed_harnesses`]) so the New dialog knows what to offer.
pub fn hello_frame(host_label: &str, harnesses: &[String], owner: bool) -> String {
    let mut features: Vec<&str> = vec!["shelf", "shelf_upload"];
    if owner {
        features.extend_from_slice(OWNER_FEATURES);
    }
    serde_json::json!({
        "t": "hello", "proto": PROTO_VERSION, "host_label": host_label,
        "features": features, "harnesses": harnesses,
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

/// `resized{id,cols,rows}`: the session's PTY size changed (some client
/// resized it). Sent to every connection attached to the session, only when
/// the size actually changes.
pub fn resized_frame(id: &str, cols: u16, rows: u16) -> String {
    serde_json::json!({"t": "resized", "id": id, "cols": cols, "rows": rows}).to_string()
}

/// `size_released{id}`: the device that sized the session let go (or
/// vanished). Sent to every attached connection, which re-asserts its own
/// size on seeing it.
pub fn size_released_frame(id: &str) -> String {
    serde_json::json!({"t": "size_released", "id": id}).to_string()
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

/// `error{code:"not_found"}` for one shelf file, by request.
pub fn shelf_missing_frame(req: &str, file_id: &str) -> String {
    error_frame(
        Some(req),
        ERR_NOT_FOUND,
        &format!("no shelf file '{file_id}'"),
    )
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

/// `spawn{req,kind,cli?,model?,workdir,name?,prompt?,interactive_args?,fallback_args?,model_flag?,id?,seed_id?,accent?,harness_conversation_id?,yolo?,preserve_args?}`:
/// spawn a hub session. `cli` may be absent or `null` for `kind:"terminal"`
/// (the default shell opens); agents still require it. The agent launch fields mirror what the TUI passes to
/// `InteractiveAgent::spawn` today: explicit args (resume/yolo included),
/// the resume identity, the seed, the accent (`"r,g,b"`) and the harness's
/// own conversation identifier. `yolo` (default
/// `false`) asks the hub to append the harness's registry `yolo_flag` to the
/// interactive args; it is refused for `kind:"terminal"` and for a harness
/// with no such flag. `workdir` may start with `~/` (or be `~` alone) to mean
/// the host user's home, must be absolute after that expansion, and must name
/// an existing directory. `None` values serialize as `null`, which the hub
/// reads as absent and falls back to the registry for.
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
        "harness_conversation_id": ext.harness_conversation_id,
        "yolo": ext.yolo, "preserve_args": ext.preserve_args,
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

/// `resumable_list{req}`: ask for the resumable recorded conversations.
#[cfg(test)]
pub fn resumable_list_frame(req: &str) -> String {
    serde_json::json!({"t": "resumable_list", "req": req}).to_string()
}

/// `session_resume{req,id}`: bring one recorded conversation back under its
/// own id.
#[cfg(test)]
pub fn session_resume_frame(req: &str, id: &str) -> String {
    serde_json::json!({"t": "session_resume", "req": req, "id": id}).to_string()
}
