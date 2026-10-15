//! Daily database health routine: full `integrity_check` and
//! `foreign_key_check`, plus a verified single-backup replacement via
//! `VACUUM INTO`, run inside the daemon only while it's idle (no graph
//! running, no TUI attached).
//!
//! Reuses the same `tokio::spawn` + interval-poll pattern the internal cron
//! scheduler (`scheduler::cron_scheduler`) and the RAG ingestion manager
//! (`rag::ingestion::IngestionManager`) already use for their own periodic
//! background work — no new scheduler dependency. [`HealthRoutine::start`]
//! is started and cancelled from `daemon::server::run_http_server` exactly
//! like those.
//!
//! Split in two on purpose:
//! - [`run_health_check`] is the whole check-and-backup as one plain sync
//!   function over a `Database` and two paths. No idle/cadence decision, no
//!   tokio — directly callable from tests and from the on-demand trigger
//!   (`canopy daemon health-check`, see `daemon::cli`) without spinning up
//!   the daemon's background graph.
//! - [`HealthRoutine`] is the daemon-side wrapper: decides *when* to call
//!   it (idle + due), runs it off the async runtime via `spawn_blocking` (a
//!   long `integrity_check` on a large database must not stall graph
//!   execution or MCP requests), and persists the result.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::application::ports::StateRepository;
use crate::db::health::integrity_check_file;
use crate::db::Database;
use crate::domain::db_health::{
    first_deferral_exceeded, is_due, DbHealthOutcome, DbHealthStatus, MAX_HEALTH_DEFERRAL,
    STATE_KEY,
};
use crate::domain::db_paths::{database_path, DB_FILE_NAME};

/// Where the single verified backup lives, under the canopy data directory.
pub fn backup_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!("{DB_FILE_NAME}.backup"))
}

const TEMP_SUFFIX: &str = ".tmp";

/// How often the daemon rechecks whether the routine is due and the daemon
/// is idle. Independent of the 24h cadence itself — a shorter poll just
/// means a busy daemon notices the moment it goes idle sooner, and a skip
/// gets re-evaluated (and re-recorded) promptly rather than staying
/// invisible for hours.
const POLL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// Safety margin over the live database's file size required as free disk
/// space before attempting `VACUUM INTO`: the copy is roughly the same
/// size (usually a little smaller, since it's compacted), so 10% headroom
/// absorbs that without risking a mid-copy `SQLITE_FULL`.
const DISK_SPACE_MARGIN_NUMERATOR: u64 = 11;
const DISK_SPACE_MARGIN_DENOMINATOR: u64 = 10;

/// The daemon-side wrapper around [`run_health_check`]: decides when to run
/// it (idle + due — decision 1 in the health-routine spec) and persists the
/// result.
pub struct HealthRoutine {
    db: Arc<Database>,
    data_dir: PathBuf,
}

impl HealthRoutine {
    pub fn new(db: Arc<Database>, data_dir: PathBuf) -> Self {
        Self { db, data_dir }
    }

    /// Start the routine as a background tokio task. Returns a
    /// `CancellationToken` the caller cancels on daemon shutdown.
    pub fn start(self: Arc<Self>) -> CancellationToken {
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let routine = Arc::clone(&self);
        tokio::spawn(async move {
            tracing::info!("Database health routine started");
            routine.run_graph(cancel_run).await;
            tracing::info!("Database health routine stopped");
        });
        cancel
    }

