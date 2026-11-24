//! Confinement for `session_send_file` source paths (audit finding F-01).
//!
//! The shelf copies bytes from a caller-supplied path, so the handler runs
//! every path through [`confine`] first: the resolved path must sit inside
//! one of the caller's allowed roots (its session workdir, the shelf
//! directory), must not sit under the canopy data directory outside the
//! shelf subtree, and must not look like a secret (`daemon.token`,
//! `*.key`, `*.pem`, `id_*`). Anything else is refused with the same
//! `forbidden` code and message whether the file exists or not, so the
//! answer never reveals existence.

use std::path::{Component, Path, PathBuf};

use super::shelf::{ShelfError, ERR_FORBIDDEN};

/// The message of every `forbidden` refusal: identical for existing and
/// missing paths, inside and outside the roots, so it leaks nothing.
pub const FORBIDDEN_MESSAGE: &str = "path is outside this session's workdir";

/// Check `path` against `allowed_roots` (already canonical) and `data_dir`
/// (canopy's data dir, e.g. `~/.canopy`).
///
/// A path that resolves (symlinks, `..`) is checked by its canonical form;
/// one that does not resolve (missing, dangling link, permission) is
/// checked by its lexical normalization, so a missing path inside a root
/// still reaches [`crate::sessions::shelf::ShelfStore::insert`] (which
/// reports `not_found`/`io`) while a missing path outside every root gets
/// the same `forbidden` refusal as an existing file there. Returns the
/// checked (canonical or normalized) path on success; the handler still
/// passes the original path to `insert` so names and messages are stable.
pub fn confine(
    path: &Path,
    allowed_roots: &[PathBuf],
    data_dir: &Path,
) -> Result<PathBuf, ShelfError> {
    let candidate = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(_) => normalize_lexically(path),
    };
    // The candidate is canonical, so the data dir must be canonical too:
    // otherwise a symlinked `$HOME` (or `TMPDIR`) would let a workdir that
    // links into `~/.canopy` slip past the data-dir rule via its workdir
    // root. A data dir that does not resolve keeps its raw form.
    let canonical_data = std::fs::canonicalize(data_dir).unwrap_or_else(|_| data_dir.to_path_buf());
    check_candidate(&candidate, allowed_roots, &canonical_data)?;
    Ok(candidate)
}

/// Resolve `.` and `..` components without touching the disk, so a missing
/// path can still be judged against the roots.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            _ => out.push(component.as_os_str()),
        }
    }
    out
}

/// Whether the file name looks like a secret, wherever it sits.
fn denied_file_name(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name == "daemon.token"
        || name.ends_with(".key")
        || name.ends_with(".pem")
        || name.starts_with("id_")
}

