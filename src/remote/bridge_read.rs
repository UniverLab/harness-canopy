//! The remote bridge's shelf reads and its schedule frames, each behind the
//! peer's grant. Split from `bridge.rs` for the file-size gate.

use crate::sessions::catalog::handle_schedule_frame;
use crate::sessions::protocol::{
    error_frame, file_frame, shelf_frame, shelf_missing_frame, ClientFrame, ERR_BUSY,
    ERR_FORBIDDEN, ERR_INTERNAL, ERR_NOT_FOUND,
};
use crate::sessions::ws::OutTx;

use super::{send, RemoteBridge, CHUNK_BYTES};

impl RemoteBridge {
    /// `prompt_schedule`, `scheduled_list`, `scheduled_cancel`: a scoped guest
    /// reaches only the sessions in its grant, listing included.
    pub(super) fn handle_schedule(&self, frame: ClientFrame, out: &OutTx) {
        let (ClientFrame::PromptSchedule { req, id, .. }
        | ClientFrame::ScheduledList { req, id }
        | ClientFrame::ScheduledCancel { req, id, .. }) = &frame
        else {
            return;
        };
        if self.scope.granted(&self.state.hub, Some(req), id, out) {
            handle_schedule_frame(&self.state, out, frame);
        }
    }

    pub(super) fn handle_shelf_list(&self, req: String, id: Option<String>, out: &OutTx) {
        if id.as_deref().is_some_and(|id| self.terminal(id)) {
            send(out, shelf_frame(&req, id.as_deref(), &[]));
            return;
        }
        // An ungranted session's shelf, or the host shelf for a peer whose
        // grant is narrower than every session, lists empty.
        if self.scope.hidden_file(&self.state.hub, id.as_deref()) {
            send(out, shelf_frame(&req, id.as_deref(), &[]));
            return;
        }
        let shelf = self.state.shelf.clone();
        let out = out.clone();
        let echo = id.clone();
        // No per-item filter here: `shelf.list(id)` lists exactly one shelf,
        // and that shelf was allowed or emptied above.
        tokio::spawn(async move {
            match tokio::task::spawn_blocking(move || shelf.list(id.as_deref())).await {
                Ok(items) => send(&out, shelf_frame(&req, echo.as_deref(), &items)),
                Err(_) => send(
                    &out,
                    error_frame(Some(&req), ERR_INTERNAL, "shelf list failed"),
                ),
            }
        });
    }

    pub(super) fn handle_shelf_get(&mut self, req: String, file_id: String, out: &OutTx) {
        let Some(row) = self.state.shelf.get(&file_id) else {
            let _ = out.send(shelf_missing_frame(&req, &file_id));
            return;
        };
        if self
            .scope
            .hidden_file(&self.state.hub, row.session_id.as_deref())
        {
            let _ = out.send(shelf_missing_frame(&req, &file_id));
            return;
        }
        if self.hidden_terminal_file(row.session_id.as_deref()) {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_FORBIDDEN,
                "terminal sessions are not shared with remote devices",
            ));
            return;
        }
        let Ok(permit) = self.transfers.clone().try_acquire_owned() else {
            let _ = out.send(error_frame(
                Some(&req),
                ERR_BUSY,
                "too many downloads in flight",
            ));
            return;
        };
        let shelf = self.state.shelf.clone();
        let out = out.clone();
        let task = tokio::spawn(async move {
            let _permit = permit;
            let read = tokio::task::spawn_blocking({
                let file_id = file_id.clone();
                move || shelf.read_file(&file_id)
            })
            .await;
            let bytes = match read {
                Ok(Some(bytes)) => bytes,
                Ok(None) => {
                    send(
                        &out,
                        error_frame(
                            Some(&req),
                            ERR_NOT_FOUND,
                            &format!("no shelf file '{file_id}'"),
                        ),
                    );
                    return;
                }
                Err(_) => {
                    send(
                        &out,
                        error_frame(Some(&req), ERR_INTERNAL, "shelf read failed"),
                    );
                    return;
                }
            };
            if bytes.is_empty() {
                send(&out, file_frame(&req, &file_id, 0, &[], true));
                return;
            }
            let total = bytes.len().div_ceil(CHUNK_BYTES);
            for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
                let last = index + 1 == total;
                if out
                    .send(file_frame(&req, &file_id, index as u32, chunk, last))
                    .is_err()
                {
                    return;
                }
                if !last {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
        });
        self.shelf_tasks.retain(|task| !task.is_finished());
        self.shelf_tasks.push(task);
    }
}
