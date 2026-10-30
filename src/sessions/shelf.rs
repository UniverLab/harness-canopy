//! The session shelf: files an agent sends to its session (`session_send_file`)
//! with the bytes under `~/.canopy/shelf/<file_id>` and metadata in
//! `shelf_files` (see `crate::db::shelf`).
//!
//! Invariants:
//! - the shelf directory is 0700 and its files 0600;
//! - a row and its file appear together: bytes are written to a temp name in
//!   the same directory and renamed only after the row insert, and
//!   [`ShelfStore::maintenance`] sweeps orphans of both kinds at daemon start
//!   and after every insert, so a crash never leaves one without the other;
//! - a session's files outlive it (7 days after removal) and the whole shelf
//!   is capped (1 GiB) — oldest pruned first, `shelf_removed` per victim.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use sha2::{Digest as _, Sha256};
use tokio::sync::broadcast;

use crate::db::shelf::ShelfRow;
use crate::db::Database;

use super::hub::HubEvent;
use super::protocol::ShelfItem;

/// Largest file the shelf accepts (20 MiB), checked on the resolved path.
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
/// Total shelf size cap (1 GiB); oldest files are pruned past it.
pub const CAP_BYTES: u64 = 1024 * 1024 * 1024;
/// Raw size of one `file` frame payload (12 KiB), b64'd on the wire.
pub const READ_CHUNK_BYTES: usize = 12 * 1024;
/// How long a removed session's files stay on the shelf.
pub const SESSION_RETENTION_DAYS: i64 = 7;
/// Caption limit (characters), enforced on the row CHECK and the tool input.
pub const MAX_CAPTION_CHARS: usize = 500;
/// `session_send_file` accepts 1..=10 paths per call.
pub const MAX_PATHS_PER_CALL: usize = 10;

/// Per-path failure codes for `session_send_file` (FR2).
pub const ERR_NOT_FOUND: &str = "not_found";
pub const ERR_NOT_A_FILE: &str = "not_a_file";
pub const ERR_TOO_LARGE: &str = "too_large";
pub const ERR_UNSUPPORTED_TYPE: &str = "unsupported_type";
pub const ERR_IO: &str = "io";

/// One rejected path: the code from the `ERR_*` set plus a human message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShelfError {
    pub code: &'static str,
    pub message: String,
}

impl ShelfError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// The v1 MIME allowlist, sniffed from content — never the extension.
pub const ALLOWED_MIMES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/svg+xml",
];

/// Sniff the MIME type from the file's content: magic bytes for the four
/// raster formats, an `svg` root element for SVG (so an HTML file named
/// `.svg` is rejected). `head` is the first [`SNIFF_HEAD_BYTES`] of the
/// file; `full` is the whole content the SVG root check may scan.
pub fn sniff_mime(head: &[u8], full: &[u8]) -> Option<&'static str> {
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("image/png");
    }
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("image/jpeg");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    svg_root_element(full).then_some("image/svg+xml")
}

/// Bytes of the head examined for magic (`sniff_mime`'s SVG root check may
/// scan `full`, so the head only has to cover the raster magics).
const SNIFF_HEAD_BYTES: usize = 512;