    async fn run_graph(&self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(POLL_INTERVAL) => {
                    self.maybe_run().await;
                    self.expire_subagent_runs().await;
                    self.prune_operational_sessions().await;
                }
            }
        }
    }

    async fn prune_operational_sessions(&self) {
        let db = Arc::clone(&self.db);
        match tokio::task::spawn_blocking(move || db.prune_operational_sessions(30)).await {
            Ok(Ok(count)) if count > 0 => {
                tracing::info!(
                    "operational sessions cleanup: pruned {count} session(s) older than 30 days"
                );
            }
            Ok(Err(e)) => {
                tracing::warn!("operational sessions cleanup: {e}");
            }
            Err(e) => {
                tracing::warn!("operational sessions cleanup: task panicked: {e}");
            }
            _ => {}
        }
    }

    async fn expire_subagent_runs(&self) {
        let db = Arc::clone(&self.db);
        match tokio::task::spawn_blocking(move || db.expire_subagent_runs()).await {
            Ok(Ok(count)) if count > 0 => {
                tracing::info!("subagent TTL cleanup: expired {count} run(s)");
            }
            Ok(Err(e)) => {
                tracing::warn!("subagent TTL cleanup: {e}");
            }
            Err(e) => {
                tracing::warn!("subagent TTL cleanup: task panicked: {e}");
            }
            _ => {}
        }
    }

    /// Runs the routine if (and only if) it's due and the daemon is
    /// currently idle. If it's due but busy, records the skip (so a
    /// routine that never finds an idle moment is visible) and leaves it
    /// due for the next poll rather than resetting the cadence. FR2: once
    /// the first deferral is older than [`MAX_HEALTH_DEFERRAL`], the run
    /// happens anyway.
    async fn maybe_run(&self) {
        let status = load_status(&self.db);
        let now = Utc::now();
        if !is_due(status.last_run_at, now) {
            return;
        }

        let busy = match self.db.busy_reasons() {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("db health routine: could not check busy state: {e}");
                return;
            }
        };
        if !busy.is_empty() {
            let skipped = Self::record_busy_health_skip(&self.db, status, now, &busy);
            // FR2: never defer forever. Once the first deferral is older
            // than MAX_HEALTH_DEFERRAL, run the same check anyway, through
            // the same spawn_blocking path, so graph/MCP work is unaffected.
            if first_deferral_exceeded(&skipped, now) {
                tracing::info!(
                    "db health routine: running despite busy (deferred {} times since {}, limit {}h): {}",
                    skipped.deferred_count,
                    skipped.first_deferred_at.unwrap_or(now), // guaranteed Some by first_deferral_exceeded
                    MAX_HEALTH_DEFERRAL / 3600,
                    busy.join(", ")
                );
                self.run_and_record(true).await;
            }
            return;
        }

        // Idle path: unchanged, just delegates to the shared runner.
        self.run_and_record(false).await;
    }

    /// Records the skip (so a routine that never finds an idle moment stays
    /// visible) and leaves the routine due for the next poll rather than
    /// resetting the cadence. Returns the persisted status so the caller can
    /// read the updated deferral fields without a reload.
    fn record_busy_health_skip(
        db: &Database,
        status: DbHealthStatus,
        now: DateTime<Utc>,
        busy: &[String],
    ) -> DbHealthStatus {
        let skipped = DbHealthStatus {
            deferred_count: status.deferred_count + 1,
            // Only the FIRST skip since the last completed run records the
            // deadline-start timestamp; later skips keep the original.
            first_deferred_at: status.first_deferred_at.or(Some(now)),
            // A skip is not a run — never carry ran_while_busy=true from a
            // prior forced run (the struct-spread would otherwise preserve it).
            ran_while_busy: false,
            last_skip_at: Some(now),
            last_skip_reason: Some(busy.join(", ")),
            ..status
        };
        tracing::info!("db health routine: deferred — busy: {}", busy.join(", "));
        if let Err(e) = save_status(db, &skipped) {
            tracing::error!("db health routine: failed to record skip: {e}");
        }
        skipped
    }

    /// Runs the whole check-and-backup off the async runtime (a long
    /// `integrity_check` on a large database must not stall graph execution
    /// or MCP requests). `None` when the blocking task panicked — logged.
    async fn run_health_check_blocking(
        db: Arc<Database>,
        db_path: PathBuf,
        backup_path: PathBuf,
    ) -> Option<DbHealthStatus> {
        match tokio::task::spawn_blocking(move || run_health_check(&db, &db_path, &backup_path))
            .await
        {
            Ok(status) => Some(status),
            Err(e) => {
                tracing::error!("db health routine: task panicked: {e}");
                None
            }
        }
    }

    /// Logs the outcome, then persists the status — in that order, so a
    /// failed save is always visible right next to the outcome it failed to
    /// store.
    fn record_health_outcome(db: &Database, new_status: &DbHealthStatus) {
        match &new_status.outcome {
            Some(DbHealthOutcome::Passed) => {
                tracing::info!("db health routine: passed — backup written and verified");
            }
            other => {
                tracing::warn!("db health routine: finished with outcome {:?}", other);
            }
        }
        if let Err(e) = save_status(db, new_status) {
            tracing::error!("db health routine: failed to persist status: {e}");
        }
    }

    /// Runs the health check on a blocking thread and persists the result
    /// (the FR2 forced-due run and the idle path share this). `ran_while_busy`
    /// is set on the saved status only for the forced run — the idle path
    /// passes `false`, which is also the default.
    async fn run_and_record(&self, ran_while_busy: bool) {
        let db = Arc::clone(&self.db);
        let db_path = database_path(&self.data_dir);
        let backup_path = backup_path(&self.data_dir);
        let Some(mut new_status) = Self::run_health_check_blocking(db, db_path, backup_path).await
        else {
            tracing::warn!("db health routine: blocking check did not produce a status");
            return;
        };
        new_status.ran_while_busy = ran_while_busy;
        Self::record_health_outcome(&self.db, &new_status);
    }
}

