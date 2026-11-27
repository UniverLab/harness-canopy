//! One-shot scheduled graph runs (`graph_scheduled_runs` table).

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use crate::db::Database;

/// Schema for `graph_scheduled_runs` and its `(status, fire_at)` index.
pub(super) const GRAPH_SCHEDULED_RUNS_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS graph_scheduled_runs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                fire_at TEXT NOT NULL,
                idea TEXT,
                queue_id TEXT,
                workdir TEXT,
                on_miss TEXT NOT NULL DEFAULT 'skip',
                status TEXT NOT NULL DEFAULT 'pending',
                created_at TEXT NOT NULL,
                fired_at TEXT,
                error TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_graph_scheduled_runs_status_fire_at
                ON graph_scheduled_runs(status, fire_at);";

/// A one-shot scheduled graph run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphScheduledRun {
    pub id: String,
    pub graph_id: String,
    /// RFC 3339 UTC timestamp at which the run is scheduled to fire.
    pub fire_at: String,
    pub idea: Option<String>,
    pub queue_id: Option<String>,
    pub workdir: Option<String>,
    /// Missed-window policy: `"skip"` or `"run"`.
    pub on_miss: String,
    /// Lifecycle status: `"pending"`, `"fired"`, `"missed"`, or `"cancelled"`.
    pub status: String,
    /// RFC 3339 UTC creation timestamp.
    pub created_at: String,
    /// RFC 3339 UTC timestamp when launched (if fired).
    pub fired_at: Option<String>,
    /// Launch error message if launching failed.
    pub error: Option<String>,
}

fn row_to_scheduled_run(row: &Row<'_>) -> rusqlite::Result<GraphScheduledRun> {
    Ok(GraphScheduledRun {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        fire_at: row.get(2)?,
        idea: row.get(3)?,
        queue_id: row.get(4)?,
        workdir: row.get(5)?,
        on_miss: row.get(6)?,
        status: row.get(7)?,
        created_at: row.get(8)?,
        fired_at: row.get(9)?,
        error: row.get(10)?,
    })
}

const SELECT_COLUMNS: &str =
    "id, graph_id, fire_at, idea, queue_id, workdir, on_miss, status, created_at, fired_at, error";

impl Database {
    /// Ensure `graph_scheduled_runs` exists on databases created before it was added.
    pub(super) fn migrate_graph_scheduled_runs(conn: &Connection) -> Result<()> {
        conn.execute_batch(GRAPH_SCHEDULED_RUNS_SCHEMA)
            .map_err(|e| anyhow!("graph_scheduled_runs schema migration failed: {e}"))?;
        Ok(())
    }

    /// Insert a new one-shot scheduled graph run.
    pub fn insert_graph_scheduled_run(&self, run: &GraphScheduledRun) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        conn.execute(
            "INSERT INTO graph_scheduled_runs
                (id, graph_id, fire_at, idea, queue_id, workdir, on_miss, status, created_at, fired_at, error)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                run.id,
                run.graph_id,
                run.fire_at,
                run.idea,
                run.queue_id,
                run.workdir,
                run.on_miss,
                run.status,
                run.created_at,
                run.fired_at,
                run.error,
            ],
        )?;
        Ok(())
    }

    /// Fetch a scheduled graph run by id.
    pub fn get_graph_scheduled_run(&self, id: &str) -> Result<Option<GraphScheduledRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let sql = format!("SELECT {SELECT_COLUMNS} FROM graph_scheduled_runs WHERE id = ?1");
        let row = conn
            .query_row(&sql, [id], row_to_scheduled_run)
            .optional()?;
        Ok(row)
    }

    /// List scheduled graph runs, optionally filtered by `graph_id` and/or `status`,
    /// ordered by `fire_at ASC`.
    pub fn list_graph_scheduled_runs(
        &self,
        graph_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<GraphScheduledRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let mut clauses: Vec<&str> = Vec::new();
        let mut bind_values: Vec<&str> = Vec::new();
        if let Some(gid) = graph_id {
            clauses.push("graph_id = ?");
            bind_values.push(gid);
        }
        if let Some(st) = status {
            clauses.push("status = ?");
            bind_values.push(st);
        }
        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", clauses.join(" AND "))
        };
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM graph_scheduled_runs {where_sql} ORDER BY fire_at ASC, id ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let params = rusqlite::params_from_iter(bind_values);
        let rows = stmt.query_map(params, row_to_scheduled_run)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Cancel a `pending` or `missed` scheduled run. Refuses `fired` rows.
    /// Returns `Ok(true)` if cancelled, `Ok(false)` if not found or already cancelled.
    pub fn cancel_graph_scheduled_run(&self, id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM graph_scheduled_runs WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        match status.as_deref() {
            None => Ok(false),
            Some("fired") => Err(anyhow!(
                "Cannot cancel scheduled run '{id}': it has already fired."
            )),
            Some("cancelled") => Ok(false),
            Some("pending") | Some("missed") => {
                let updated = conn.execute(
                    "UPDATE graph_scheduled_runs SET status = 'cancelled' WHERE id = ?1",
                    [id],
                )?;
                Ok(updated > 0)
            }
            Some(other) => Err(anyhow!(
                "Cannot cancel scheduled run '{id}' in status '{other}'."
            )),
        }
    }

    /// Update the status, `fired_at`, and `error` fields of a scheduled run.
    pub fn mark_graph_scheduled_run_status(
        &self,
        id: &str,
        status: &str,
        fired_at: Option<&str>,
        error: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {e}"))?;
        let updated = conn.execute(
            "UPDATE graph_scheduled_runs
             SET status = ?2, fired_at = ?3, error = ?4
             WHERE id = ?1",
            params![id, status, fired_at, error],
        )?;
        Ok(updated > 0)
    }
}