/// Whether the document's root element is `<svg …>`: skips a BOM, leading
/// whitespace, an `<?xml …?>` prolog, `<!-- … -->` comments and a doctype,
/// then requires `<svg` followed by whitespace, `>` or `/>`. Any other root
/// (`<html>`, plain text, no tag at all) is not an SVG.
fn svg_root_element(doc: &[u8]) -> bool {
    let mut rest = strip_bom(doc);
    loop {
        rest = skip_whitespace(rest);
        if let Some(after) = rest.strip_prefix(b"<?") {
            rest = terminate(after, b"?>");
        } else if let Some(after) = rest.strip_prefix(b"<!--") {
            rest = terminate(after, b"-->");
        } else if let Some(after) = strip_doctype(rest) {
            rest = after;
        } else {
            break;
        }
    }
    rest.strip_prefix(b"<svg")
        .is_some_and(|after| after.first().is_none_or(|b| b" \t\r\n>/".contains(b)))
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

fn skip_whitespace(mut bytes: &[u8]) -> &[u8] {
    while let [first, tail @ ..] = bytes {
        if !first.is_ascii_whitespace() {
            break;
        }
        bytes = tail;
    }
    bytes
}

/// Skip up to (and including) `end`, or fail: a prolog/comment that never
/// ends means this is not an SVG root.
fn terminate<'a>(after_prefix: &'a [u8], end: &[u8]) -> &'a [u8] {
    match after_prefix.windows(end.len()).position(|w| w == end) {
        Some(pos) => &after_prefix[pos + end.len()..],
        None => &[],
    }
}

fn strip_doctype(bytes: &[u8]) -> Option<&[u8]> {
    let after = bytes.strip_prefix(b"<!DOCTYPE")?;
    Some(match after.iter().position(|b| *b == b'>') {
        Some(pos) => &after[pos + 1..],
        None => &[],
    })
}

/// A fresh 16-lowercase-hex file id.
pub fn new_file_id() -> String {
    format!("{:016x}", rand::random::<u64>())
}

/// Whether `file_id` is a plausible shelf id (16 lowercase hex) — the only
/// gate before a path is built from it, so nothing ever escapes the shelf
/// directory.
pub fn is_file_id(file_id: &str) -> bool {
    file_id.len() == 16
        && file_id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The shelf directory inside canopy's data dir (`~/.canopy/shelf`).
pub fn default_shelf_dir() -> PathBuf {
    crate::ensure_data_dir()
        .unwrap_or_else(|_| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".canopy")
        })
        .join("shelf")
}

/// The absolute path of a shelf file, for the TUI/CLI to open or copy.
/// `None` unless `file_id` looks like a shelf id, so a crafted id can never
/// point outside `~/.canopy/shelf/`.
pub fn shelf_file_path(file_id: &str) -> Option<PathBuf> {
    is_file_id(file_id).then(|| default_shelf_dir().join(file_id))
}

/// The shelf's bytes + rows owner. Clone shares it; mutations are serialised
/// through one gate so the orphan sweep never meets an in-flight temp file.
pub struct ShelfStore {
    db: Arc<Database>,
    dir: PathBuf,
    cap_bytes: u64,
    gate: Mutex<()>,
    bus: Mutex<Option<broadcast::Sender<HubEvent>>>,
}

impl ShelfStore {
    /// Open (and create, mode 0700) the shelf at `dir`.
    pub fn new(db: Arc<Database>, dir: PathBuf) -> anyhow::Result<Arc<Self>> {
        create_shelf_dir(&dir)?;
        Ok(Arc::new(Self {
            db,
            dir,
            cap_bytes: CAP_BYTES,
            gate: Mutex::new(()),
            bus: Mutex::new(None),
        }))
    }

    /// A store for tests: fresh temp directory under the system temp dir.
    #[cfg(test)]
    pub fn for_test(db: &Arc<Database>) -> Arc<Self> {
        let dir = std::env::temp_dir().join(format!("canopy-shelf-{}", new_file_id()));
        Self::new(Arc::clone(db), dir).expect("test shelf directory")
    }

    /// A store with a smaller size cap, for pruning tests.
    #[cfg(test)]
    pub fn with_cap_bytes(self: Arc<Self>, cap_bytes: u64) -> Arc<Self> {
        let mut store = Arc::try_unwrap(self)
            .unwrap_or_else(|_| panic!("with_cap_bytes needs a unique shelf store"));
        store.cap_bytes = cap_bytes;
        Arc::new(store)
    }

