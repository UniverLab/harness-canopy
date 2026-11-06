//! `dir_list` handler: list one directory's subdirectories for the remote
//! New dialog's folder picker. The whole scan runs inside
//! `tokio::task::spawn_blocking` so a huge or slow directory never stalls
//! the executor. Read-only: the frame creates nothing on disk.
//!
//! Path rules mirror `spawn`'s `workdir`: absent or empty means the user's
//! home, `~`/`~/rest` expand against it, and any other relative path is
//! `bad_request`. The listed path is canonicalized, so symlinks in the
//! request resolve before the answer reports it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::protocol::{
    dir_list_frame, error_frame, DirEntry, ERR_BAD_REQUEST, ERR_FORBIDDEN, ERR_INTERNAL,
    ERR_NOT_FOUND,
};
use super::spawn::expand_home;
use super::ws::{OutTx, SessionsState};

/// Cap on listed subdirectories; `truncated` reports more exist.
const DIR_LIST_CAP: usize = 500;

/// `dir_list{req,path?}`: resolve `path`, list its subdirectories off the
/// executor, and answer `dir_list` — or the matching `error` frame.
pub(crate) fn handle_dir_list(
    _state: &Arc<SessionsState>,
    out_tx: &OutTx,
    req: String,
    path: Option<String>,
) {
    let out_tx = out_tx.clone();
    tokio::spawn(async move {
        let req_task = req.clone();
        let listed =
            tokio::task::spawn_blocking(move || dir_list_answer(&req_task, path.as_deref())).await;
        match listed {
            Ok(frame) => {
                let _ = out_tx.send(frame);
            }
            Err(_) => {
                let _ = out_tx.send(error_frame(
                    Some(&req),
                    ERR_INTERNAL,
                    "dir_list task failed",
                ));
            }
        }
    });
}

/// The whole answer as one frame string: `dir_list` on success, `error`
/// otherwise. Kept synchronous so the blocking task returns exactly what
/// the connection sends.
fn dir_list_answer(req: &str, path: Option<&str>) -> String {
    match list_directory(path) {
        Ok((canonical, dirs, truncated)) => {
            let path_string = canonical.to_string_lossy().into_owned();
            let parent = canonical
                .parent()
                .map(|parent| parent.to_string_lossy().into_owned());
            dir_list_frame(req, &path_string, parent.as_deref(), &dirs, truncated)
        }
        Err((code, message)) => error_frame(Some(req), code, &message),
    }
}

/// Resolve `path` and collect its subdirectory names. `Err` carries the
/// error code and message for the `error` frame.
fn list_directory(
    path: Option<&str>,
) -> Result<(PathBuf, Vec<DirEntry>, bool), (&'static str, String)> {
    let canonical = resolve_dir(path)?;
    let mut names = subdir_names(&canonical)?;
    names.sort_by_cached_key(|name| name.to_lowercase());
    let truncated = names.len() > DIR_LIST_CAP;
    names.truncate(DIR_LIST_CAP);
    let dirs = names.into_iter().map(|name| DirEntry { name }).collect();
    Ok((canonical, dirs, truncated))
}

/// The requested directory, canonicalized: absent/empty is the home
/// directory, `~`-prefixed paths expand against it, and anything else must
/// already be absolute.
fn resolve_dir(path: Option<&str>) -> Result<PathBuf, (&'static str, String)> {
    let requested = match path {
        None | Some("") => {
            dirs::home_dir().ok_or((ERR_INTERNAL, "dir_list: no home directory".to_string()))?
        }
        Some(raw) if raw == "~" || raw.starts_with("~/") => PathBuf::from(expand_home(raw)),
        Some(raw) => {
            let candidate = PathBuf::from(raw);
            if !candidate.is_absolute() {
                return Err((
                    ERR_BAD_REQUEST,
                    "dir_list: field 'path' must be absolute or ~-prefixed".to_string(),
                ));
            }
            candidate
        }
    };
    let canonical = std::fs::canonicalize(&requested).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => (
            ERR_FORBIDDEN,
            format!("dir_list: cannot read '{}'", requested.display()),
        ),
        _ => (
            ERR_NOT_FOUND,
            format!("dir_list: no such directory '{}'", requested.display()),
        ),
    })?;
    if !canonical.is_dir() {
        return Err((
            ERR_NOT_FOUND,
            format!("dir_list: not a directory '{}'", requested.display()),
        ));
    }
    Ok(canonical)
}

/// Names of `dir`'s subdirectories — hidden names included, symlinks to
/// directories included, files never. An entry that cannot be read, or
/// whose target cannot be resolved (a dangling symlink), is skipped.
fn subdir_names(dir: &Path) -> Result<Vec<String>, (&'static str, String)> {
    let entries = std::fs::read_dir(dir).map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => (
            ERR_FORBIDDEN,
            format!("dir_list: cannot read '{}'", dir.display()),
        ),
        _ => (
            ERR_INTERNAL,
            format!("dir_list: cannot list '{}'", dir.display()),
        ),
    })?;
    let mut names = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        // `metadata` follows symlinks, so a link to a directory counts and
        // a dangling one fails and is skipped.
        let Ok(metadata) = std::fs::metadata(entry.path()) else {
            continue;
        };
        if metadata.is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(names)
}