/// Loads the routine's persisted status, defaulting to "never run" when
/// nothing has been recorded yet (fresh install, or a database predating
/// this routine).
pub fn load_status(db: &Database) -> DbHealthStatus {
    db.get_state(STATE_KEY)
        .ok()
        .flatten()
        .map(|s| DbHealthStatus::from_json(&s))
        .unwrap_or_default()
}

pub fn save_status(db: &Database, status: &DbHealthStatus) -> anyhow::Result<()> {
    db.set_state(STATE_KEY, &status.to_json())
}

/// The whole check-and-backup: full `integrity_check`, `foreign_key_check`,
/// and — only if integrity passed — a verified single-backup replacement.
/// Plain sync function over a `Database` and two paths; no idle/cadence
/// decision here (that's [`HealthRoutine::maybe_run`]'s job), which is what
/// keeps this directly callable from tests and from an on-demand trigger.
pub fn run_health_check(db: &Database, db_path: &Path, backup_path: &Path) -> DbHealthStatus {
    let now = Utc::now();

    let integrity_result = match db.integrity_check() {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("db health routine: integrity_check errored: {e}");
            return DbHealthStatus {
                last_run_at: Some(now),
                outcome: Some(DbHealthOutcome::IntegrityFailed),
                integrity_result: Some(format!("integrity_check errored: {e}")),
                ..Default::default()
            };
        }
    };

    let foreign_key_violations = db
        .foreign_key_check()
        .unwrap_or_else(|e| vec![format!("foreign_key_check errored: {e}")]);

    if let Some(status) = integrity_failure_status(now, &integrity_result, &foreign_key_violations)
    {
        return status;
    }

    let temp_path = temp_backup_path(backup_path);
    // Best-effort cleanup of a temp file abandoned by a prior run that
    // never reached its rename (e.g. the daemon was killed mid-`VACUUM
    // INTO`). `VACUUM INTO` refuses to write over an existing destination,
    // and the previous verified backup at `backup_path` was never touched
    // by that interrupted run — this is purely tidying up, not a recovery
    // step.
    let _ = std::fs::remove_file(&temp_path);

    if let Some(status) = insufficient_backup_space_status(
        now,
        db_path,
        backup_path,
        &integrity_result,
        &foreign_key_violations,
    ) {
        return status;
    }

    if let Err(e) = db.backup_into(&temp_path) {
        tracing::error!("db health routine: VACUUM INTO failed: {e}");
        let _ = std::fs::remove_file(&temp_path);
        return DbHealthStatus {
            last_run_at: Some(now),
            outcome: Some(DbHealthOutcome::BackupVerificationFailed),
            integrity_result: Some(integrity_result),
            foreign_key_violations,
            backup_path: existing_backup_path(backup_path),
            ..Default::default()
        };
    }

    finalize_backup(
        &temp_path,
        backup_path,
        integrity_result,
        foreign_key_violations,
        now,
    )
}

