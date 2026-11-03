//! Upload bookkeeping for the session shelf: the in-flight state of one
//! `shelf_put_*` transfer plus the name, checksum and session-line rules the
//! protocol pins.
//!
//! The bytes themselves live in a temp file next to the shelf
//! (`.<file_id>.upload`) until [`super::shelf::ShelfStore::upload_end`]
//! verifies them and renames the file into place; this module holds only the
//! metadata that follows the transfer in memory.

/// Suffix of an in-flight upload's temp file (the file id precedes it).
pub const TEMP_SUFFIX: &str = ".upload";

/// Longest accepted display name, in bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// One in-flight upload, keyed by its reserved `file_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadState {
    /// Target shelf (`None` = the host shelf).
    pub session_id: Option<String>,
    /// Sanitized display name stored on the row.
    pub name: String,
    /// Declared total size in bytes.
    pub size: u64,
    /// MIME the client declared, when it did; sniffing still wins.
    pub mime: Option<String>,
    /// Lowercase hex SHA-256 over the complete file bytes.
    pub sha256: String,
    /// Bytes appended so far.
    pub received: u64,
    /// Part number the next `shelf_put_part` must carry.
    pub next_part: u64,
}

/// Sanitize a client-supplied display name: strip every directory component,
/// reject an empty or dot name, reject control characters, and cap the length
/// at [`MAX_NAME_BYTES`] bytes.
///
/// The on-disk name is always the `file_id`, so this is display metadata only
/// — a traversal-shaped name (`../../etc/passwd`) lands as `passwd` and can
/// never move the file outside the shelf directory.
pub fn sanitize_name(raw: &str) -> Result<String, String> {
    if raw.chars().any(|c| c.is_control()) {
        return Err("name contains control characters".to_string());
    }
    let name = std::path::Path::new(raw)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.is_empty() || name == "." || name == ".." {
        return Err("name is empty".to_string());
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(format!("name is longer than {MAX_NAME_BYTES} bytes"));
    }
    Ok(name)
}

/// Whether `hash` is a lowercase hex SHA-256 (64 characters).
pub fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The automatic session line one user upload appends:
/// `📎 the user attached \`NAME\` (N.N MB) → SHELF_PATH` (megabytes are
/// decimal, matching the `MB` label). A backtick inside the name would break
/// the delimiters, so it degrades to an apostrophe.
pub fn attachment_line(name: &str, size_bytes: u64, shelf_path: &std::path::Path) -> String {
    let megabytes = size_bytes as f64 / 1_000_000.0;
    let name = name.replace('`', "'");
    format!(
        "📎 the user attached `{name}` ({megabytes:.1} MB) → {}",
        shelf_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_directories_and_rejects_bad_names() {
        assert_eq!(sanitize_name("../../etc/passwd").unwrap(), "passwd");
        assert_eq!(sanitize_name("notes.md").unwrap(), "notes.md");
        assert!(sanitize_name("").is_err());
        assert!(sanitize_name("..").is_err());
        assert!(sanitize_name("bad\u{0007}name").is_err());
        assert!(sanitize_name(&"a".repeat(MAX_NAME_BYTES + 1)).is_err());
        assert!(sanitize_name(&"a".repeat(MAX_NAME_BYTES)).is_ok());
    }

    #[test]
    fn sha256_validation_is_lowercase_hex_only() {
        assert!(valid_sha256(&"a".repeat(64)));
        assert!(valid_sha256(&"0".repeat(64)));
        assert!(!valid_sha256(&"A".repeat(64)));
        assert!(!valid_sha256(&"a".repeat(63)));
        assert!(!valid_sha256(&"g".repeat(64)));
    }

    #[test]
    fn attachment_line_has_the_pinned_shape() {
        let line = attachment_line("notes.md", 2 * 1024 * 1024, std::path::Path::new("/s/x"));
        assert_eq!(line, "📎 the user attached `notes.md` (2.1 MB) → /s/x");
        let line = attachment_line("a`b", 11, std::path::Path::new("/s/x"));
        assert_eq!(line, "📎 the user attached `a'b` (0.0 MB) → /s/x");
    }
}
