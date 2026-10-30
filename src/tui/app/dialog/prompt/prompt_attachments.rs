//! Prompt-builder image attachments (split from `dialog::prompt`).
//!
//! Attached images live as bytes on the target session's shelf — the same
//! store behind `session_send_file`, so they also show in the hub Shelf tab
//! and on remote. The draft only keeps the absolute shelf path, which the
//! composed prompt carries as `<image path="…"/>` (see
//! `domain::prompt_draft`).

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::db::Database;
use crate::sessions::shelf::ShelfStore;

/// Maximum images per builder draft, mirroring the shelf's per-call cap.
pub const MAX_BUILDER_ATTACHMENTS: usize = 10;
/// `from` label stored on shelf rows written by the builder.
pub const BUILDER_FROM_LABEL: &str = "prompt-builder";

/// One attached image: its absolute shelf path plus the byte size shown on
/// the builder chip. Serialisable so the draft's attachments survive a
/// close/reopen through [`super::PersistedBuilderState`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderAttachment {
    pub path: String,
    pub size_bytes: u64,
}

impl BuilderAttachment {
    /// A chip without shelf backing, for tests that never attach.
    #[cfg(test)]
    pub fn for_test(path: &str, size_bytes: u64) -> Self {
        Self {
            path: path.to_string(),
            size_bytes,
        }
    }
}

/// The shelf a builder session key files under: an `interactive:`/`terminal:`
/// session id lands on that session's shelf, every other key on the host
/// shelf (`None`).
pub fn shelf_session_for_key(session_key: &str) -> Option<&str> {
    session_key
        .strip_prefix("interactive:")
        .or_else(|| session_key.strip_prefix("terminal:"))
        .filter(|id| !id.is_empty())
}

/// Store `bytes` on the shelf (named `prompt-<millis>-<n>.<ext>`) and push
/// the attachment. Past the count cap, or when the shelf rejects the bytes,
/// returns the inline error and attaches nothing.
pub fn attach_image_bytes(
    db: &Arc<Database>,
    shelf_dir: &Path,
    session_key: &str,
    attachments: &mut Vec<BuilderAttachment>,
    bytes: &[u8],
    ext: &str,
) -> Result<(), String> {
    if attachments.len() >= MAX_BUILDER_ATTACHMENTS {
        return Err(format!(
            "at most {MAX_BUILDER_ATTACHMENTS} images per prompt"
        ));
    }
    let name = attachment_name(attachments.len(), ext);
    let store = ShelfStore::new(Arc::clone(db), shelf_dir.to_path_buf())
        .map_err(|e| format!("shelf unavailable: {e}"))?;
    let item = store
        .insert_bytes(
            bytes,
            name,
            shelf_session_for_key(session_key),
            None,
            BUILDER_FROM_LABEL,
        )
        .map_err(|e| e.message)?;
    attachments.push(BuilderAttachment {
        path: shelf_dir.join(&item.file_id).to_string_lossy().into_owned(),
        size_bytes: u64::try_from(item.size).unwrap_or(0),
    });
    Ok(())
}

/// Remove the attachment at `index`. The shelf row and bytes stay: removal
/// only detaches the image from this draft. `false` for an out-of-range
/// index.
pub fn remove_attachment(attachments: &mut Vec<BuilderAttachment>, index: usize) -> bool {
    if index < attachments.len() {
        attachments.remove(index);
        true
    } else {
        false
    }
}