/// The status for a source database that failed (or couldn't finish) its
/// integrity check: `Some` means stop here — a backup taken from a corrupt
/// source would overwrite the last good one — `None` means the source is
/// healthy and the backup may proceed.
fn integrity_failure_status(
    now: DateTime<Utc>,
    integrity_result: &str,
    foreign_key_violations: &[String],
) -> Option<DbHealthStatus> {
    if integrity_result == "ok" {
        return None;
    }
    tracing::error!("db health routine: integrity_check found problems: {integrity_result}");
    Some(DbHealthStatus {
        last_run_at: Some(now),
        outcome: Some(DbHealthOutcome::IntegrityFailed),
        integrity_result: Some(integrity_result.to_string()),
        foreign_key_violations: foreign_key_violations.to_vec(),
        ..Default::default()
    })
}

/// `Some` (skip the backup) when the filesystem holding `backup_path`
/// can't offer the margin `required_backup_space` demands, `None` when
/// there's room — or when either size can't be read at all: callers fail
/// open rather than perpetually skipping backups.
fn insufficient_backup_space_status(
    now: DateTime<Utc>,
    db_path: &Path,
    backup_path: &Path,
    integrity_result: &str,
    foreign_key_violations: &[String],
) -> Option<DbHealthStatus> {
    let required = required_backup_space(db_path)?;
    let available = available_space(backup_path)?;
    if available >= required {
        return None;
    }
    tracing::warn!(
        "db health routine: skipping backup — {available} byte(s) free, \
         {required} required"
    );
    Some(DbHealthStatus {
        last_run_at: Some(now),
        outcome: Some(DbHealthOutcome::BackupSkippedInsufficientDiskSpace),
        integrity_result: Some(integrity_result.to_string()),
        foreign_key_violations: foreign_key_violations.to_vec(),
        backup_path: existing_backup_path(backup_path),
        ..Default::default()
    })
}

/// Verifies a freshly written backup at `temp_path` and, only if it passes
/// its own `integrity_check`, atomically replaces `backup_path` with it
/// (decision 5). On any failure the temp file is discarded and
/// `backup_path` — the previous verified backup, if any — is left exactly
/// as it was.
fn finalize_backup(
    temp_path: &Path,
    backup_path: &Path,
    integrity_result: String,
    foreign_key_violations: Vec<String>,
    now: DateTime<Utc>,
) -> DbHealthStatus {
    let verdict = integrity_check_file(temp_path);
    let verified_ok = matches!(&verdict, Ok(v) if v == "ok");

    if !verified_ok {
        return backup_verification_failure(
            temp_path,
            backup_path,
            integrity_result,
            foreign_key_violations,
            now,
            &verdict,
        );
    }

    replace_verified_backup(
        temp_path,
        backup_path,
        integrity_result,
        foreign_key_violations,
        now,
    )
}

/// The failed-verification verdict: log *why* verification failed (a backup
/// whose own `integrity_check` failed and a verification that errored out
/// are different faults and must read differently), discard the temp file,
/// and report the previous backup as still in place.
fn backup_verification_failure(
    temp_path: &Path,
    backup_path: &Path,
    integrity_result: String,
    foreign_key_violations: Vec<String>,
    now: DateTime<Utc>,
    verdict: &anyhow::Result<String>,
) -> DbHealthStatus {
    match verdict {
        Ok(v) => tracing::error!("db health routine: new backup failed integrity_check: {v}"),
        Err(e) => tracing::error!("db health routine: new backup verification errored: {e}"),
    }
    let _ = std::fs::remove_file(temp_path);
    DbHealthStatus {
        last_run_at: Some(now),
        outcome: Some(DbHealthOutcome::BackupVerificationFailed),
        integrity_result: Some(integrity_result),
        foreign_key_violations,
        backup_path: existing_backup_path(backup_path),
        ..Default::default()
    }
}

