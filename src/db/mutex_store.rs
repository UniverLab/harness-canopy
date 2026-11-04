//! Named mutexes: one node run holds each resource at a time, across all
//! graphs of this daemon. A node (or its ensemble) declares `mutex:
//! ["cpu"]`; the engine inserts its run row as `waiting`, queues it in
//! `mutex_waiters`, and grants the full set atomically (all or none, so a
//! `cpu`+`gpu` node never holds `cpu` while waiting for `gpu`). Grants go to
//! the earliest waiter whose full set is free — an earlier waiter blocked on
//! a busy resource never head-of-line-blocks a later waiter for a different
//! resource. Every state change pings `Database::mutex_notify` so waiters
//! wake without polling (with a 30 s safety poll in the gate).
//!
//! Scoped to this daemon (one machine): leases live in the local database
//! and die with it — `reconcile_mutex_leases` drops whatever a previous
//! daemon left behind.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::Database;

/// One granted resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutexLease {
    pub resource: String,
    pub holder_run_id: String,
    pub graph_id: String,
    pub node_name: String,
    pub acquired_at: String,
}

/// One queued waiter: `run_id` still needs every name in `resources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutexWaiter {
    pub run_id: String,
    pub graph_id: String,
    pub node_name: String,
    pub resources: Vec<String>,
    pub requested_at: String,
}

/// Both halves of the mutex state, for the `mutex list` read surface.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutexState {
    pub leases: Vec<MutexLease>,
    pub waiters: Vec<MutexWaiter>,
}

/// One entry of a waiting run's `waiting_for`: the resource, who holds it
/// (when held — `None` while the resource is free but an earlier waiter is
/// ahead of this run in FIFO order), and since when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutexWaitDetail {
    pub resource: String,
    pub holder_graph: Option<String>,
    pub holder_node: Option<String>,
    pub since: Option<String>,
}