/// The same deny + containment checks for a resolved and for a lexically
/// normalized path, so both spellings get the identical refusal.
fn check_candidate(
    candidate: &Path,
    allowed_roots: &[PathBuf],
    data_dir: &Path,
) -> Result<(), ShelfError> {
    let forbidden = || ShelfError::new(ERR_FORBIDDEN, FORBIDDEN_MESSAGE);
    if denied_file_name(candidate) {
        return Err(forbidden());
    }
    // The shelf subtree is exempt: it lives inside the data dir, and the
    // handler allows it as a root.
    let shelf_dir = data_dir.join("shelf");
    if candidate.starts_with(data_dir) && !candidate.starts_with(&shelf_dir) {
        return Err(forbidden());
    }
    if allowed_roots.iter().any(|root| candidate.starts_with(root)) {
        return Ok(());
    }
    Err(forbidden())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workdir, a data dir with a `shelf/` subtree, and roots allowing
    /// exactly those two — the handler's shape for a caller with a session.
    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Vec<PathBuf>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let workdir = dir.path().join("work");
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&workdir).expect("work dir");
        std::fs::create_dir_all(data_dir.join("shelf")).expect("shelf dir");
        let roots = vec![
            std::fs::canonicalize(&workdir).expect("canonical workdir"),
            std::fs::canonicalize(data_dir.join("shelf")).expect("canonical shelf"),
        ];
        (dir, workdir, data_dir, roots)
    }

    fn write(path: &Path) {
        std::fs::write(path, b"bytes").expect("write fixture file");
    }

    #[test]
    fn file_inside_workdir_passes() {
        let (_dir, workdir, data_dir, roots) = fixture();
        let path = workdir.join("shot.png");
        write(&path);
        assert!(confine(&path, &roots, &data_dir).is_ok());
    }

    #[test]
    fn dot_dot_escape_is_forbidden() {
        let (_dir, workdir, data_dir, roots) = fixture();
        let outside = workdir.parent().expect("temp parent").join("escape.png");
        write(&outside);
        let attempt = workdir.join("..").join("escape.png");
        let err = confine(&attempt, &roots, &data_dir).expect_err("escape refused");
        assert_eq!(err.code, ERR_FORBIDDEN);
        assert_eq!(err.message, FORBIDDEN_MESSAGE);
    }

    #[test]
    fn symlink_to_data_dir_token_is_forbidden() {
        let (_dir, workdir, data_dir, roots) = fixture();
        let token = data_dir.join("daemon.token");
        write(&token);
        let link = workdir.join("link.png");
        std::os::unix::fs::symlink(&token, &link).expect("symlink");
        let err = confine(&link, &roots, &data_dir).expect_err("link refused");
        assert_eq!(err.code, ERR_FORBIDDEN);
        assert_eq!(err.message, FORBIDDEN_MESSAGE);
    }

    #[test]
    fn secret_names_inside_workdir_are_forbidden() {
        let (_dir, workdir, data_dir, roots) = fixture();
        for name in [
            "daemon.token",
            "agent.key",
            "agent.pem",
            "id_rsa",
            "id_ed25519.pub",
        ] {
            let path = workdir.join(name);
            write(&path);
            let err = confine(&path, &roots, &data_dir).expect_err("secret refused");
            assert_eq!(err.code, ERR_FORBIDDEN, "{name}");
            assert_eq!(err.message, FORBIDDEN_MESSAGE, "{name}");
        }
    }

    #[test]
    fn missing_secret_name_inside_workdir_is_still_forbidden() {
        let (_dir, workdir, data_dir, roots) = fixture();
        let err = confine(&workdir.join("id_rsa"), &roots, &data_dir).expect_err("refused");
        assert_eq!(err.code, ERR_FORBIDDEN);
    }

    #[test]
    fn file_under_data_dir_outside_shelf_is_forbidden() {
        let (_dir, _workdir, data_dir, mut roots) = fixture();
        let path = data_dir.join("config.toml");
        write(&path);
        // Even with the data dir itself allowed as a root, the data-dir
        // rule fires first.
        roots.push(std::fs::canonicalize(&data_dir).expect("canonical data dir"));
        let roots_plus_data = roots;
        let err = confine(&path, &roots_plus_data, &data_dir).expect_err("refused");
        assert_eq!(err.code, ERR_FORBIDDEN);
        assert_eq!(err.message, FORBIDDEN_MESSAGE);
    }

    #[test]
    fn workdir_linking_into_symlinked_data_dir_is_forbidden() {
        // A symlinked `$HOME` (or `TMPDIR`) plus a workdir that links into
        // `~/.canopy`: the candidate is canonical, so the data dir must be
        // canonicalized too, or the workdir root lets the file through.
        let dir = tempfile::tempdir().expect("temp dir");
        let real_data = dir.path().join("data");
        let proj = real_data.join("proj");
        std::fs::create_dir_all(real_data.join("shelf")).expect("shelf dir");
        std::fs::create_dir_all(&proj).expect("proj dir");
        let data_link = dir.path().join("datalink");
        std::os::unix::fs::symlink(&real_data, &data_link).expect("symlink data dir");
        let work_link = dir.path().join("work");
        std::os::unix::fs::symlink(&proj, &work_link).expect("symlink workdir");
        let config = proj.join("config.toml");
        write(&config);

        let roots = vec![
            std::fs::canonicalize(&work_link).expect("canonical workdir"),
            std::fs::canonicalize(real_data.join("shelf")).expect("canonical shelf"),
        ];
        let err = confine(&config, &roots, &data_link).expect_err("data-dir file refused");
        assert_eq!(err.code, ERR_FORBIDDEN);
        assert_eq!(err.message, FORBIDDEN_MESSAGE);
    }

    #[test]
    fn file_under_shelf_passes() {
        let (_dir, _workdir, data_dir, roots) = fixture();
        let path = data_dir.join("shelf").join("abc.png");
        write(&path);
        assert!(confine(&path, &roots, &data_dir).is_ok());
    }

    #[test]
    fn missing_inside_workdir_passes_to_insert() {
        let (_dir, workdir, data_dir, roots) = fixture();
        let missing = workdir.join("missing.png");
        assert!(!missing.exists());
        assert!(confine(&missing, &roots, &data_dir).is_ok());
    }

    #[test]
    fn missing_outside_roots_matches_existing_outside_refusal() {
        let (dir, _workdir, data_dir, roots) = fixture();
        let existing = dir.path().join("real.png");
        write(&existing);
        let existing_err = confine(&existing, &roots, &data_dir).expect_err("refused");
        let missing = dir.path().join("no-such.png");
        assert!(!missing.exists());
        let missing_err = confine(&missing, &roots, &data_dir).expect_err("refused");
        assert_eq!(existing_err.code, ERR_FORBIDDEN);
        assert_eq!(missing_err.code, ERR_FORBIDDEN);
        assert_eq!(missing_err.message, existing_err.message);
        assert_eq!(missing_err.message, FORBIDDEN_MESSAGE);
    }

    #[test]
    fn directory_and_fifo_inside_workdir_pass_to_insert() {
        let (_dir, workdir, data_dir, roots) = fixture();
        // Both are contained, so `confine` passes them: `insert` reports
        // the unchanged `not_a_file`.
        assert!(confine(&workdir, &roots, &data_dir).is_ok());
        let fifo = workdir.join("pipe");
        let cstr = std::ffi::CString::new(fifo.to_string_lossy().into_owned().into_bytes())
            .expect("cstring fifo path");
        // SAFETY: `mkfifo` with a NUL-terminated path and user read/write.
        let rc = unsafe { libc::mkfifo(cstr.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo");
        assert!(confine(&fifo, &roots, &data_dir).is_ok());
    }

    #[test]
    fn shelf_only_roots_refuse_workdir_file_and_pass_shelf_file() {
        let (_dir, workdir, data_dir, _roots) = fixture();
        // A caller with no session row: the shelf directory is the only
        // allowed root.
        let shelf_root = std::fs::canonicalize(data_dir.join("shelf")).expect("shelf");
        let roots = vec![shelf_root];
        let work_file = workdir.join("shot.png");
        write(&work_file);
        let err = confine(&work_file, &roots, &data_dir).expect_err("refused");
        assert_eq!(err.code, ERR_FORBIDDEN);
        let shelf_file = data_dir.join("shelf").join("shot.png");
        write(&shelf_file);
        assert!(confine(&shelf_file, &roots, &data_dir).is_ok());
    }
}
