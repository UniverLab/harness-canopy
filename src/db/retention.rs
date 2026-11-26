//! Retention pruning for `graph_runs` payloads and stale sessions.
//!
//! The daemon's [`crate::daemon::health_routine`] calls
//! [`run_retention_prune`] once per idle pass: old `graph_runs` rows lose
//! their bulky `input`/`output` payloads first (metadata stays queryable)
//! and are deleted only when twice as old, while stale
//! `interactive_sessions`/`terminal_sessions` rows go straight to deletion.
//! Graph events and spec notes are never touched here.
//!
//! Query/mutate helpers only — the decision of *what* is safe to remove
//! lives in the status allowlists below, mirroring the ones `clean.rs`
//! already enforces for `canopy clean`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use rusqlite::params;

use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;

const SECONDS_PER_DAY: i64 = 86_400;

/// A `graph_runs` row is eligible only when neither its graph nor its spec
/// is still reachable: running/pausing/paused graphs and
/// running/pending/interrupted specs (the resumable states) keep their runs.
const ELIGIBLE_RUN: &str = "NOT EXISTS (SELECT 1 FROM graphs g \
     WHERE g.id = graph_runs.graph_id AND g.status IN ('running', 'pausing', 'paused')) \
     AND NOT EXISTS (SELECT 1 FROM graph_specs s \
     WHERE s.id = graph_runs.spec_id AND s.status IN ('running', 'pending', 'interrupted'))";

/// Row counts for the tables the retention pass manages.
pub struct TableRowCounts {
    pub graph_runs: i64,
    pub interactive_sessions: i64,
    pub terminal_sessions: i64,
}

/// Outcome of one [`run_retention_prune`] pass.
#[derive(Debug, Default)]
pub struct RetentionPruneCounts {
    pub stripped_graph_runs: usize,
    pub deleted_graph_runs: usize,
    pub deleted_interactive_sessions: usize,
    pub deleted_terminal_sessions: usize,
    pub vacuumed: bool,
}

impl RetentionPruneCounts {
    fn total_deleted(&self) -> usize {
        self.deleted_graph_runs + self.deleted_interactive_sessions + self.deleted_terminal_sessions
    }
}

fn parse_rfc3339_ts(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

impl Database {
    /// Strip (`input`/`output` to `NULL`) `graph_runs` rows whose
    /// `COALESCE(completed_at, started_at)` is older than `days` but at most
    /// `2 * days` old, and delete rows older than `2 * days`. Rows on live
    /// graphs/specs (see [`ELIGIBLE_RUN`]) are never touched. Returns
    /// `(stripped, deleted)`. `days == 0` disables pruning and returns zeros.
    pub fn prune_graph_runs(&self, days: u64, now_ts: i64) -> Result<(usize, usize)> {
        if days == 0 {
            return Ok((0, 0));
        }
        let window = (days as i64).saturating_mul(SECONDS_PER_DAY);
        let cutoff_1x = now_ts.saturating_sub(window);
        let cutoff_2x = now_ts.saturating_sub(window.saturating_mul(2));
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let stripped = conn.execute(
            &format!(
                "UPDATE graph_runs SET input = NULL, output = NULL \
                 WHERE COALESCE(completed_at, started_at) >= ?1 \
                 AND COALESCE(completed_at, started_at) < ?2 \
                 AND (input IS NOT NULL OR output IS NOT NULL) \
                 AND {ELIGIBLE_RUN}"
            ),
            params![cutoff_2x, cutoff_1x],
        )? as usize;
        let deleted = conn.execute(
            &format!(
                "DELETE FROM graph_runs \
                 WHERE COALESCE(completed_at, started_at) < ?1 AND {ELIGIBLE_RUN}"
            ),
            params![cutoff_2x],
        )? as usize;
        Ok((stripped, deleted))
    }

    /// Ids of `active`/`resumed` interactive sessions whose process is
    /// really alive right now, per
    /// [`crate::system::session_process_is_live`]. Rows whose process is
    /// already gone are stale and do not count.
    pub fn live_interactive_session_ids(&self) -> Result<HashSet<String>> {
        let current_boot_id = crate::system::boot_id();
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut stmt = conn.prepare(
            "SELECT id, pid, boot_id FROM interactive_sessions
             WHERE status IN ('active', 'resumed')",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|(_, pid, boot_id)| {
                crate::system::session_process_is_live(
                    *pid,
                    boot_id.as_deref(),
                    current_boot_id.as_deref(),
                )
            })
            .map(|(id, _, _)| id)
            .collect())
    }

    /// Delete stale session rows older than `days`: `interactive_sessions`
    /// in `('orphaned', 'error', 'completed')` by
    /// `COALESCE(exited_at, started_at)` and `terminal_sessions` in
    /// `('finished', 'orphaned')` by `COALESCE(last_active, created_at)`.
    /// `active`/`resumed` interactive rows and `idle` terminal rows are never
    /// matched, and neither is any id in `exclude_ids`. Returns
    /// `(deleted_interactive, deleted_terminal)`.
    pub fn prune_stale_sessions(
        &self,
        days: u64,
        now_ts: i64,
        exclude_ids: &HashSet<String>,
    ) -> Result<(usize, usize)> {
        let cutoff = now_ts.saturating_sub((days as i64).saturating_mul(SECONDS_PER_DAY));
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let interactive = delete_sessions_older_than(
            &conn,
            "interactive_sessions",
            "COALESCE(exited_at, started_at)",
            &["orphaned", "error", "completed"],
            cutoff,
            exclude_ids,
        )?;
        let terminal = delete_sessions_older_than(
            &conn,
            "terminal_sessions",
            "COALESCE(last_active, created_at)",
            &["finished", "orphaned"],
            cutoff,
            exclude_ids,
        )?;
        Ok((interactive, terminal))
    }

    /// Row counts for the three tables the retention pass manages.
    pub fn table_row_counts(&self) -> Result<TableRowCounts> {
        let conn = self.conn.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let graph_runs: i64 =
            conn.query_row("SELECT COUNT(*) FROM graph_runs", [], |row| row.get(0))?;
        let interactive_sessions: i64 =
            conn.query_row("SELECT COUNT(*) FROM interactive_sessions", [], |row| {
                row.get(0)
            })?;
        let terminal_sessions: i64 =
            conn.query_row("SELECT COUNT(*) FROM terminal_sessions", [], |row| {
                row.get(0)
            })?;
        Ok(TableRowCounts {
            graph_runs,
            interactive_sessions,
            terminal_sessions,
        })
    }
}

