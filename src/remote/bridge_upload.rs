//! The remote bridge's upload arms: `shelf_put_begin` / `part` / `end` /
//! `abort` (protocol v2).
//!
//! Uploads are permitted only with the `upload` grant and only into sessions
//! that grant covers. Refusals happen at `begin`, before any file data; a
//! part or end whose grant vanished mid-flight answers `forbidden` and
//! deletes the temp file. A successful `end` has no answer frame: the shelf's
//! existing `shelf_added` broadcast is the completion signal, and a live
//! target session gets one automatic attachment line.

use crate::sessions::protocol::{
    error_frame, shelf_put_ack_frame, shelf_put_ready_frame, ClientFrame, ERR_BAD_REQUEST,
    ERR_BUSY, ERR_CHECKSUM_MISMATCH, ERR_FORBIDDEN, ERR_INTERNAL, ERR_NOT_FOUND, ERR_TOO_LARGE,
};
use crate::sessions::shelf::ShelfError;
use crate::sessions::shelf_upload::attachment_line;
use crate::sessions::ws::OutTx;

use super::RemoteBridge;

impl RemoteBridge {
    /// `shelf_put_begin{req,id?,name,size,mime?,sha256}`: validate the grant,
    /// reserve a slot and a `file_id`, and answer `shelf_put_ready`.
    pub(crate) fn handle_shelf_put_begin(&mut self, frame: ClientFrame, out: &OutTx) {
        let ClientFrame::ShelfPutBegin {
            req,
            id,
            name,
            size,
            mime,
            sha256,
        } = frame
        else {
            return;
        };
        // An empty id is the host shelf, like `session_send_file`'s empty
        // filter — otherwise the file would land in an unlistable `""` shelf.
        let id = id.filter(|id| !id.trim().is_empty());
        if !self.scope.can_upload() {
            send_error(out, Some(&req), ERR_FORBIDDEN, "upload is not granted");
            return;
        }
        if id.is_none() && !self.scope.is_all() {
            send_error(
                out,
                Some(&req),
                ERR_FORBIDDEN,
                "uploads go only into a granted session",
            );
            return;
        }
        if let Some(session_id) = id.as_deref() {
            if !self.scope.allows_id(&self.state.hub, session_id) {
                send_error(
                    out,
                    Some(&req),
                    ERR_FORBIDDEN,
                    "upload is not granted for this session",
                );
                return;
            }
        }
        let Ok(permit) = self.uploads.clone().try_acquire_owned() else {
            send_error(out, Some(&req), ERR_BUSY, "too many uploads in flight");
            return;
        };
        match self
            .state
            .shelf
            .begin_upload(id.as_deref(), &name, size, mime, &sha256)
        {
            Ok(file_id) => {
                self.upload_permits.insert(file_id.clone(), permit);
                let _ = out.send(shelf_put_ready_frame(&req, &file_id));
            }
            Err(error) => send_shelf_error(out, Some(&req), &error),
        }
    }

    /// `shelf_put_part{file_id,part,data}`: append one chunk and acknowledge
    /// every eighth part.
    pub(crate) fn handle_shelf_put_part(&mut self, frame: ClientFrame, out: &OutTx) {
        let ClientFrame::ShelfPutPart {
            file_id,
            part,
            data,
        } = frame
        else {
            return;
        };
        let Some(session_id) = self.state.shelf.upload_session(&file_id) else {
            send_error(out, None, ERR_NOT_FOUND, &format!("no upload '{file_id}'"));
            return;
        };
        if self.scope_lost(session_id.as_deref()) {
            self.drop_upload(&file_id);
            send_error(out, None, ERR_FORBIDDEN, "upload grant vanished");
            return;
        }
        match self.state.shelf.upload_part(&file_id, part, &data) {
            Ok(Some(ack_part)) => {
                let _ = out.send(shelf_put_ack_frame(&file_id, ack_part));
            }
            Ok(None) => {}
            Err(error) => send_shelf_error(out, None, &error),
        }
    }

    /// `shelf_put_end{file_id}`: verify the checksum, commit the file, and
    /// append the automatic session line when the target session is live.
    pub(crate) fn handle_shelf_put_end(&mut self, file_id: &str, out: &OutTx) {
        let Some(session_id) = self.state.shelf.upload_session(file_id) else {
            send_error(out, None, ERR_NOT_FOUND, &format!("no upload '{file_id}'"));
            return;
        };
        if self.scope_lost(session_id.as_deref()) {
            self.drop_upload(file_id);
            send_error(out, None, ERR_FORBIDDEN, "upload grant vanished");
            return;
        }
        match self.state.shelf.upload_end(file_id) {
            Ok(item) => {
                self.upload_permits.remove(file_id);
                self.append_attachment_line(session_id.as_deref(), &item);
            }
            Err(error) => {
                self.upload_permits.remove(file_id);
                send_shelf_error(out, None, &error);
            }
        }
    }

    /// `shelf_put_abort{file_id}`: silent best-effort cleanup.
    pub(crate) fn handle_shelf_put_abort(&mut self, file_id: &str) {
        self.drop_upload(file_id);
    }

    /// Whether the upload's grant is gone, mirroring begin's rules: no
    /// upload permission, a host-shelf upload without a full grant, or a
    /// session target outside the current scope (including a removed one).
    fn scope_lost(&self, session_id: Option<&str>) -> bool {
        if !self.scope.can_upload() {
            return true;
        }
        match session_id {
            None => !self.scope.is_all(),
            Some(id) => !self.scope.allows_id(&self.state.hub, id),
        }
    }

    /// Drop one in-flight upload: delete its temp, release its slot.
    fn drop_upload(&mut self, file_id: &str) {
        self.state.shelf.abort_upload(file_id);
        self.upload_permits.remove(file_id);
    }

    /// Append the pinned `📎 the user attached …` line to the target
    /// session when it is live; a host-shelf upload writes no line.
    fn append_attachment_line(
        &self,
        session_id: Option<&str>,
        item: &crate::sessions::protocol::ShelfItem,
    ) {
        let Some(session_id) = session_id else {
            return;
        };
        let Some(session) = self.state.hub.get(session_id) else {
            return;
        };
        let Some(path) = self.state.shelf.path_for(&item.file_id) else {
            return;
        };
        let line = attachment_line(&item.name, item.size.max(0) as u64, &path);
        // The parser renders raw bytes, so the line carries its own
        // terminator — without it the notice glues to the next output row.
        session.ingest_chunk(format!("{line}\r\n").as_bytes());
    }
}

fn send_error(out: &OutTx, req: Option<&str>, code: &str, message: &str) {
    let _ = out.send(error_frame(req, code, message));
}

/// Translate one store refusal into the protocol's error frame. Codes the
/// wire contract names travel verbatim; anything else is an internal error
/// so an IO detail never leaks as a made-up code.
fn send_shelf_error(out: &OutTx, req: Option<&str>, error: &ShelfError) {
    let wire_code = matches!(
        error.code,
        ERR_TOO_LARGE | ERR_NOT_FOUND | ERR_FORBIDDEN | ERR_BAD_REQUEST | ERR_CHECKSUM_MISMATCH
    );
    if wire_code {
        let _ = out.send(error_frame(req, error.code, &error.message));
    } else {
        let _ = out.send(error_frame(
            req,
            ERR_INTERNAL,
            &format!("upload failed: {}", error.message),
        ));
    }
}
