//! Database damage detection and verified-backup restore.
//!
//! A truncated or otherwise corrupted database used to kill the daemon with a
//! bare `database disk image is malformed` error and no way forward. The
//! daily health routine keeps exactly one verified backup
//! ([`super::health_routine::backup_path`], written with `VACUUM INTO` and
//! checked with [`crate::db::health::integrity_check_file`]); this module is
//! the other half of that contract:
//!
//! - [`probe_database`] lets `daemon start` fail fast with a message naming
//!   the backup and the restore command, before any spawn or pid-file write.
//! - [`open_database`] is the daemon's guarded open: same one-line message on
//!   the daemon's `ERROR` log line (which `daemon start` quotes) and as the
//!   returned error. The damaged file is never modified or deleted by either.
//! - [`run_restore`] (`canopy doctor --restore`) moves the damaged file aside
//!   (never deletes it) and copies the verified backup into place.
//!
//! Detection is exact, never heuristic: [`is_damage_error`] only matches a
//! [`rusqlite::Error::SqliteFailure`] whose code is `DatabaseCorrupt` or
//! `NotADatabase` anywhere in the error chain (mirroring
//! [`crate::db::connection::is_busy_error`]). No quota inference, no pattern
//! process killing — restore and orphan recovery only use recorded data.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::db::health::integrity_check_file;
use crate::db::Database;
use crate::domain::db_paths::database_path;

use super::health_routine::backup_path;

/// The single line naming the damage, the backup and the restore command.
///
/// `error` is the damage text (from [`probe_database`] or the failed open);
/// the backup clause names `backup` with its size and UTC mtime when the
/// file exists, and says so explicitly when it does not.
pub(crate) fn damage_message(_db_path: &Path, error: &str, backup: &Path) -> String {
    let base = format!(
        "the database is damaged: {error}; restore the verified backup with: canopy doctor --restore"
    );
    match backup_meta(backup) {
        Some(meta) => format!("{base} (backup {}, {meta})", backup.display()),
        None => format!("{base} (no backup exists at {})", backup.display()),
    }
}

/// `Some("<size MiB>, <UTC date>")` for an existing backup, `None` otherwise.
fn backup_meta(backup: &Path) -> Option<String> {
    let metadata = backup.metadata().ok()?;
    let mib = metadata.len() as f64 / (1024.0 * 1024.0);
    let modified = metadata.modified().ok()?;
    let date = chrono::DateTime::<chrono::Utc>::from(modified).format("%Y-%m-%dT%H:%M:%SZ");
    Some(format!("{mib:.2} MiB, {date} UTC"))
}

/// True when the error chain holds a SQLite corruption failure — and only then.
pub(crate) fn is_damage_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<rusqlite::Error>(),
            Some(rusqlite::Error::SqliteFailure(err, _))
                if matches!(
                    err.code,
                    rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
                )
        )
    })
}

/// Read-only damage probe for `daemon start`: opens `db_path` with
/// `SQLITE_OPEN_READ_ONLY` and reads `sqlite_master`.
///
/// Returns the damage text when that fails with a corruption error, `None`
/// for a healthy file and for a missing file (a missing file is not damage —
/// first start creates it). Never writes: the damaged file is untouched.
pub(crate) fn probe_database(db_path: &Path) -> Option<String> {
    if !db_path.exists() {
        return None;
    }
    let probe = || -> Result<()> {
        let conn = rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })?;
        Ok(())
    };
    match probe() {
        Ok(()) => None,
        Err(error) if is_damage_error(&error) => Some(format!("{error:#}")),
        Err(_) => None,
    }
}

/// The daemon's guarded database open for `run_http_server`: on a corruption
/// open failure, log [`damage_message`]'s single line at `ERROR` (so
/// `daemon.log` carries it and `daemon start` quotes it as the fatal line)
/// and return that line as the error. Any other open error propagates
/// unchanged. The damaged file is never modified or deleted.
pub(crate) fn open_database(data_dir: &Path) -> Result<Database> {
    let db_path = database_path(data_dir);
    match Database::new(&db_path) {
        Ok(db) => Ok(db),
        Err(error) if is_damage_error(&error) => {
            let line = damage_message(&db_path, &format!("{error:#}"), &backup_path(data_dir));
            tracing::error!("{line}");
            Err(anyhow::anyhow!(line))
        }
        Err(error) => Err(error),
    }
}

