use anyhow::Result;

use crate::db::Database;

use super::{ProjectHistoryEntry, ProjectHistoryKind};

pub(super) fn parse_rfc3339_timestamp(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.timestamp())
        .unwrap_or(0)
}

impl Database {
    /// Persisted history for a project's `Focus → History` tab: finished
    /// graphs plus past (exited/finished) interactive and terminal sessions,
    /// all scoped to `workdir` and merged newest-first. Reads straight from
    /// SQLite (via the `idx_graphs_workdir_created`,
    /// `idx_interactive_sessions_workdir`, and `idx_terminal_sessions_workdir`
    /// indices) rather than the live agent/graph providers, which only know
    /// about state observed since this TUI process started.
    pub fn list_project_history(
        &self,
        workdir: &str,
        limit: usize,
    ) -> Result<Vec<ProjectHistoryEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        let mut entries = Vec::new();

        let mut graph_stmt = conn.prepare(
            "SELECT name, status, COALESCE(completed_at, created_at)
             FROM graphs
             WHERE workdir = ?1 AND status IN ('completed', 'failed')
             ORDER BY COALESCE(completed_at, created_at) DESC
             LIMIT ?2",
        )?;
        let graph_rows = graph_stmt.query_map(rusqlite::params![workdir, limit as i64], |row| {
            Ok(ProjectHistoryEntry {
                kind: ProjectHistoryKind::Graph,
                name: row.get(0)?,
                status: row.get(1)?,
                at: row.get(2)?,
            })
        })?;
        for row in graph_rows {
            entries.push(row?);
        }

        let mut interactive_stmt = conn.prepare(
            "SELECT name, status, COALESCE(exited_at, started_at)
             FROM interactive_sessions
             WHERE working_dir = ?1 AND status != 'active'
             ORDER BY COALESCE(exited_at, started_at) DESC
             LIMIT ?2",
        )?;
        let interactive_rows =
            interactive_stmt.query_map(rusqlite::params![workdir, limit as i64], |row| {
                let name: String = row.get(0)?;
                let status: String = row.get(1)?;
                let at: String = row.get(2)?;
                Ok((name, status, at))
            })?;
        for row in interactive_rows {
            let (name, status, at) = row?;
            entries.push(ProjectHistoryEntry {
                kind: ProjectHistoryKind::InteractiveSession,
                name,
                status,
                at: parse_rfc3339_timestamp(&at),
            });
        }

        let mut terminal_stmt = conn.prepare(
            "SELECT name, status, COALESCE(last_active, created_at)
             FROM terminal_sessions
             WHERE working_dir = ?1 AND status != 'idle'
             ORDER BY COALESCE(last_active, created_at) DESC
             LIMIT ?2",
        )?;
        let terminal_rows =
            terminal_stmt.query_map(rusqlite::params![workdir, limit as i64], |row| {
                let name: String = row.get(0)?;
                let status: String = row.get(1)?;
                let at: String = row.get(2)?;
                Ok((name, status, at))
            })?;
        for row in terminal_rows {
            let (name, status, at) = row?;
            entries.push(ProjectHistoryEntry {
                kind: ProjectHistoryKind::TerminalSession,
                name,
                status,
                at: parse_rfc3339_timestamp(&at),
            });
        }

        entries.sort_by_key(|b| std::cmp::Reverse(b.at));
        entries.truncate(limit);
        Ok(entries)
    }
}