/// Base-schema batch for the mutex tables, referenced from
/// `BASE_SCHEMA_BATCHES` — kept here so the schema lives with the store.
/// `mutex_leases` holds one row per currently-granted resource;
/// `mutex_waiters` queues runs still waiting (FIFO by `requested_at`).
pub(super) const MUTEX_TABLES_SCHEMA: &str = "
    -- Named mutexes: a node run declares the resources it needs and
    -- the engine grants each resource to one node run at a time across
    -- all graphs. Scoped to this daemon (one machine).
    CREATE TABLE IF NOT EXISTS mutex_leases (
        resource TEXT PRIMARY KEY,
        holder_run_id TEXT NOT NULL,
        graph_id TEXT NOT NULL,
        node_name TEXT NOT NULL,
        acquired_at TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS mutex_waiters (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        resources TEXT NOT NULL,
        run_id TEXT NOT NULL UNIQUE,
        graph_id TEXT NOT NULL,
        node_name TEXT NOT NULL,
        requested_at TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_mutex_waiters_requested
        ON mutex_waiters(requested_at ASC);";

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn map_lease_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MutexLease> {
    Ok(MutexLease {
        resource: row.get(0)?,
        holder_run_id: row.get(1)?,
        graph_id: row.get(2)?,
        node_name: row.get(3)?,
        acquired_at: row.get(4)?,
    })
}

fn map_waiter_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MutexWaiter> {
    let resources_text: String = row.get(1)?;
    let resources: Vec<String> = serde_json::from_str(&resources_text).unwrap_or_default();
    Ok(MutexWaiter {
        run_id: row.get(2)?,
        graph_id: row.get(3)?,
        node_name: row.get(4)?,
        resources,
        requested_at: row.get(5)?,
    })
}

impl Database {
    /// Wake every mutex waiter. Called after any lease/waiter change; waiters
    /// re-check under the connection lock, so a notify that lands before the
    /// wait (or with nothing waiting) is harmless.
    pub(crate) fn notify_mutex_waiters(&self) {
        self.mutex_notify.notify_waiters();
    }

    /// The `Notify` waiters park on between grant checks.
    pub(crate) fn mutex_notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.mutex_notify.notified()
    }

    /// Try to grant `resources` to `run_id`, atomically (all or none).
    ///
    /// Enqueues `run_id` first when it is not queued yet, then grants the
    /// earliest waiter (FIFO by `requested_at`) whose full set is free —
    /// returning `true` only when that waiter is `run_id` itself. A waiter
    /// whose full set is free is granted even when an earlier waiter needs a
    /// busy resource (no head-of-line blocking across resources).
    ///
    /// Returns `false` (never granting) when `run_id`'s run row is gone or is
    /// no longer `waiting` — the run was finalized out from under the waiter
    /// (interrupt, pause, reset, shutdown sweep), so its queue entry is
    /// dropped instead. An empty set needs no lease and grants immediately.
    pub fn try_acquire_mutex_set(
        &self,
        run_id: &str,
        graph_id: &str,
        node_name: &str,
        resources: &[String],
    ) -> Result<bool> {
        if resources.is_empty() {
            return Ok(true);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;
        let status: Option<String> = tx
            .query_row(
                "SELECT status FROM graph_runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )
            .optional()?;
        if status.as_deref() != Some("waiting") {
            tx.execute(
                "DELETE FROM mutex_waiters WHERE run_id = ?1",
                params![run_id],
            )?;
            tx.commit()?;
            self.notify_mutex_waiters();
            return Ok(false);
        }
        tx.execute(
            "INSERT OR IGNORE INTO mutex_waiters (resources, run_id, graph_id, node_name, requested_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                serde_json::to_string(resources)?,
                run_id,
                graph_id,
                node_name,
                now_rfc3339(),
            ],
        )?;
        let granted = Self::grant_earliest_grantable_waiter(&tx, run_id)?;
        tx.commit()?;
        if granted {
            self.notify_mutex_waiters();
        }
        Ok(granted)
    }

    /// Grant the earliest waiter whose full set is free. Returns whether that
    /// waiter is `run_id` (and the grant was recorded for it).
    fn grant_earliest_grantable_waiter(
        tx: &rusqlite::Transaction<'_>,
        run_id: &str,
    ) -> Result<bool> {
        let mut stmt = tx.prepare("SELECT resource FROM mutex_leases")?;
        let held: std::collections::HashSet<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut stmt = tx.prepare(
            "SELECT id, resources, run_id, graph_id, node_name, requested_at
             FROM mutex_waiters ORDER BY requested_at ASC, id ASC",
        )?;
        let waiters: Vec<MutexWaiter> = stmt
            .query_map([], map_waiter_row)?
            .collect::<rusqlite::Result<_>>()?;
        for waiter in &waiters {
            if waiter.resources.iter().all(|name| !held.contains(name)) {
                if waiter.run_id != run_id {
                    return Ok(false);
                }
                let acquired_at = now_rfc3339();
                for resource in &waiter.resources {
                    tx.execute(
                        "INSERT OR REPLACE INTO mutex_leases
                             (resource, holder_run_id, graph_id, node_name, acquired_at)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            resource,
                            waiter.run_id,
                            waiter.graph_id,
                            waiter.node_name,
                            acquired_at,
                        ],
                    )?;
                }
                tx.execute(
                    "DELETE FROM mutex_waiters WHERE run_id = ?1",
                    params![waiter.run_id],
                )?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Release every lease held by `run_id` and drop its queue entry, if any.
    /// Called whenever a run ends for any reason (via
    /// `update_graph_run_result`/`interrupt_graph_run` for terminal
    /// transitions) and by reconciliation. Returns the leases released.
    pub fn release_mutexes_for_run(&self, run_id: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let released = Self::release_mutexes_locked(&conn, run_id)?;
        drop(conn);
        if released > 0 {
            self.notify_mutex_waiters();
        }
        Ok(released)
    }

    /// [`Self::release_mutexes_for_run`] over an already-held connection, for
    /// callers (`update_graph_run_result`, `interrupt_graph_run`) that release
    /// inside their own locked write.
    pub(super) fn release_mutexes_locked(conn: &Connection, run_id: &str) -> Result<usize> {
        let released = conn.execute(
            "DELETE FROM mutex_leases WHERE holder_run_id = ?1",
            params![run_id],
        )?;
        conn.execute(
            "DELETE FROM mutex_waiters WHERE run_id = ?1",
            params![run_id],
        )?;
        Ok(released)
    }

    /// Daemon-start reconciliation: release leases whose holder run is not
    /// `running` (a previous daemon died holding them — a live run keeps its
    /// leases) and drop waiters whose run row no longer exists. Only touches
    /// provably-dead state, so it is safe even when another process owns the
    /// daemon lifecycle. Returns `(leases_released, waiters_dropped)`.
    pub fn reconcile_mutex_leases(&self) -> Result<(usize, usize)> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let leases = conn.execute(
            "DELETE FROM mutex_leases
             WHERE holder_run_id NOT IN (SELECT id FROM graph_runs WHERE status = 'running')",
            [],
        )?;
        let waiters = conn.execute(
            "DELETE FROM mutex_waiters
             WHERE run_id NOT IN (SELECT id FROM graph_runs)",
            [],
        )?;
        drop(conn);
        if leases > 0 || waiters > 0 {
            self.notify_mutex_waiters();
        }
        Ok((leases, waiters))
    }

    /// Both halves of the mutex state — leases and FIFO waiters — for the
    /// `mutex list` read surface.
    pub fn list_mutex_state(&self) -> Result<MutexState> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut lease_stmt = conn.prepare(
            "SELECT resource, holder_run_id, graph_id, node_name, acquired_at
             FROM mutex_leases ORDER BY resource ASC",
        )?;
        let leases: Vec<MutexLease> = lease_stmt
            .query_map([], map_lease_row)?
            .collect::<rusqlite::Result<_>>()?;
        let mut waiter_stmt = conn.prepare(
            "SELECT id, resources, run_id, graph_id, node_name, requested_at
             FROM mutex_waiters ORDER BY requested_at ASC, id ASC",
        )?;
        let waiters: Vec<MutexWaiter> = waiter_stmt
            .query_map([], map_waiter_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(MutexState { leases, waiters })
    }

    /// Record `run_id` as waiting for `resources` (no-op when already
    /// queued). The acquire path enqueues by itself; this is the explicit
    /// form for callers that only need the run visible as waiting.
    /// Test-only today (the engine always waits through `try_acquire`).
    #[cfg(test)]
    pub fn set_graph_run_waiting_for(
        &self,
        run_id: &str,
        graph_id: &str,
        node_name: &str,
        resources: &[String],
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT OR IGNORE INTO mutex_waiters (resources, run_id, graph_id, node_name, requested_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                serde_json::to_string(resources)?,
                run_id,
                graph_id,
                node_name,
                now_rfc3339(),
            ],
        )?;
        Ok(())
    }

    /// What `run_id` is waiting for: one entry per requested resource with
    /// the current holder, if any. Empty when the run is not queued.
    pub fn get_graph_run_waiting_for(&self, run_id: &str) -> Result<Vec<MutexWaitDetail>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let waiter: Option<(String, String)> = conn
            .query_row(
                "SELECT resources, requested_at FROM mutex_waiters WHERE run_id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((resources_text, requested_at)) = waiter else {
            return Ok(Vec::new());
        };
        let resources: Vec<String> = serde_json::from_str(&resources_text).unwrap_or_default();
        let mut details = Vec::with_capacity(resources.len());
        for resource in resources {
            let holder: Option<(String, String, String)> = conn
                .query_row(
                    "SELECT graph_id, node_name, acquired_at FROM mutex_leases WHERE resource = ?1",
                    params![resource],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            match holder {
                Some((graph_id, node_name, acquired_at)) => details.push(MutexWaitDetail {
                    resource,
                    holder_graph: Some(graph_id),
                    holder_node: Some(node_name),
                    since: Some(acquired_at),
                }),
                None => details.push(MutexWaitDetail {
                    resource,
                    holder_graph: None,
                    holder_node: None,
                    since: Some(requested_at.clone()),
                }),
            }
        }
        Ok(details)
    }

    /// Flip a granted run from `waiting` to `running`, restarting its clock:
    /// the node's timeout starts at the grant, not at the enqueue. Returns
    /// `false` when the run is no longer `waiting` (finalized out from under
    /// the grant — the caller must release instead of running).
    pub fn mark_graph_run_started_from_waiting(&self, run_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs SET status = 'running', started_at = ?1
             WHERE id = ?2 AND status = 'waiting'",
            params![chrono::Utc::now().timestamp(), run_id],
        )?;
        Ok(rows > 0)
    }
}