/// Restore the verified backup over a damaged database (`doctor --restore`).
///
/// `from` overrides the default source ([`backup_path`]); `is_daemon_running`
/// is a parameter so tests can inject it (production passes the
/// `other_instance_may_be_running` / port-occupant probes). Steps (a)–(f):
/// refuse while the daemon runs; refuse a missing source; refuse a source
/// that fails [`integrity_check_file`]; rename the database (plus `-wal` /
/// `-shm` when present) to `<stem>.damaged-<UTC>`; copy the source into
/// place (the backup stays); verify the restored file. Every failure returns
/// a one-line error, and nothing is changed on any refusal.
pub(crate) fn run_restore(
    data_dir: &Path,
    from: Option<&Path>,
    is_daemon_running: &dyn Fn() -> bool,
) -> Result<()> {
    if is_daemon_running() {
        anyhow::bail!("the daemon is running; run canopy daemon stop first");
    }
    let source = from.map_or_else(|| backup_path(data_dir), Path::to_path_buf);
    refuse_unless_verified_backup(&source)?;
    let db_path = database_path(data_dir);
    let damaged = damaged_path(data_dir, &db_path);
    move_aside(&db_path, &damaged).with_context(|| {
        format!(
            "could not move the damaged database aside to {}",
            damaged.display()
        )
    })?;
    std::fs::copy(&source, &db_path).with_context(|| {
        format!(
            "could not copy {} to {}",
            source.display(),
            db_path.display()
        )
    })?;
    match integrity_check_file(&db_path) {
        Ok(result) if result == "ok" => {
            println!(
                "restored {} to {}; integrity_check: ok",
                source.display(),
                db_path.display()
            );
            Ok(())
        }
        Ok(result) => anyhow::bail!("the restored database failed its integrity check: {result}"),
        Err(error) => anyhow::bail!("the restored database failed its integrity check: {error:#}"),
    }
}

/// Refuse unless `source` exists and passes [`integrity_check_file`].
fn refuse_unless_verified_backup(source: &Path) -> Result<()> {
    if !source.exists() {
        anyhow::bail!("no backup found at {}", source.display());
    }
    match integrity_check_file(source) {
        Ok(result) if result == "ok" => Ok(()),
        Ok(result) => anyhow::bail!("the backup failed its integrity check: {result}"),
        Err(error) => anyhow::bail!("the backup failed its integrity check: {error:#}"),
    }
}

/// `<data_dir>/<stem>.damaged-<UTC %Y%m%dT%H%M%SZ>`, e.g. the database
/// `background_agents.db` becomes `background_agents.damaged-20260101T000000Z`.
fn damaged_path(data_dir: &Path, db_path: &Path) -> PathBuf {
    let stem = db_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("database");
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    data_dir.join(format!("{stem}.damaged-{stamp}"))
}