/// Swaps the verified temp file into place. Any rename failure discards the
/// temp file and leaves `backup_path` — the previous verified backup —
/// exactly as it was.
fn replace_verified_backup(
    temp_path: &Path,
    backup_path: &Path,
    integrity_result: String,
    foreign_key_violations: Vec<String>,
    now: DateTime<Utc>,
) -> DbHealthStatus {
    if let Err(e) = std::fs::rename(temp_path, backup_path) {
        tracing::error!("db health routine: could not replace backup: {e}");
        let _ = std::fs::remove_file(temp_path);
        return DbHealthStatus {
            last_run_at: Some(now),
            outcome: Some(DbHealthOutcome::BackupVerificationFailed),
            integrity_result: Some(integrity_result),
            foreign_key_violations,
            backup_path: existing_backup_path(backup_path),
            ..Default::default()
        };
    }

    DbHealthStatus {
        last_run_at: Some(now),
        outcome: Some(DbHealthOutcome::Passed),
        integrity_result: Some(integrity_result),
        foreign_key_violations,
        backup_path: Some(backup_path.display().to_string()),
        ..Default::default()
    }
}

fn temp_backup_path(backup_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}{TEMP_SUFFIX}", backup_path.display()))
}

fn existing_backup_path(backup_path: &Path) -> Option<String> {
    backup_path
        .exists()
        .then(|| backup_path.display().to_string())
}

/// Bytes of free space required to safely attempt a backup: the live
/// database's current file size plus a 10% margin. `None` when the file's
/// size can't be read — nothing to require, since there is then also
/// nothing to back up.
fn required_backup_space(db_path: &Path) -> Option<u64> {
    std::fs::metadata(db_path)
        .ok()
        .map(|m| m.len() * DISK_SPACE_MARGIN_NUMERATOR / DISK_SPACE_MARGIN_DENOMINATOR)
}

