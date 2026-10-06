use anyhow::Result;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::Database;

impl Database {
    pub fn new(db_path: &PathBuf) -> Result<Self> {
        let conn = Self::open_connection_with_pragmas(db_path)?;
        let db = Database {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.init()?;
        db.seed_startup_intelligence();
        Ok(db)
    }

    /// Open the database file and set the pragmas every fresh connection needs.
    fn open_connection_with_pragmas(db_path: &PathBuf) -> Result<Connection> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        Ok(conn)
    }

    /// Best-effort log for a startup step that failed; open must never fail
    /// because a seed or backfill path isn't registered.
    fn log_startup_step_failure(&self, step: &str, e: &anyhow::Error) {
        tracing::warn!("Could not {step}: {e}");
    }

    /// Best-effort log for a successful containment rebuild.
    fn log_rebuilt_containment(&self, count: usize) {
        tracing::debug!("Rebuilt {count} derived containment edge(s)");
    }

    /// Seed and backfill the intelligence graph on startup, in order. All
    /// best-effort: a failure is logged, never propagated.
    fn seed_startup_intelligence(&self) {
        if let Err(e) = self.backfill_project_nodes() {
            self.log_startup_step_failure("backfill project intelligence nodes", &e);
        }
        // CM9 typed project graph: retype backlog notes, derive containment,
        // seed known cross-project dependencies. All best-effort: open must
        // never fail because a seed path isn't registered.
        if let Err(e) = self.retype_backlog_project_nodes() {
            self.log_startup_step_failure("retype backlog project nodes", &e);
        }
        match self.rebuild_containment_edges() {
            Ok(count) => self.log_rebuilt_containment(count),
            Err(e) => self.log_startup_step_failure("rebuild containment edges", &e),
        }
        if let Err(e) = self.seed_cm9_project_dependencies() {
            self.log_startup_step_failure("seed CM9 project dependencies", &e);
        }
        if let Err(e) = self.seed_builtin_blueprints() {
            self.log_startup_step_failure("seed builtin blueprints", &e);
        }
        if let Err(e) = self.seed_builtin_ensemble_blueprints() {
            self.log_startup_step_failure("seed builtin ensemble blueprints", &e);
        }
    }

    /// Test-only escape hatch: run raw SQL against the handle. Lets UI tests
    /// pin signal timestamps to deterministic values instead of depending on
    /// wall-clock monotonicity — backward NTP/VM steps of hundreds of ms make
    /// "write, tick, write, tick" sequences flaky (PR #51 CI failure), and no
    /// sleep can cover a backward jump.
    #[cfg(test)]
    pub(crate) fn exec_test_sql(&self, sql: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute_batch(sql)?;
        Ok(())
    }

    /// Open the database, running migrations only if no foreign daemon is live.
    /// If a daemon is running, opens the file without running migrations or
    /// seeding (reads and ordinary writes against the existing schema still
    /// work) so a newer binary's migrations don't rename columns under a live
    /// older daemon. Every non-daemon path that opens the real `~/.canopy`
    /// database must use this instead of `new` — `run_http_server` is the one
    /// exception, because it holds the daemon lock and therefore *is* the
    /// daemon.
    pub fn new_safe(db_path: &PathBuf, data_dir: &std::path::Path) -> Result<Self> {
        if crate::daemon::process::other_instance_may_be_running(data_dir) {
            let conn = Connection::open(db_path)?;
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
            return Ok(Database {
                conn: Arc::new(Mutex::new(conn)),
            });
        }
        Database::new(db_path)
    }
}