/// Rename `db_path` to `damaged`, and its `-wal` / `-shm` sidecars (when
/// present) to the same name with the same `-wal` / `-shm` suffix.
fn move_aside(db_path: &Path, damaged: &Path) -> Result<()> {
    std::fs::rename(db_path, damaged)?;
    for sidecar in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{sidecar}", db_path.display()));
        if from.exists() {
            let to = PathBuf::from(format!("{}{sidecar}", damaged.display()));
            std::fs::rename(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_failure(code: rusqlite::ErrorCode, extended_code: i32) -> anyhow::Error {
        anyhow::Error::new(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code,
                extended_code,
            },
            None,
        ))
    }

    /// A real database file, then a damaged copy keeping only its first 100
    /// bytes — the spec's truncation shape (valid header, broken pages).
    fn truncated_copy(dir: &Path) -> PathBuf {
        let healthy = dir.join("healthy.db");
        {
            let db = Database::new(&healthy).expect("create healthy database");
            db.backup_into(&dir.join("seed-backup.db"))
                .expect("seed backup");
            drop(db);
        }
        let damaged = dir.join("damaged.db");
        let bytes = std::fs::read(&healthy).expect("read healthy database");
        let end = 100.min(bytes.len());
        std::fs::write(&damaged, &bytes[..end]).expect("write truncated copy");
        damaged
    }

    #[test]
    fn damage_message_names_backup_with_size_and_date() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("canopy.db.backup");
        std::fs::write(&backup, vec![0u8; 2048]).unwrap();
        let line = damage_message(Path::new("/data/canopy.db"), "boom", &backup);
        assert!(
            !line.contains('\n'),
            "damage_message must be exactly one line"
        );
        assert!(line.contains("the database is damaged: boom"));
        assert!(line.contains("canopy doctor --restore"));
        assert!(line.contains(&backup.display().to_string()));
        assert!(line.contains("MiB"));
        assert!(line.contains("UTC"));
    }

    #[test]
    fn damage_message_reports_missing_backup() {
        let missing = Path::new("/data/canopy.db.backup");
        let line = damage_message(Path::new("/data/canopy.db"), "boom", missing);
        assert!(
            !line.contains('\n'),
            "damage_message must be exactly one line"
        );
        assert!(line.contains("the database is damaged: boom"));
        assert!(line.contains("canopy doctor --restore"));
        assert!(line.contains("(no backup exists at /data/canopy.db.backup)"));
    }

    #[test]
    fn is_damage_error_matches_corruption_codes_only() {
        assert!(is_damage_error(&sqlite_failure(
            rusqlite::ErrorCode::DatabaseCorrupt,
            11
        )));
        assert!(is_damage_error(&sqlite_failure(
            rusqlite::ErrorCode::NotADatabase,
            26
        )));
        assert!(!is_damage_error(&sqlite_failure(
            rusqlite::ErrorCode::DatabaseBusy,
            5
        )));
        assert!(!is_damage_error(&anyhow::anyhow!("plain failure")));
    }

    #[test]
    fn probe_database_reports_truncated_copy_and_leaves_bytes_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let damaged = truncated_copy(dir.path());
        let before = std::fs::read(&damaged).unwrap();
        let found = probe_database(&damaged);
        assert!(
            found.is_some(),
            "a truncated database file must probe as damaged"
        );
        let after = std::fs::read(&damaged).unwrap();
        assert_eq!(before, after, "probing must not modify the damaged file");
    }

    #[test]
    fn probe_database_reports_none_for_healthy_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let healthy = dir.path().join("healthy.db");
        Database::new(&healthy)
            .expect("create healthy database")
            .backup_into(&dir.path().join("backup.db"))
            .expect("backup healthy database");
        assert_eq!(probe_database(&healthy), None);
        assert_eq!(probe_database(&dir.path().join("never-created.db")), None);
    }

    /// Temp data dir with a damaged database and a valid backup, the spec's
    /// round-trip shape. Returns the dir; the database lives at the real
    /// [`database_path`] so restore touches the production layout.
    fn round_trip_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let db_path = database_path(dir.path());
        {
            let db = Database::new(&db_path).expect("create database");
            db.backup_into(&backup_path(dir.path()))
                .expect("write valid backup");
            drop(db);
        }
        let bytes = std::fs::read(&db_path).expect("read database");
        let end = 100.min(bytes.len());
        std::fs::write(&db_path, &bytes[..end]).expect("damage the database");
        assert!(probe_database(&db_path).is_some());
        dir
    }

    #[test]
    fn run_restore_round_trip_keeps_damaged_file_and_backup() {
        let dir = round_trip_dir();
        let db_path = database_path(dir.path());
        let backup = backup_path(dir.path());
        let backup_bytes = std::fs::read(&backup).unwrap();

        run_restore(dir.path(), None, &|| false).expect("restore succeeds");

        assert_eq!(
            std::fs::read(&backup).unwrap(),
            backup_bytes,
            "the backup must stay in place"
        );
        let kept: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| {
                name.contains(".damaged-") && !name.ends_with("-wal") && !name.ends_with("-shm")
            })
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "the damaged file is kept under a .damaged- name"
        );
        assert_eq!(
            integrity_check_file(&db_path).unwrap(),
            "ok",
            "the restored file must pass integrity_check"
        );
    }

    #[test]
    fn run_restore_refuses_missing_from_file() {
        let dir = round_trip_dir();
        let db_path = database_path(dir.path());
        let before = std::fs::read(&db_path).unwrap();
        let missing = dir.path().join("no-such-backup.db");
        let error = run_restore(dir.path(), Some(&missing), &|| false).unwrap_err();
        assert!(error.to_string().contains("no backup found at"));
        assert!(error.to_string().contains(&missing.display().to_string()));
        assert_eq!(
            std::fs::read(&db_path).unwrap(),
            before,
            "nothing may change on refusal"
        );
    }

    #[test]
    fn run_restore_refuses_garbage_backup() {
        let dir = round_trip_dir();
        let db_path = database_path(dir.path());
        let before = std::fs::read(&db_path).unwrap();
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, b"this is not a database file at all").unwrap();
        let error = run_restore(dir.path(), Some(&garbage), &|| false).unwrap_err();
        assert!(
            error.to_string().contains("failed its integrity check"),
            "unexpected error: {error:#}"
        );
        assert_eq!(
            std::fs::read(&db_path).unwrap(),
            before,
            "nothing may change on refusal"
        );
    }

    #[test]
    fn run_restore_refuses_while_daemon_is_running() {
        let dir = round_trip_dir();
        let db_path = database_path(dir.path());
        let backup = backup_path(dir.path());
        let before_db = std::fs::read(&db_path).unwrap();
        let before_backup = std::fs::read(&backup).unwrap();
        let error = run_restore(dir.path(), None, &|| true).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the daemon is running; run canopy daemon stop first"
        );
        assert_eq!(std::fs::read(&db_path).unwrap(), before_db);
        assert_eq!(std::fs::read(&backup).unwrap(), before_backup);
    }
}
