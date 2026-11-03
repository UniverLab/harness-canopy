//! The local socket's `shelf_put_*` arms (protocol v2). Split from `ws.rs`
//! for the file-size gate; the local socket is the owner, so unlike the
//! remote bridge there is no upload-grant check here.

use std::sync::Arc;

use super::{OutTx, SessionsState};
use crate::sessions::protocol::{
    error_frame, shelf_put_ack_frame, shelf_put_ready_frame, ClientFrame,
};

/// Route one `shelf_put_*` frame to its arm.
pub(super) fn handle(state: &Arc<SessionsState>, out_tx: &OutTx, frame: ClientFrame) {
    match frame {
        frame @ ClientFrame::ShelfPutBegin { .. } => handle_begin(state, out_tx, frame),
        frame @ ClientFrame::ShelfPutPart { .. } => handle_part(state, out_tx, frame),
        ClientFrame::ShelfPutEnd { file_id } => handle_end(state, out_tx, &file_id),
        ClientFrame::ShelfPutAbort { file_id } => state.shelf.abort_upload(&file_id),
        _ => {}
    }
}

fn handle_begin(state: &Arc<SessionsState>, out_tx: &OutTx, frame: ClientFrame) {
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
    match state
        .shelf
        .begin_upload(id.as_deref(), &name, size, mime, &sha256)
    {
        Ok(file_id) => {
            let _ = out_tx.send(shelf_put_ready_frame(&req, &file_id));
        }
        Err(error) => {
            let _ = out_tx.send(error_frame(Some(&req), error.code, &error.message));
        }
    }
}

fn handle_part(state: &Arc<SessionsState>, out_tx: &OutTx, frame: ClientFrame) {
    let ClientFrame::ShelfPutPart {
        file_id,
        part,
        data,
    } = frame
    else {
        return;
    };
    match state.shelf.upload_part(&file_id, part, &data) {
        Ok(Some(ack_part)) => {
            let _ = out_tx.send(shelf_put_ack_frame(&file_id, ack_part));
        }
        Ok(None) => {}
        Err(error) => {
            let _ = out_tx.send(error_frame(None, error.code, &error.message));
        }
    }
}

fn handle_end(state: &Arc<SessionsState>, out_tx: &OutTx, file_id: &str) {
    let session_id = state.shelf.upload_session(file_id).flatten();
    match state.shelf.upload_end(file_id) {
        Ok(item) => {
            let Some(session_id) = session_id else {
                return;
            };
            let (Some(session), Some(path)) = (
                state.hub.get(&session_id),
                state.shelf.path_for(&item.file_id),
            ) else {
                return;
            };
            let line = crate::sessions::shelf_upload::attachment_line(
                &item.name,
                item.size.max(0) as u64,
                &path,
            );
            // The parser renders raw bytes, so the line carries its own
            // terminator — without it the notice glues to the next output row.
            session.ingest_chunk(format!("{line}\r\n").as_bytes());
        }
        Err(error) => {
            let _ = out_tx.send(error_frame(None, error.code, &error.message));
        }
    }
}
