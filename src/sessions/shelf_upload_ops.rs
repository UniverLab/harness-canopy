//! `shelf_put_*` operations on [`super::ShelfStore`]: reserve an upload,
//! append parts, verify the checksum and commit, or abandon it.
//!
//! Split out of `shelf.rs` for the file-size gate; the store's fields and
//! helpers stay private and reachable to this child module.

use chrono::Utc;

use super::{
    append_to_file, hex_hash, is_file_id, new_file_id, write_shelf_file, ShelfError,
    ACK_EVERY_PARTS, ERR_BAD_REQUEST, ERR_CHECKSUM_MISMATCH, ERR_IO, ERR_NOT_FOUND, ERR_TOO_LARGE,
};
use crate::db::shelf::ShelfRow;
use crate::sessions::hub::HubEvent;
use crate::sessions::protocol::ShelfItem;
use crate::sessions::shelf_upload::{sanitize_name, valid_sha256, UploadState};

impl super::ShelfStore {
    /// Start an upload: validate the size against the cap and the name and
    /// checksum shapes, reserve a `file_id`, and open its temp file 0600.
    /// Every refusal happens here, before any file data arrives.
    pub fn begin_upload(
        &self,
        session_id: Option<&str>,
        name: &str,
        size: u64,
        mime: Option<String>,
        sha256: &str,
    ) -> Result<String, ShelfError> {
        if size > self.max_file_bytes() {
            return Err(ShelfError::new(
                ERR_TOO_LARGE,
                format!(
                    "{size} bytes exceeds the {} byte upload limit",
                    self.max_file_bytes()
                ),
            ));
        }
        let name =
            sanitize_name(name).map_err(|message| ShelfError::new(ERR_BAD_REQUEST, message))?;
        if !valid_sha256(sha256) {
            return Err(ShelfError::new(
                ERR_BAD_REQUEST,
                "sha256 must be 64 lowercase hex characters",
            ));
        }
        let mime = mime.filter(|mime| !mime.is_empty());
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let file_id = new_file_id();
        let tmp_path = self.upload_path(&file_id);
        write_shelf_file(&tmp_path, &[])?;
        self.uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                file_id.clone(),
                UploadState {
                    session_id: session_id.map(str::to_owned),
                    name,
                    size,
                    mime,
                    sha256: sha256.to_string(),
                    received: 0,
                    next_part: 0,
                },
            );
        Ok(file_id)
    }

    /// The target shelf of an in-flight upload: `None` for an unknown id,
    /// `Some(None)` for the host shelf.
    pub fn upload_session(&self, file_id: &str) -> Option<Option<String>> {
        self.uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(file_id)
            .map(|state| state.session_id.clone())
    }

    /// Append one part to an in-flight upload. Returns the part number to
    /// acknowledge on every [`ACK_EVERY_PARTS`]th part, else `None`. Parts
    /// must arrive in order (`bad_request` otherwise).
    pub fn upload_part(
        &self,
        file_id: &str,
        part: u64,
        data: &[u8],
    ) -> Result<Option<u64>, ShelfError> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let mut uploads = self.uploads.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = uploads.get_mut(file_id) else {
            return Err(ShelfError::new(
                ERR_NOT_FOUND,
                format!("no upload '{file_id}'"),
            ));
        };
        if part != state.next_part {
            return Err(ShelfError::new(
                ERR_BAD_REQUEST,
                format!("expected part {}, got {part}", state.next_part),
            ));
        }
        let received = state.received.saturating_add(data.len() as u64);
        if received > state.size {
            return Err(ShelfError::new(
                ERR_TOO_LARGE,
                format!("upload exceeds its declared {} bytes", state.size),
            ));
        }
        append_to_file(&self.upload_path(file_id), data)?;
        state.received = received;
        state.next_part = part + 1;
        Ok((part + 1).is_multiple_of(ACK_EVERY_PARTS).then_some(part))
    }

    /// Verify an upload's checksum and commit it: the temp file is renamed
    /// into place, the row (origin `user`) is written, and the existing
    /// `shelf_added` event is emitted as the completion signal. A checksum
    /// or size mismatch deletes the temp and answers `checksum_mismatch`.
    pub fn upload_end(&self, file_id: &str) -> Result<ShelfItem, ShelfError> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let state = {
            let mut uploads = self.uploads.lock().unwrap_or_else(|e| e.into_inner());
            match uploads.remove(file_id) {
                Some(state) => state,
                None => {
                    return Err(ShelfError::new(
                        ERR_NOT_FOUND,
                        format!("no upload '{file_id}'"),
                    ))
                }
            }
        };
        let tmp_path = self.upload_path(file_id);
        let bytes = std::fs::read(&tmp_path)
            .map_err(|e| ShelfError::new(ERR_IO, format!("{}: {e}", tmp_path.display())))?;
        if bytes.len() as u64 != state.size || hex_hash(&bytes) != state.sha256 {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(ShelfError::new(
                ERR_CHECKSUM_MISMATCH,
                format!("{} does not match its declared sha256", state.name),
            ));
        }
        let mime = self
            .classify_mime(&bytes, state.mime.as_deref())
            .unwrap_or_else(|| "application/octet-stream".to_string());
        let row = ShelfRow {
            file_id: file_id.to_string(),
            session_id: state.session_id.clone(),
            name: state.name.clone(),
            mime,
            size: bytes.len() as i64,
            sha256: state.sha256,
            caption: None,
            from_label: "user".to_string(),
            origin: "user".to_string(),
            created_at: Utc::now().to_rfc3339(),
        };
        if let Err(e) = self.db.insert_shelf_file(&row) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(ShelfError::new(ERR_IO, format!("shelf insert failed: {e}")));
        }
        let final_path = self.dir.join(file_id);
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            let _ = self.db.delete_shelf_file(file_id);
            return Err(ShelfError::new(ERR_IO, format!("shelf rename failed: {e}")));
        }
        let item = ShelfItem::from(&row);
        self.emit(HubEvent::ShelfAdded(Box::new(item.clone())));
        for removed in self.maintenance_locked() {
            self.emit(HubEvent::ShelfRemoved { file_id: removed });
        }
        Ok(item)
    }

    /// Abandon an upload: delete its temp file and forget its state. Silent.
    /// The id comes straight from the client, so it is validated before any
    /// path is built from it — otherwise `../x` would delete outside the
    /// shelf (part/end are safe without this: they only reach the path after
    /// an in-map lookup of a server-issued id).
    pub fn abort_upload(&self, file_id: &str) {
        if !is_file_id(file_id) {
            return;
        }
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.uploads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(file_id);
        let _ = std::fs::remove_file(self.upload_path(file_id));
    }
}