/// `prompt-<millis>-<n>.<ext>`: the millis stamp orders same-session
/// attachments, the 1-based slot keeps names unique within a burst.
fn attachment_name(index: usize, ext: &str) -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    format!("prompt-{millis}-{}.{}", index + 1, ext.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sessions::shelf::MAX_FILE_BYTES;
    use crate::tui::clipboard::ImageClipboard;

    /// A file that sniffs as PNG: the 8-byte magic plus arbitrary tail (the
    /// sniffer only reads the head).
    fn png_bytes(len: usize) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.resize(len.max(8), 0x42);
        bytes
    }

    fn test_db() -> (Arc<Database>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Database::new(&dir.path().join("test.db")).expect("test database");
        (Arc::new(db), dir)
    }

    /// A clipboard that always holds `bytes`, so attach tests never touch
    /// the real clipboard.
    struct StubClipboard {
        bytes: Vec<u8>,
    }

    impl ImageClipboard for StubClipboard {
        fn read_image(&self) -> Option<Vec<u8>> {
            Some(self.bytes.clone())
        }
    }

    #[test]
    fn stub_clipboard_bytes_become_a_shelf_backed_chip() {
        let (db, _dir) = test_db();
        let shelf_dir = tempfile::tempdir().expect("shelf dir");
        let clipboard = StubClipboard {
            bytes: png_bytes(120),
        };

        let mut attachments = Vec::new();
        let bytes = clipboard.read_image().expect("stub holds an image");
        attach_image_bytes(
            &db,
            shelf_dir.path(),
            "interactive:sess-1",
            &mut attachments,
            &bytes,
            "png",
        )
        .expect("attaches");

        assert_eq!(attachments.len(), 1);
        let chip = &attachments[0];
        assert_eq!(chip.size_bytes, 120);
        // The chip path is a real shelf file: absolute, under the shelf dir.
        let path = Path::new(&chip.path);
        assert!(path.is_absolute());
        assert!(path.starts_with(shelf_dir.path()));
        assert_eq!(std::fs::read(path).expect("shelf bytes"), bytes);
        // …filed under the target session, from the builder.
        let store =
            ShelfStore::new(Arc::clone(&db), shelf_dir.path().to_path_buf()).expect("reopen");
        let file_id = path.file_name().expect("file id").to_string_lossy();
        let row = store.get(&file_id).expect("shelf row");
        assert_eq!(row.session_id.as_deref(), Some("sess-1"));
        assert_eq!(row.from_label, BUILDER_FROM_LABEL);
        assert!(row.name.starts_with("prompt-") && row.name.ends_with(".png"));
    }

    #[test]
    fn eleventh_attachment_is_rejected_and_attaches_nothing() {
        let (db, _dir) = test_db();
        let shelf_dir = tempfile::tempdir().expect("shelf dir");
        let mut attachments = Vec::new();
        for _ in 0..MAX_BUILDER_ATTACHMENTS {
            attach_image_bytes(
                &db,
                shelf_dir.path(),
                "interactive:sess-1",
                &mut attachments,
                &png_bytes(64),
                "png",
            )
            .expect("attaches under the cap");
        }

        let err = attach_image_bytes(
            &db,
            shelf_dir.path(),
            "interactive:sess-1",
            &mut attachments,
            &png_bytes(64),
            "png",
        )
        .unwrap_err();

        assert!(err.contains("at most 10 images"), "inline error: {err}");
        assert_eq!(attachments.len(), MAX_BUILDER_ATTACHMENTS);
    }

    #[test]
    fn oversize_bytes_are_rejected_with_the_shelf_limit() {
        let (db, _dir) = test_db();
        let shelf_dir = tempfile::tempdir().expect("shelf dir");
        let mut attachments = Vec::new();

        let err = attach_image_bytes(
            &db,
            shelf_dir.path(),
            "interactive:sess-1",
            &mut attachments,
            &png_bytes(MAX_FILE_BYTES as usize + 1),
            "png",
        )
        .unwrap_err();

        assert!(
            err.contains(&MAX_FILE_BYTES.to_string()),
            "names the cap: {err}"
        );
        assert!(attachments.is_empty());
    }

    #[test]
    fn remove_attachment_keeps_the_shelf_row() {
        let (db, _dir) = test_db();
        let shelf_dir = tempfile::tempdir().expect("shelf dir");
        let mut attachments = Vec::new();
        attach_image_bytes(
            &db,
            shelf_dir.path(),
            "interactive:sess-1",
            &mut attachments,
            &png_bytes(64),
            "png",
        )
        .expect("attaches");
        let path = attachments[0].path.clone();

        assert!(remove_attachment(&mut attachments, 0));
        assert!(!remove_attachment(&mut attachments, 0));
        assert!(attachments.is_empty());
        // The bytes stay on the shelf: removal only detaches from the draft.
        assert!(Path::new(&path).is_file());
    }

    #[test]
    fn shelf_session_for_key_routes_sessions_and_hosts_the_rest() {
        assert_eq!(shelf_session_for_key("interactive:abc"), Some("abc"));
        assert_eq!(shelf_session_for_key("terminal:xyz"), Some("xyz"));
        assert_eq!(shelf_session_for_key("workdir:/tmp/x"), None);
        assert_eq!(shelf_session_for_key("project:/tmp/x"), None);
        assert_eq!(shelf_session_for_key("interactive:"), None);
    }
}