    /// Route shelf events onto the hub's broadcast bus so `/sessions/ws`
    /// clients see `shelf_added` / `shelf_removed`. Daemon wiring calls this
    /// once, after the hub exists.
    pub fn attach_bus(&self, sender: broadcast::Sender<HubEvent>) {
        *self.bus.lock().unwrap_or_else(|e| e.into_inner()) = Some(sender);
    }

    /// The shelf directory. Production code reaches the bytes through
    /// [`Self::read_file`] / [`Self::path_for`]; only tests poke the
    /// directory itself (orphan and temp-file assertions).
    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Validate a `file_id` and resolve it to its path inside the shelf dir.
    pub fn path_for(&self, file_id: &str) -> Option<PathBuf> {
        is_file_id(file_id).then(|| self.dir.join(file_id))
    }

    /// One shelf's items, newest first (`None` = host shelf).
    pub fn list(&self, session_id: Option<&str>) -> Vec<ShelfItem> {
        self.db
            .list_shelf_files(session_id)
            .unwrap_or_default()
            .iter()
            .map(ShelfItem::from)
            .collect()
    }

    /// One file's row, searched across every shelf.
    pub fn get(&self, file_id: &str) -> Option<ShelfRow> {
        self.db.get_shelf_file(file_id).ok().flatten()
    }

    /// A file's bytes, only ever from inside the shelf dir. `None` for an
    /// unknown id, a malformed id, or a row whose file is gone.
    pub fn read_file(&self, file_id: &str) -> Option<Vec<u8>> {
        self.get(file_id)?;
        let path = self.path_for(file_id)?;
        std::fs::read(path).ok()
    }

    /// Delete a file: row first, then bytes. Emits `shelf_removed` when
    /// anything was deleted. `false` when there was no such file.
    pub fn delete(&self, file_id: &str) -> bool {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        let removed_row = self.db.delete_shelf_file(file_id).unwrap_or(false);
        let removed_file = self
            .path_for(file_id)
            .is_some_and(|path| std::fs::remove_file(path).is_ok());
        if removed_row {
            self.emit(HubEvent::ShelfRemoved {
                file_id: file_id.to_string(),
            });
        }
        removed_row || removed_file
    }

