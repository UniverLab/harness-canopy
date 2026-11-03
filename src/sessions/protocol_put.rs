//! `shelf_put_*` frames (protocol v2): parser arms and the ready/ack plus
//! client builders. Split from `protocol.rs` for the file-size gate.

use serde_json::Value;

#[cfg(test)]
use super::encode_b64;
use super::{
    bad_request, data_field, opt_str_field, req_field, str_field, u64_field, ClientFrame,
    FrameError,
};

/// Parse one `shelf_put_*` client frame; the caller has already matched `t`.
pub(super) fn parse(value: &Value, frame: &str) -> Result<ClientFrame, FrameError> {
    match frame {
        "shelf_put_begin" => {
            let req = req_field(value, frame)?;
            let id = opt_str_field(value, frame, "id")?;
            let name = str_field(value, frame, "name")?;
            let size = u64_field(value, frame, "size")?;
            let mime = opt_str_field(value, frame, "mime")?;
            let sha256 = str_field(value, frame, "sha256")?;
            Ok(ClientFrame::ShelfPutBegin {
                req,
                id,
                name,
                size,
                mime,
                sha256,
            })
        }
        "shelf_put_part" => {
            let file_id = str_field(value, frame, "file_id")?;
            let part = u64_field(value, frame, "part")?;
            let data = data_field(value, frame)?;
            Ok(ClientFrame::ShelfPutPart {
                file_id,
                part,
                data,
            })
        }
        "shelf_put_end" => Ok(ClientFrame::ShelfPutEnd {
            file_id: str_field(value, frame, "file_id")?,
        }),
        "shelf_put_abort" => Ok(ClientFrame::ShelfPutAbort {
            file_id: str_field(value, frame, "file_id")?,
        }),
        other => Err(bad_request(None, format!("unknown frame '{other}'"))),
    }
}

/// `shelf_put_ready{req,file_id}`: the answer to an accepted
/// `shelf_put_begin`.
pub fn shelf_put_ready_frame(req: &str, file_id: &str) -> String {
    serde_json::json!({"t": "shelf_put_ready", "req": req, "file_id": file_id}).to_string()
}

/// `shelf_put_ack{file_id,part}`: acknowledges every eighth received part.
pub fn shelf_put_ack_frame(file_id: &str, part: u64) -> String {
    serde_json::json!({"t": "shelf_put_ack", "file_id": file_id, "part": part}).to_string()
}

/// `shelf_put_begin{req,id?,name,size,mime?,sha256}`: reserve an upload slot
/// on one shelf (`id` absent = the host shelf).
#[cfg(test)]
pub fn shelf_put_begin_frame(
    req: &str,
    id: Option<&str>,
    name: &str,
    size: u64,
    mime: Option<&str>,
    sha256: &str,
) -> String {
    serde_json::json!({
        "t": "shelf_put_begin", "req": req, "id": id, "name": name,
        "size": size, "mime": mime, "sha256": sha256,
    })
    .to_string()
}

/// `shelf_put_part{file_id,part,data}`: one base64 chunk, numbered from 0.
#[cfg(test)]
pub fn shelf_put_part_frame(file_id: &str, part: u64, data: &[u8]) -> String {
    serde_json::json!({
        "t": "shelf_put_part", "file_id": file_id, "part": part, "data": encode_b64(data),
    })
    .to_string()
}

/// `shelf_put_end{file_id}`: ask the host to verify and commit the upload.
#[cfg(test)]
pub fn shelf_put_end_frame(file_id: &str) -> String {
    serde_json::json!({"t": "shelf_put_end", "file_id": file_id}).to_string()
}

/// `shelf_put_abort{file_id}`: abandon an upload and delete its temp file.
#[cfg(test)]
pub fn shelf_put_abort_frame(file_id: &str) -> String {
    serde_json::json!({"t": "shelf_put_abort", "file_id": file_id}).to_string()
}