/// Delete rows from one session table whose parsed age instant predates
/// `cutoff_ts` and whose id is not excluded. Timestamps are parsed in Rust
/// (not compared as text) so mixed-precision RFC 3339 values order
/// correctly.
fn delete_sessions_older_than(
    conn: &rusqlite::Connection,
    table: &str,
    age_expr: &str,
    statuses: &[&str],
    cutoff_ts: i64,
    exclude_ids: &HashSet<String>,
) -> Result<usize> {
    let placeholders = vec!["?"; statuses.len()].join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT id, {age_expr} FROM {table} WHERE status IN ({placeholders})"
    ))?;
    let params: Vec<&dyn rusqlite::ToSql> =
        statuses.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt
        .query_map(params.as_slice(), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let ids: Vec<String> = rows
        .into_iter()
        .filter(|(id, at)| parse_rfc3339_ts(at) < cutoff_ts && !exclude_ids.contains(id))
        .map(|(id, _)| id)
        .collect();
    if ids.is_empty() {
        return Ok(0);
    }
    let mut deleted = 0;
    for chunk in ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        deleted += conn.execute(
            &format!("DELETE FROM {table} WHERE id IN ({placeholders})"),
            rusqlite::params_from_iter(chunk.iter()),
        )? as usize;
    }
    Ok(deleted)
}

/// One full retention pass over `db`: strip/delete old `graph_runs`,
/// delete stale sessions (excluding live interactive ids), log one `INFO`
/// line with the four counts, and — when `allow_vacuum` (the idle path,
/// never the busy-forced path) and the deleted total reaches
/// `clean.reclaim_min_deleted_rows` — `VACUUM` once, logging the database
/// file size before and after.
pub fn run_retention_prune(
    db: &Database,
    config: &CanopyConfig,
    db_path: &Path,
    allow_vacuum: bool,
) -> RetentionPruneCounts {
    let now_ts = chrono::Utc::now().timestamp();
    let mut counts = RetentionPruneCounts::default();
    prune_graph_runs_into(db, config.retention.graph_runs_days, now_ts, &mut counts);
    prune_sessions_into(db, config.clean.retention_days, now_ts, &mut counts);
    tracing::info!(
        "retention prune: stripped {} graph_runs, deleted {} graph_runs, \
         deleted {} interactive_sessions, deleted {} terminal_sessions",
        counts.stripped_graph_runs,
        counts.deleted_graph_runs,
        counts.deleted_interactive_sessions,
        counts.deleted_terminal_sessions
    );
    vacuum_if_due(db, config, db_path, allow_vacuum, &mut counts);
    counts
}

