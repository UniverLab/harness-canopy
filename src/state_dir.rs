//! Private state directory and file permission enforcement.
//!
//! Ensures `~/.canopy` and its contents are created and maintained without
//! group or world permissions (0700 for directories, 0600/0700 for files),
//! without altering the process umask.

use std::fs::File;
use std::io;
use std::path::Path;

/// Ensure `dir` exists as a private directory (mode 0700 on Unix).
///
/// Creates `dir` and any missing parents with mode 0700. If `dir` already
/// exists as a symlink, leaves it untouched and returns an error. If it exists
/// with any group or other permission bits (`mode & 0o077 != 0`), chmods it
/// to 0700 and logs an INFO line.
#[cfg(unix)]
pub(crate) fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("state dir {} is a symlink", dir.display()),
                ));
            }
            if !meta.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("state dir {} is not a directory", dir.display()),
                ));
            }
            let old_mode = meta.permissions().mode() & 0o7777;
            if old_mode & 0o077 != 0 {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
                tracing::info!("state dir {}: mode {:o} -> 0700", dir.display(), old_mode);
            }
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir),
        Err(e) => Err(e),
    }
}

/// Non-Unix stub: returns `Ok(())`.
#[cfg(not(unix))]
pub(crate) fn ensure_private_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}

/// Open (or create) a log file for appending with mode 0600 on Unix.
#[cfg(unix)]
pub(crate) fn open_private_log(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
}

/// Non-Unix stub: opens or creates `path` for appending.
#[cfg(not(unix))]
pub(crate) fn open_private_log(path: &Path) -> io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
}

/// Recursively strip group and world permission bits (`mode & 0o700`) from all
/// non-symlink entries under `root`.
///
/// Symlinks are never followed or modified. Entries that cannot be inspected
/// or chmodded are skipped with a DEBUG log. Returns the number of entries
/// tightened and logs one INFO line when that count is non-zero.
#[cfg(unix)]
pub(crate) fn tighten_tree(root: &Path) -> usize {
    let Ok(root_meta) = std::fs::symlink_metadata(root) else {
        return 0;
    };
    if root_meta.file_type().is_symlink() || !root_meta.is_dir() {
        return 0;
    }

    let mut stack = vec![root.to_path_buf()];
    let mut tightened = 0usize;

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::debug!("failed to read dir {}: {e}", dir.display());
                continue;
            }
        };
        for entry in entries.flatten() {
            tightened += tighten_entry(&entry.path(), &mut stack);
        }
    }

    if tightened > 0 {
        let noun = if tightened == 1 { "entry" } else { "entries" };
        tracing::info!("tightened {tightened} {noun} under {}", root.display());
    }
    tightened
}

#[cfg(unix)]
fn tighten_entry(path: &Path, stack: &mut Vec<std::path::PathBuf>) -> usize {
    use std::os::unix::fs::PermissionsExt;

    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) => {
            tracing::debug!("failed to stat {}: {e}", path.display());
            return 0;
        }
    };
    if meta.file_type().is_symlink() {
        return 0;
    }

    let mode = meta.permissions().mode();
    let mut count = 0;
    if mode & 0o077 != 0 {
        let new_mode = mode & 0o700;
        match std::fs::set_permissions(path, std::fs::Permissions::from_mode(new_mode)) {
            Ok(()) => count = 1,
            Err(e) => tracing::debug!("failed to chmod {}: {e}", path.display()),
        }
    }
    if meta.is_dir() {
        stack.push(path.to_path_buf());
    }
    count
}

/// Non-Unix stub: returns `0`.
#[cfg(not(unix))]
pub(crate) fn tighten_tree(_root: &Path) -> usize {
    0
}