/// Free space on the filesystem holding `path`'s parent directory. `None`
/// when it can't be determined (e.g. no matching mount found) — callers
/// fail open rather than perpetually skipping backups on a platform where
/// this can't be read.
fn available_space(path: &Path) -> Option<u64> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| dir.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{Graph, GraphStatus};
    use tempfile::tempdir;

    fn make_running_graph(id: &str) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: format!("graph-{id}"),
            description: None,
            workdir: "/tmp/proj".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    // ── run_health_check: pass path ───────────────────────────────────

    #[test]
    fn run_health_check_passes_on_a_healthy_database_and_writes_a_verified_backup() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();
        db.insert_terminal_session("t1", "t1", "bash", "/tmp")
            .unwrap();

        let backup_path = backup_path(dir.path());
        let status = run_health_check(&db, &db_path, &backup_path);

        assert_eq!(status.outcome, Some(DbHealthOutcome::Passed));
        assert_eq!(status.integrity_result.as_deref(), Some("ok"));
        assert!(status.foreign_key_violations.is_empty());
        assert!(status.last_run_at.is_some());
        assert_eq!(status.backup_path, Some(backup_path.display().to_string()));
        assert!(backup_path.exists());
        assert_eq!(integrity_check_file(&backup_path).unwrap(), "ok");
        assert!(
            !temp_backup_path(&backup_path).exists(),
            "temp file must not survive a successful run"
        );
    }

    // ── run_health_check: integrity failure skips backup ──────────────

    #[test]
    fn run_health_check_skips_backup_when_integrity_check_fails() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        {
            let db = Database::new(&db_path).unwrap();
            db.insert_terminal_session("t1", "t1", "bash", "/tmp")
                .unwrap();
            drop(db);
        }
        // Same corruption technique as `db::health`'s and `db::clean`'s own
        // tests: a valid header first, then bit-flipped page data, so this
        // is in-page corruption rather than a "not a database" error.
        let mut bytes = std::fs::read(&db_path).unwrap();
        // CM5: a fixed offset in page 3 rather than bytes.len()/2 — the
        // subagent_runs table shifted the file so the midpoint no longer
        // lands in a page quick_check validates.
        let start = 8192;
        let end = (start + 100).min(bytes.len());
        for b in &mut bytes[start..end] {
            *b ^= 0xFF;
        }
        std::fs::write(&db_path, &bytes).unwrap();

        let db = Database::new(&db_path).unwrap();
        let backup_path = backup_path(dir.path());
        let status = run_health_check(&db, &db_path, &backup_path);

        assert_eq!(status.outcome, Some(DbHealthOutcome::IntegrityFailed));
        assert_ne!(status.integrity_result.unwrap(), "ok");
        assert!(
            !backup_path.exists(),
            "a corrupt source must never produce a backup"
        );
    }

    // ── backup failing its own verification (decision 5) ──────────────

    #[test]
    fn finalize_backup_leaves_previous_backup_untouched_when_new_backup_fails_verification() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();
        db.insert_terminal_session("t1", "t1", "bash", "/tmp")
            .unwrap();

        let backup_path = backup_path(dir.path());
        // Seed a previous good backup.
        db.backup_into(&backup_path).unwrap();
        let previous_backup_bytes = std::fs::read(&backup_path).unwrap();

        // Produce a new backup for real, then deliberately corrupt it in
        // place — simulating damage that happened during (or just after)
        // the write, before verification runs.
        let temp_path = temp_backup_path(&backup_path);
        db.backup_into(&temp_path).unwrap();
        let mut bytes = std::fs::read(&temp_path).unwrap();
        // CM5: a fixed offset in page 3 rather than bytes.len()/2 — the
        // subagent_runs table shifted the file so the midpoint no longer
        // lands in a page quick_check validates.
        let start = 8192;
        let end = (start + 100).min(bytes.len());
        for b in &mut bytes[start..end] {
            *b ^= 0xFF;
        }
        std::fs::write(&temp_path, &bytes).unwrap();

        let status = finalize_backup(
            &temp_path,
            &backup_path,
            "ok".to_string(),
            vec![],
            Utc::now(),
        );

        assert_eq!(
            status.outcome,
            Some(DbHealthOutcome::BackupVerificationFailed)
        );
        assert!(
            !temp_path.exists(),
            "the corrupted new backup must be discarded"
        );
        assert_eq!(
            std::fs::read(&backup_path).unwrap(),
            previous_backup_bytes,
            "the previous backup must be left byte-for-byte untouched"
        );
        assert_eq!(
            status.backup_path,
            Some(backup_path.display().to_string()),
            "status must still name the (old, still-valid) backup"
        );
    }

    // ── interrupted backup (crash mid-VACUUM INTO) ─────────────────────

    #[test]
    fn interrupted_backup_leaves_previous_backup_intact_and_the_next_run_recovers() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();
        db.insert_terminal_session("t1", "t1", "bash", "/tmp")
            .unwrap();

        let backup_path = backup_path(dir.path());
        db.backup_into(&backup_path).unwrap();
        let previous_backup_bytes = std::fs::read(&backup_path).unwrap();

        // Simulate the daemon being killed mid-`VACUUM INTO` on a prior
        // run: an abandoned, partially-written temp file next to the
        // still-valid previous backup.
        let temp_path = temp_backup_path(&backup_path);
        std::fs::write(&temp_path, b"partial write, daemon died here").unwrap();

        // The interruption itself must never have touched the previous
        // backup.
        assert_eq!(std::fs::read(&backup_path).unwrap(), previous_backup_bytes);

        let status = run_health_check(&db, &db_path, &backup_path);

        assert_eq!(status.outcome, Some(DbHealthOutcome::Passed));
        assert!(
            !temp_path.exists(),
            "the abandoned temp file must be cleared before writing a new one"
        );
        assert_eq!(integrity_check_file(&backup_path).unwrap(), "ok");
    }

    // ── busy-skip recording ─────────────────────────────────────────────

    #[tokio::test]
    async fn maybe_run_records_a_skip_without_running_when_the_daemon_is_busy() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Arc::new(Database::new(&db_path).unwrap());
        db.insert_graph(&make_running_graph("graph-1")).unwrap();

        let routine = HealthRoutine::new(Arc::clone(&db), dir.path().to_path_buf());
        routine.maybe_run().await;

        let status = load_status(&db);
        assert!(
            status.last_run_at.is_none(),
            "a busy daemon must not have actually run the routine"
        );
        assert!(status.last_skip_at.is_some());
        assert!(status.last_skip_reason.unwrap().contains("running"));
        assert_eq!(status.deferred_count, 1);
        assert!(status.first_deferred_at.is_some());
        assert!(!backup_path(dir.path()).exists());
    }

    #[test]
    fn record_busy_health_skip_increments_count_and_sets_first_deferral_only_once() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let now1 = Utc::now();
        let now2 = now1 + chrono::Duration::hours(1);

        let first = HealthRoutine::record_busy_health_skip(
            &db,
            DbHealthStatus::default(),
            now1,
            &["1 graph(s) currently running".to_string()],
        );
        assert_eq!(first.deferred_count, 1);
        assert_eq!(first.first_deferred_at, Some(now1));

        let second = HealthRoutine::record_busy_health_skip(
            &db,
            first,
            now2,
            &["2 graph(s) currently running".to_string()],
        );
        assert_eq!(second.deferred_count, 2);
        assert_eq!(
            second.first_deferred_at,
            Some(now1),
            "the first deferral timestamp must stick across later skips"
        );
        assert_eq!(second.last_skip_at, Some(now2));
    }

    #[test]
    fn completed_run_resets_deferral_fields() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();
        db.insert_terminal_session("t1", "t1", "bash", "/tmp")
            .unwrap();
        save_status(
            &db,
            &DbHealthStatus {
                deferred_count: 5,
                first_deferred_at: Some(Utc::now() - chrono::Duration::hours(80)),
                ran_while_busy: true,
                ..Default::default()
            },
        )
        .unwrap();

        let status = run_health_check(&db, &db_path, &backup_path(dir.path()));

        assert_eq!(status.deferred_count, 0);
        assert!(status.first_deferred_at.is_none());
        assert!(!status.ran_while_busy);
    }

    #[tokio::test]
    async fn maybe_run_forces_run_after_max_deferral_while_busy() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Arc::new(Database::new(&db_path).unwrap());
        db.insert_graph(&make_running_graph("graph-1")).unwrap();
        save_status(
            &db,
            &DbHealthStatus {
                last_run_at: Some(Utc::now() - chrono::Duration::hours(25)),
                first_deferred_at: Some(Utc::now() - chrono::Duration::hours(73)),
                deferred_count: 5,
                ..Default::default()
            },
        )
        .unwrap();

        let routine = HealthRoutine::new(Arc::clone(&db), dir.path().to_path_buf());
        routine.maybe_run().await;

        assert!(
            backup_path(dir.path()).exists(),
            "72h of deferral must force the check to run despite the busy daemon"
        );
        let status = load_status(&db);
        assert_eq!(status.outcome, Some(DbHealthOutcome::Passed));
        assert!(status.last_run_at.is_some());
        assert!(status.ran_while_busy, "the forced run must be marked busy");
        assert_eq!(status.deferred_count, 0, "a completed run resets deferrals");
    }

    #[tokio::test]
    async fn maybe_run_does_not_force_run_before_max_deferral_while_busy() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Arc::new(Database::new(&db_path).unwrap());
        db.insert_graph(&make_running_graph("graph-1")).unwrap();
        // Due (last run 25h ago) but the first deferral is only an hour old —
        // short of the 72h ceiling, so the busy daemon must still defer.
        let last_run = Utc::now() - chrono::Duration::hours(25);
        save_status(
            &db,
            &DbHealthStatus {
                last_run_at: Some(last_run),
                first_deferred_at: Some(Utc::now() - chrono::Duration::hours(1)),
                deferred_count: 5,
                ..Default::default()
            },
        )
        .unwrap();

        let routine = HealthRoutine::new(Arc::clone(&db), dir.path().to_path_buf());
        routine.maybe_run().await;

        assert!(
            !backup_path(dir.path()).exists(),
            "short of 72h, a busy daemon must still defer the run"
        );
        let status = load_status(&db);
        assert_eq!(
            status.last_run_at,
            Some(last_run),
            "the deferred routine must not have refreshed its last-run time"
        );
        assert!(status.outcome.is_none(), "no run must have been recorded");
        assert_eq!(status.deferred_count, 6);
    }

    #[tokio::test]
    async fn maybe_run_runs_and_records_a_pass_when_idle() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Arc::new(Database::new(&db_path).unwrap());

        let routine = HealthRoutine::new(Arc::clone(&db), dir.path().to_path_buf());
        routine.maybe_run().await;

        let status = load_status(&db);
        assert_eq!(status.outcome, Some(DbHealthOutcome::Passed));
        assert!(status.last_run_at.is_some());
        assert!(backup_path(dir.path()).exists());
    }

    // ── is_due gating inside maybe_run ──────────────────────────────────

    #[tokio::test]
    async fn maybe_run_does_nothing_when_not_yet_due() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Arc::new(Database::new(&db_path).unwrap());
        save_status(
            &db,
            &DbHealthStatus {
                last_run_at: Some(Utc::now()),
                outcome: Some(DbHealthOutcome::Passed),
                ..Default::default()
            },
        )
        .unwrap();

        let routine = HealthRoutine::new(Arc::clone(&db), dir.path().to_path_buf());
        routine.maybe_run().await;

        assert!(
            !backup_path(dir.path()).exists(),
            "must not run again immediately after a fresh pass"
        );
    }

    // ── extracted helper branches the fixtures above don't reach ───────

    #[test]
    fn finalize_backup_discards_a_temp_file_that_is_not_a_database() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();
        db.backup_into(&backup_path(dir.path())).unwrap();
        drop(db);

        let backup_path = backup_path(dir.path());
        let previous_backup_bytes = std::fs::read(&backup_path).unwrap();

        // Not a SQLite file at all: verification itself *errors* (rather
        // than reporting a failed integrity_check) and must take the same
        // discard-and-keep-the-old-backup path, with its own log line.
        let temp_path = temp_backup_path(&backup_path);
        std::fs::write(&temp_path, b"not a sqlite database at all").unwrap();

        let status = finalize_backup(
            &temp_path,
            &backup_path,
            "ok".to_string(),
            vec![],
            Utc::now(),
        );

        assert_eq!(
            status.outcome,
            Some(DbHealthOutcome::BackupVerificationFailed)
        );
        assert!(
            !temp_path.exists(),
            "the unverifiable temp file must be discarded"
        );
        assert_eq!(
            std::fs::read(&backup_path).unwrap(),
            previous_backup_bytes,
            "the previous backup must be left byte-for-byte untouched"
        );
    }

    #[test]
    fn replace_verified_backup_keeps_everything_when_the_swap_fails() {
        let dir = tempdir().unwrap();
        let backup_path = backup_path(dir.path());
        // A directory where the backup file must go: `rename` refuses, so
        // the temp file is discarded and the previous backup is untouched.
        std::fs::create_dir(&backup_path).unwrap();
        std::fs::write(backup_path.join("marker"), b"previous backup").unwrap();

        let temp_path = temp_backup_path(&backup_path);
        std::fs::write(&temp_path, b"freshly verified backup bytes").unwrap();

        let status = replace_verified_backup(
            &temp_path,
            &backup_path,
            "ok".to_string(),
            vec![],
            Utc::now(),
        );

        assert_eq!(
            status.outcome,
            Some(DbHealthOutcome::BackupVerificationFailed)
        );
        assert!(
            !temp_path.exists(),
            "the failed swap must not leave the temp file behind"
        );
        assert!(
            backup_path.join("marker").exists(),
            "the previous backup must be left exactly as it was"
        );
    }

    #[test]
    fn insufficient_backup_space_status_is_none_when_the_disk_has_room() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();
        db.insert_terminal_session("t1", "t1", "bash", "/tmp")
            .unwrap();

        assert!(
            insufficient_backup_space_status(
                Utc::now(),
                &db_path,
                &backup_path(dir.path()),
                "ok",
                &[],
            )
            .is_none(),
            "an ordinary tempdir has more than 110% of a tiny database free"
        );
    }

    #[test]
    fn record_health_outcome_persists_a_non_passing_outcome() {
        let dir = tempdir().unwrap();
        let db_path = database_path(dir.path());
        let db = Database::new(&db_path).unwrap();

        HealthRoutine::record_health_outcome(
            &db,
            &DbHealthStatus {
                last_run_at: Some(Utc::now()),
                outcome: Some(DbHealthOutcome::IntegrityFailed),
                integrity_result: Some("page 3 failed".to_string()),
                ..Default::default()
            },
        );

        let status = load_status(&db);
        assert_eq!(status.outcome, Some(DbHealthOutcome::IntegrityFailed));
        assert_eq!(status.integrity_result.as_deref(), Some("page 3 failed"));
    }
}