fn prune_graph_runs_into(db: &Database, days: u64, now_ts: i64, counts: &mut RetentionPruneCounts) {
    match db.prune_graph_runs(days, now_ts) {
        Ok((stripped, deleted)) => {
            counts.stripped_graph_runs = stripped;
            counts.deleted_graph_runs = deleted;
        }
        Err(e) => tracing::warn!("retention prune: graph_runs: {e}"),
    }
}

fn prune_sessions_into(db: &Database, days: u64, now_ts: i64, counts: &mut RetentionPruneCounts) {
    let live_ids = match db.live_interactive_session_ids() {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("retention prune: live session ids: {e}");
            HashSet::new()
        }
    };
    match db.prune_stale_sessions(days, now_ts, &live_ids) {
        Ok((interactive, terminal)) => {
            counts.deleted_interactive_sessions = interactive;
            counts.deleted_terminal_sessions = terminal;
        }
        Err(e) => tracing::warn!("retention prune: sessions: {e}"),
    }
}

fn vacuum_if_due(
    db: &Database,
    config: &CanopyConfig,
    db_path: &Path,
    allow_vacuum: bool,
    counts: &mut RetentionPruneCounts,
) {
    if !allow_vacuum || counts.total_deleted() < config.clean.reclaim_min_deleted_rows {
        return;
    }
    let before = std::fs::metadata(db_path).ok().map(|m| m.len());
    match db.reclaim_space() {
        Ok(()) => {
            counts.vacuumed = true;
            let after = std::fs::metadata(db_path).ok().map(|m| m.len());
            tracing::info!(
                "retention prune: VACUUM reclaimed space ({} -> {} bytes)",
                before.unwrap_or(0),
                after.unwrap_or(0)
            );
        }
        Err(e) => tracing::warn!("retention prune: VACUUM: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::canopy_config::{CleanConfig, RetentionConfig};

    const DAY: i64 = 86_400;

    fn test_db() -> Database {
        Database::new(&crate::test_support::temp_db_path()).expect("create test db")
    }

    fn rfc3339(ts: i64) -> String {
        chrono::DateTime::from_timestamp(ts, 0)
            .expect("valid test timestamp")
            .to_rfc3339()
    }

    fn seed_chain(
        conn: &rusqlite::Connection,
        graph: &str,
        gstatus: &str,
        spec: &str,
        sstatus: &str,
        node: &str,
    ) {
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES (?1, 'g', '/tmp', ?2, 1)",
            params![graph, gstatus],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES (?1, ?2, 's', 0, 0, ?3)",
            params![spec, graph, sstatus],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, 'n', 'agent', '{}', 0, 1)",
            params![node, spec],
        )
        .unwrap();
    }

    fn seed_run(
        conn: &rusqlite::Connection,
        id: &str,
        graph: &str,
        spec: &str,
        node: &str,
        started_ts: i64,
        completed_ts: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, executed_platform, executed_model) VALUES (?1, ?2, ?3, ?4, 'success', '{\"in\":1}', '{\"out\":2}', ?5, ?6, 3, 'opencode', 'm1')",
            params![id, graph, spec, node, started_ts, completed_ts],
        )
        .unwrap();
    }

    fn run_payload(conn: &rusqlite::Connection, id: &str) -> (Option<String>, Option<String>, i64) {
        conn.query_row(
            "SELECT input, output, iteration FROM graph_runs WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
    }

    fn test_config(graph_runs_days: u64, reclaim: usize) -> CanopyConfig {
        CanopyConfig {
            clean: CleanConfig {
                retention_days: 7,
                reclaim_min_deleted_rows: reclaim,
            },
            retention: RetentionConfig { graph_runs_days },
            ..Default::default()
        }
    }

    #[test]
    fn prune_graph_runs_keeps_strips_and_deletes_by_age() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let conn = db.conn.lock().unwrap();
        seed_chain(&conn, "g", "completed", "s", "completed", "n");
        seed_run(
            &conn,
            "fresh",
            "g",
            "s",
            "n",
            now - 10 * DAY,
            Some(now - 9 * DAY),
        );
        seed_run(
            &conn,
            "mid",
            "g",
            "s",
            "n",
            now - 200 * DAY,
            Some(now - 90 * DAY),
        );
        seed_run(&conn, "mid-open", "g", "s", "n", now - 90 * DAY, None);
        seed_run(
            &conn,
            "old",
            "g",
            "s",
            "n",
            now - 400 * DAY,
            Some(now - 150 * DAY),
        );
        drop(conn);

        let (stripped, deleted) = db.prune_graph_runs(60, now).unwrap();
        assert_eq!((stripped, deleted), (2, 1));

        let conn = db.conn.lock().unwrap();
        assert!(run_payload(&conn, "fresh").0.is_some());
        let (input, output, iteration) = run_payload(&conn, "mid");
        assert_eq!((input, output), (None, None));
        assert_eq!(iteration, 3, "strip keeps metadata");
        let platform: Option<String> = conn
            .query_row(
                "SELECT executed_platform FROM graph_runs WHERE id = 'mid'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(platform.as_deref(), Some("opencode"));
        assert!(run_payload(&conn, "mid-open").0.is_none());
        let old: Option<String> = conn
            .query_row("SELECT id FROM graph_runs WHERE id = 'old'", [], |row| {
                row.get(0)
            })
            .ok();
        assert_eq!(old, None);
        drop(conn);

        assert_eq!(db.prune_graph_runs(60, now).unwrap(), (0, 0));
        let counts = db.table_row_counts().unwrap();
        assert_eq!(counts.graph_runs, 3);
    }

    #[test]
    fn prune_graph_runs_skips_live_graphs_and_specs() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let conn = db.conn.lock().unwrap();
        for (i, gstatus) in ["running", "pausing", "paused"].iter().enumerate() {
            let g = format!("g{i}");
            let s = format!("s{i}");
            let n = format!("n{i}");
            seed_chain(&conn, &g, gstatus, &s, "completed", &n);
            seed_run(
                &conn,
                &format!("r{i}"),
                &g,
                &s,
                &n,
                now - 500 * DAY,
                Some(now - 400 * DAY),
            );
        }
        for (i, sstatus) in ["running", "pending", "interrupted"].iter().enumerate() {
            let g = format!("h{i}");
            let s = format!("t{i}");
            let n = format!("m{i}");
            seed_chain(&conn, &g, "completed", &s, sstatus, &n);
            seed_run(
                &conn,
                &format!("q{i}"),
                &g,
                &s,
                &n,
                now - 500 * DAY,
                Some(now - 400 * DAY),
            );
        }
        seed_chain(&conn, "done-g", "completed", "done-s", "failed", "done-n");
        seed_run(
            &conn,
            "done-r",
            "done-g",
            "done-s",
            "done-n",
            now - 500 * DAY,
            Some(now - 400 * DAY),
        );
        drop(conn);

        assert_eq!(db.prune_graph_runs(60, now).unwrap(), (0, 1));
        assert_eq!(db.table_row_counts().unwrap().graph_runs, 6);
    }

    #[test]
    fn prune_graph_runs_zero_days_is_a_noop() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let conn = db.conn.lock().unwrap();
        seed_chain(&conn, "g", "completed", "s", "completed", "n");
        seed_run(
            &conn,
            "old",
            "g",
            "s",
            "n",
            now - 500 * DAY,
            Some(now - 400 * DAY),
        );
        drop(conn);

        assert_eq!(db.prune_graph_runs(0, now).unwrap(), (0, 0));
        assert_eq!(db.table_row_counts().unwrap().graph_runs, 1);
    }

    fn seed_interactive(
        conn: &rusqlite::Connection,
        id: &str,
        status: &str,
        started_ts: i64,
        exited_ts: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO interactive_sessions (id, name, cli, working_dir, started_at, exited_at, status) VALUES (?1, 'n', 'opencode', '/tmp', ?2, ?3, ?4)",
            params![id, rfc3339(started_ts), exited_ts.map(rfc3339), status],
        )
        .unwrap();
    }

    fn seed_terminal(
        conn: &rusqlite::Connection,
        id: &str,
        status: &str,
        created_ts: i64,
        active_ts: Option<i64>,
    ) {
        conn.execute(
            "INSERT INTO terminal_sessions (id, name, shell, working_dir, created_at, last_active, status) VALUES (?1, 'n', 'bash', '/tmp', ?2, ?3, ?4)",
            params![id, rfc3339(created_ts), active_ts.map(rfc3339), status],
        )
        .unwrap();
    }

    fn session_exists(conn: &rusqlite::Connection, table: &str, id: &str) -> bool {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE id = ?1"),
            params![id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap()
            > 0
    }

    #[test]
    fn prune_stale_sessions_deletes_only_allowlisted_old_rows() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let old = now - 30 * DAY;
        let young = now - DAY;
        let conn = db.conn.lock().unwrap();
        for status in ["orphaned", "error", "completed"] {
            seed_interactive(&conn, &format!("i-{status}"), status, old, Some(old));
        }
        seed_interactive(&conn, "i-active", "active", old, None);
        seed_interactive(&conn, "i-resumed", "resumed", old, None);
        seed_interactive(&conn, "i-young", "orphaned", young, Some(young));
        seed_interactive(&conn, "i-excluded", "orphaned", old, Some(old));
        seed_terminal(&conn, "t-finished", "finished", old, Some(old));
        seed_terminal(&conn, "t-orphaned", "orphaned", old, Some(old));
        seed_terminal(&conn, "t-idle", "idle", old, Some(old));
        seed_terminal(&conn, "t-young", "finished", young, Some(young));
        seed_terminal(&conn, "t-excluded", "finished", old, Some(old));
        drop(conn);

        let mut exclude = HashSet::new();
        exclude.insert("i-excluded".to_string());
        exclude.insert("t-excluded".to_string());
        assert_eq!(db.prune_stale_sessions(7, now, &exclude).unwrap(), (3, 2));

        let conn = db.conn.lock().unwrap();
        for id in [
            "i-orphaned",
            "i-error",
            "i-completed",
            "t-finished",
            "t-orphaned",
        ] {
            let table = if id.starts_with("i-") {
                "interactive_sessions"
            } else {
                "terminal_sessions"
            };
            assert!(!session_exists(&conn, table, id), "{id} deleted");
        }
        for (table, id) in [
            ("interactive_sessions", "i-active"),
            ("interactive_sessions", "i-resumed"),
            ("interactive_sessions", "i-young"),
            ("interactive_sessions", "i-excluded"),
            ("terminal_sessions", "t-idle"),
            ("terminal_sessions", "t-young"),
            ("terminal_sessions", "t-excluded"),
        ] {
            assert!(session_exists(&conn, table, id), "{id} kept");
        }
        drop(conn);

        assert_eq!(db.prune_stale_sessions(7, now, &exclude).unwrap(), (0, 0));
    }

    #[test]
    fn live_interactive_session_ids_reports_only_live_processes() {
        let db = test_db();
        let now = chrono::Utc::now().timestamp();
        let live_pid = std::process::id() as i64;
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO interactive_sessions (id, name, cli, working_dir, started_at, status, pid) VALUES ('live', 'n', 'opencode', '/tmp', ?1, 'active', ?2)",
            params![rfc3339(now), live_pid],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO interactive_sessions (id, name, cli, working_dir, started_at, status, pid) VALUES ('dead', 'n', 'opencode', '/tmp', ?1, 'resumed', 987654321)",
            params![rfc3339(now)],
        )
        .unwrap();
        drop(conn);

        let ids = db.live_interactive_session_ids().unwrap();
        assert!(ids.contains("live"));
        assert!(!ids.contains("dead"));
    }

    #[test]
    fn run_retention_prune_vacuums_only_when_allowed_and_threshold_met() {
        let now = chrono::Utc::now().timestamp();
        let old = now - 400 * DAY;

        let seed_old = |db: &Database| {
            let conn = db.conn.lock().unwrap();
            seed_chain(&conn, "g", "completed", "s", "completed", "n");
            seed_run(&conn, "old", "g", "s", "n", old, Some(old));
            seed_interactive(&conn, "i-old", "orphaned", old, Some(old));
        };

        let path = crate::test_support::temp_db_path();
        let db = Database::new(&path).unwrap();
        seed_old(&db);
        let counts = run_retention_prune(&db, &test_config(60, 1), &path, true);
        assert_eq!(counts.deleted_graph_runs, 1);
        assert_eq!(counts.deleted_interactive_sessions, 1);
        assert!(counts.vacuumed);

        let path = crate::test_support::temp_db_path();
        let db = Database::new(&path).unwrap();
        seed_old(&db);
        let counts = run_retention_prune(&db, &test_config(60, 1), &path, false);
        assert_eq!(counts.deleted_graph_runs, 1);
        assert!(!counts.vacuumed);

        let path = crate::test_support::temp_db_path();
        let db = Database::new(&path).unwrap();
        seed_old(&db);
        let counts = run_retention_prune(&db, &test_config(60, 1_000_000), &path, true);
        assert_eq!(counts.deleted_graph_runs, 1);
        assert!(!counts.vacuumed);
    }
}
