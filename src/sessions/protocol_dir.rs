//! `dir_list`: one directory's subdirectories, so a remote New dialog can
//! offer a real folder picker instead of a remembered-paths list. The frame
//! is read-only — it creates nothing on disk — and owner-only on the device
//! bridge, ungated on the local socket.

use serde::Serialize;
use serde_json::Value;

use super::protocol::{opt_str_field, req_field, ClientFrame, FrameError};

/// One subdirectory name in a `dir_list` answer.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
}

/// Parse one `dir_list` client frame; the caller already matched `t`. A
/// missing `req` answers `bad_request` naming the frame and the field;
/// `path` is optional (absent or `null` lists the home directory) and must
/// be a string when present.
pub(super) fn parse(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    let req = req_field(value, frame)?;
    let path = opt_str_field(value, frame, "path")?;
    Ok(ClientFrame::DirList { req, path })
}

/// `dir_list{req,path,parent,dirs,truncated}`: the canonical directory that
/// was listed, its parent (`null` at the filesystem root), the subdirectory
/// names sorted case-insensitively and capped at 500, and whether more
/// entries exist beyond the cap.
pub fn dir_list_frame(
    req: &str,
    path: &str,
    parent: Option<&str>,
    dirs: &[DirEntry],
    truncated: bool,
) -> String {
    serde_json::json!({
        "t": "dir_list", "req": req, "path": path, "parent": parent,
        "dirs": dirs, "truncated": truncated,
    })
    .to_string()
}