    /// Copy one file onto the shelf. `session_id: None` lands it on the host
    /// shelf; `from_label` is the sending session's name, or `host`.
    pub fn insert(
        &self,
        src: &Path,
        session_id: Option<&str>,
        caption: Option<&str>,
        from_label: &str,
    ) -> Result<ShelfItem, ShelfError> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());

        let resolved = std::fs::canonicalize(src).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ShelfError::new(ERR_NOT_FOUND, format!("no such file: {}", src.display()))
            } else {
                ShelfError::new(ERR_IO, format!("{}: {e}", src.display()))
            }
        })?;
        let metadata = std::fs::metadata(&resolved).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ShelfError::new(ERR_NOT_FOUND, format!("no such file: {}", src.display()))
            } else {
                ShelfError::new(ERR_IO, format!("{}: {e}", src.display()))
            }
        })?;
        if !metadata.is_file() {
            return Err(ShelfError::new(
                ERR_NOT_A_FILE,
                format!("not a regular file: {}", src.display()),
            ));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(ShelfError::new(
                ERR_TOO_LARGE,
                format!(
                    "{} is {} bytes; the shelf accepts at most {MAX_FILE_BYTES}",
                    src.display(),
                    metadata.len()
                ),
            ));
        }

        let bytes = std::fs::read(&resolved)
            .map_err(|e| ShelfError::new(ERR_IO, format!("{}: {e}", src.display())))?;

        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "file".to_string());
        self.store_bytes_locked(&bytes, name, session_id, caption, from_label)
    }

    /// Store in-memory bytes on the shelf: the same size cap, MIME sniff,
    /// row + rename commit and maintenance sweep as [`Self::insert`], for
    /// callers that have bytes but no source path (prompt-builder image
    /// attachments). `name` is the display name stored on the row.
    pub fn insert_bytes(
        &self,
        bytes: &[u8],
        name: String,
        session_id: Option<&str>,
        caption: Option<&str>,
        from_label: &str,
    ) -> Result<ShelfItem, ShelfError> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(ShelfError::new(
                ERR_TOO_LARGE,
                format!(
                    "{name} is {} bytes; the shelf accepts at most {MAX_FILE_BYTES}",
                    bytes.len()
                ),
            ));
        }
        self.store_bytes_locked(bytes, name, session_id, caption, from_label)
    }

    /// The shared tail of every insert: sniff the MIME from the bytes, write
    /// the row plus a temp file, rename it into place, emit `shelf_added`,
    /// and run the lifecycle sweep. The caller holds the gate.
    fn store_bytes_locked(
        &self,
        bytes: &[u8],
        name: String,
        session_id: Option<&str>,
        caption: Option<&str>,
        from_label: &str,
    ) -> Result<ShelfItem, ShelfError> {
        let head_len = bytes.len().min(SNIFF_HEAD_BYTES);
        let mime = sniff_mime(&bytes[..head_len], bytes).ok_or_else(|| {
            ShelfError::new(
                ERR_UNSUPPORTED_TYPE,
                format!(
                    "{name} is not one of {} (sniffed from its content, never the extension)",
                    ALLOWED_MIMES.join(", "),
                ),
            )
        })?;

        let file_id = new_file_id();
        let row = ShelfRow {
            file_id: file_id.clone(),
            session_id: session_id.map(str::to_owned),
            name,
            mime: mime.to_string(),
            size: bytes.len() as i64,
            sha256: hex_hash(bytes),
            caption: caption
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(str::to_owned),
            from_label: from_label.to_string(),
            created_at: Utc::now().to_rfc3339(),
        };

        let final_path = self.dir.join(&file_id);
        let tmp_path = self.dir.join(format!(".{file_id}.tmp"));
        write_shelf_file(&tmp_path, bytes)?;

        if let Err(e) = self.db.insert_shelf_file(&row) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(ShelfError::new(ERR_IO, format!("shelf insert failed: {e}")));
        }
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            let _ = self.db.delete_shelf_file(&file_id);
            return Err(ShelfError::new(ERR_IO, format!("shelf rename failed: {e}")));
        }

        let item = ShelfItem::from(&row);
        self.emit(HubEvent::ShelfAdded(Box::new(item.clone())));
        // Retention, cap and orphan checks run after every insert (FR4).
        for file_id in self.maintenance_locked() {
            self.emit(HubEvent::ShelfRemoved { file_id });
        }
        Ok(item)
    }

    /// Run the lifecycle checks now: 7-day retention for removed sessions,
    /// the size cap (oldest first), and the orphan sweep in both directions.
    /// Returns the removed file ids; each one is emitted as `shelf_removed`.
    /// Called at daemon start, after every insert, and on session `kill`.
    pub fn maintenance(&self) -> Vec<String> {
        let _gate = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.maintenance_locked()
    }

    fn maintenance_locked(&self) -> Vec<String> {
        let mut removed = Vec::new();
        let rows = self.db.all_shelf_files_oldest_first().unwrap_or_default();

        // Retention: a removed session's files live at least 7 days — the
        // clock starts when the session ended, not when the file arrived.
        let existing = self.db.live_session_ids().unwrap_or_default();
        let has_removed = rows.iter().any(|row| {
            row.session_id
                .as_deref()
                .is_some_and(|id| !existing.contains(id))
        });
        // Only a removed session needs its end instant looked up; a shelf
        // with no such row never pays for the session scan.
        let ends = if has_removed {
            self.db.session_end_times().unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let cutoff = Utc::now() - Duration::days(SESSION_RETENTION_DAYS);
        for row in &rows {
            let Some(session_id) = row.session_id.as_deref() else {
                continue;
            };
            if existing.contains(session_id) {
                continue;
            }
            // A session row that never recorded an end (`orphaned`), or one
            // canopy no longer has at all (`canopy clean`), falls back to the
            // file's own age, so the file is kept at least as long.
            let ended = ends.get(session_id).cloned().flatten();
            let anchor = ended.as_deref().unwrap_or(&row.created_at);
            if parse_created(anchor).is_some_and(|at| at <= cutoff) {
                self.drop_row(row, &mut removed);
            }
        }

        // Orphans: rows without files lose the row; files without rows
        // (crash between write and insert, or between delete and commit)
        // lose the bytes.
        let surviving: Vec<ShelfRow> = self
            .db
            .all_shelf_files_oldest_first()
            .unwrap_or_default()
            .into_iter()
            .filter(|row| {
                let has_file = self.path_for(&row.file_id).is_some_and(|p| p.is_file());
                if !has_file {
                    removed.push(row.file_id.clone());
                    if let Err(e) = self.db.delete_shelf_file(&row.file_id) {
                        tracing::warn!("shelf orphan row {} not removed: {e}", row.file_id);
                    }
                }
                has_file
            })
            .collect();

        // Cap: prune oldest first until the shelf fits.
        let mut total: u64 = surviving.iter().map(|r| r.size.max(0) as u64).sum();
        for row in &surviving {
            if total <= self.cap_bytes {
                break;
            }
            self.drop_row(row, &mut removed);
            total = total.saturating_sub(row.size.max(0) as u64);
        }

        self.sweep_stray_files(&surviving);
        removed
    }

    /// Delete one row plus its bytes, recording the id for `shelf_removed`.
    fn drop_row(&self, row: &ShelfRow, removed: &mut Vec<String>) {
        if let Err(e) = self.db.delete_shelf_file(&row.file_id) {
            tracing::warn!("shelf row {} not removed: {e}", row.file_id);
            return;
        }
        if let Some(path) = self.path_for(&row.file_id) {
            let _ = std::fs::remove_file(path);
        }
        removed.push(row.file_id.clone());
    }

    /// Remove shelf-directory entries no row claims (temp files and bytes
    /// without a row). Never touches a claimed file.
    fn sweep_stray_files(&self, rows: &[ShelfRow]) {
        let claimed: std::collections::HashSet<&str> =
            rows.iter().map(|r| r.file_id.as_str()).collect();
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if (name.starts_with('.') || !claimed.contains(name.as_ref()))
                && entry.file_type().map(|t| t.is_file()).unwrap_or(false)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    fn emit(&self, event: HubEvent) {
        let bus = self.bus.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sender) = bus.as_ref() {
            let _ = sender.send(event);
        }
    }
}

/// Create the shelf directory with mode 0700 (files get 0600 on create).
fn create_shelf_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    restrict_shelf_dir(dir)?;
    Ok(())
}

#[cfg(unix)]
fn restrict_shelf_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_shelf_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Write the bytes to `path` with mode 0600 (unix).
fn write_shelf_file(path: &Path, bytes: &[u8]) -> Result<(), ShelfError> {
    let write = || -> std::io::Result<()> {
        #[cfg(unix)]
        let mut file = {
            use std::os::unix::fs::OpenOptionsExt as _;
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(path)?
        };
        #[cfg(not(unix))]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()
    };
    write().map_err(|e| ShelfError::new(ERR_IO, format!("{}: {e}", path.display())))
}

fn hex_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn parse_created(created_at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(created_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

impl From<&ShelfRow> for ShelfItem {
    fn from(row: &ShelfRow) -> Self {
        Self {
            file_id: row.file_id.clone(),
            session_id: row.session_id.clone(),
            name: row.name.clone(),
            mime: row.mime.clone(),
            size: row.size,
            caption: row.caption.clone(),
            from_label: row.from_label.clone(),
            created_at: row.created_at.clone(),
        }
    }
}
