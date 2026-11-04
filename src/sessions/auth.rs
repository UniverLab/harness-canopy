//! Daemon token auth for the sessions WebSocket endpoint.
//!
//! The token is 32 random bytes as hex in `~/.canopy/daemon.token` (file
//! mode 0600), created at daemon start when missing. Its value never appears
//! in logs or errors.

use std::path::{Path, PathBuf};

/// Token file name inside the data dir.
pub const TOKEN_FILE_NAME: &str = "daemon.token";

const TOKEN_BYTES: usize = 32;

/// Path of the daemon token file.
pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TOKEN_FILE_NAME)
}

/// Read the daemon token, creating it when missing or blank. Never logs it.
pub fn ensure_token(data_dir: &Path) -> anyhow::Result<String> {
    let path = token_path(data_dir);
    if let Ok(existing) = std::fs::read_to_string(&path) {
        let token = existing.trim().to_string();
        if !token.is_empty() {
            restrict_token_file(&path);
            return Ok(token);
        }
    }
    let bytes: [u8; TOKEN_BYTES] = rand::random();
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    write_token_file(&path, &token)?;
    Ok(token)
}

#[cfg(unix)]
fn write_token_file(path: &Path, token: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?
        .write_all(token.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_token_file(path: &Path, token: &str) -> anyhow::Result<()> {
    std::fs::write(path, token)?;
    Ok(())
}

/// Best-effort: an existing token file keeps mode 0600.
#[cfg(unix)]
fn restrict_token_file(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.permissions().mode() & 0o777 != 0o600 {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        let _ = std::fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
fn restrict_token_file(_path: &Path) {}

/// Extract the bearer token from an `Authorization` header value.
pub fn bearer_token(header: &str) -> Option<&str> {
    header.strip_prefix("Bearer ").filter(|t| !t.is_empty())
}

/// Constant-time token comparison. Callers must never log either side.
pub fn token_valid(provided: &str, expected: &str) -> bool {
    let a = provided.as_bytes();
    let b = expected.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |diff, (x, y)| diff | (x ^ y))
        == 0
}
