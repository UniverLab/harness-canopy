//! Agent backing-graph migration support (split from `db::agent` so that
//! file stays under the 800-line gate).
//!
//! When the daemon migrates a legacy agent row to its spec-less backing
//! graph (one graph per agent, see `daemon::agent_graph_migration`), the
//! helpers here record the move: the row itself is never deleted (rollback
//! safety), only the `migrated_to_graph` ownership flag flips so the backing
//! graph's trigger — never the legacy scheduler path — owns all future
//! fires.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::db::Database;
use crate::domain::graphs::{Graph, GraphNode};
use crate::domain::models::Agent;

impl Database {
    /// Every agent row not yet migrated to its spec-less backing graph
    /// (`migrated_to_graph = 0`), decodable or not skipped the same way as
    /// [`Self::list_agents_where`]. The daemon-start migration consumes this.
    pub fn list_unmigrated_agents(&self) -> Result<Vec<Agent>> {
        self.list_agents_where("WHERE migrated_to_graph = 0")
    }

    /// Whether the agent row is already owned by its backing graph.
    /// A missing row reads as not migrated.
    pub fn is_agent_migrated(&self, id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let flag: Option<bool> = conn
            .query_row(
                "SELECT migrated_to_graph FROM agents WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(flag.unwrap_or(false))
    }

    /// Mark the agent row migrated: its backing graph owns all future fires.
    /// The row itself stays (rollback safety), only the scheduler's
    /// ownership moves.
    pub fn set_migrated_to_graph(&self, id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE agents SET migrated_to_graph = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Insert the agent's spec-less backing graph plus its single worker
    /// node and mark the agent row migrated — one transaction, so a crash
    /// never leaves a graph without its ownership flag (or vice versa).
    /// A retry after a partial failure is safe: the caller checks for an
    /// own-backing graph before calling this again.
    pub fn insert_agent_backing_graph(&self, graph: &Graph, node: &GraphNode) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (trigger_type, trigger_config) = match &graph.trigger {
            Some(trigger) => (
                Some(trigger.type_str().to_string()),
                Some(serde_json::to_string(trigger)?),
            ),
            None => (None, None),
        };
        tx.execute(
            "INSERT INTO graphs (id, name, description, workdir, status, trigger_type, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start, require_spec_type, trigger_prompt, enabled, enable_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            rusqlite::params![
                &graph.id,
                &graph.name,
                &graph.description,
                &graph.workdir,
                graph.status.as_str(),
                trigger_type,
                trigger_config,
                graph.created_at.timestamp(),
                graph.started_at.map(|value| value.timestamp()),
                graph.completed_at.map(|value| value.timestamp()),
                graph.autorun_at.map(|value| value.timestamp()),
                &graph.active_run_queue_id,
                Option::<String>::None,
                graph.auto_continue_at.map(|value| value.timestamp()),
                &graph.auto_continue_action,
                graph.archived,
                graph.paused_by_reconciliation,
                &graph.infra_node_id,
                if graph.hooks.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&graph.hooks)?)
                },
                graph.allow_dirty_start,
                graph.require_spec_type,
                &graph.trigger_prompt,
                graph.enabled,
                graph.enable_at.map(|value| value.timestamp()),
                graph.expires_at.map(|value| value.timestamp()),
            ],
        )?;
        tx.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                &node.id,
                &node.spec_id,
                &node.graph_id,
                &node.name,
                node.kind.as_str(),
                serde_json::to_string(&node.config)?,
                node.position,
                node.created_at.timestamp(),
            ],
        )?;
        tx.execute(
            "UPDATE agents SET migrated_to_graph = 1 WHERE id = ?1",
            rusqlite::params![&graph.id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// The agent id backing `graph_id`, when the graph with that id is the
    /// spec-less backing graph of a migrated agent (same id, flagged row).
    /// Used by the schedulers' overlap guard to attribute a busy backing
    /// graph to its agent's `Missed` history.
    pub fn agent_backing_id_for_graph(&self, graph_id: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let id: Option<String> = conn
            .query_row(
                "SELECT id FROM agents WHERE id = ?1 AND migrated_to_graph = 1",
                params![graph_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(id)
    }

    /// Mark agent rows migrated to their spec-less backing graph (one graph
    /// per agent). `0` (default) keeps the legacy scheduler scheduling the
    /// row; `1` means the backing graph owns all future fires. Rows are
    /// never deleted by the migration (rollback safety: a downgrade finds
    /// them intact, and the flag is meaningless to it). Lives here rather
    /// than in `migration::late` so that file stays under the 800-line gate.
    pub(in crate::db) fn migrate_agent_migrated_to_graph_column(
        conn: &rusqlite::Connection,
    ) -> Result<()> {
        let has_column: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'migrated_to_graph'",
                [],
                |row| Ok(row.get::<_, i32>(0)? > 0),
            )
            .unwrap_or(false);
        if !has_column {
            conn.execute(
                "ALTER TABLE agents ADD COLUMN migrated_to_graph BOOLEAN NOT NULL DEFAULT 0",
                [],
            )
            .map_err(|e| anyhow::anyhow!("Migration failed: {e}"))?;
        }
        Ok(())
    }
}

/// Test-only: insert an agent row with malformed `trigger_config`, the way
/// an external SQLite edit would leave it. Production code never writes
/// such rows; the migration must quarantine them instead of failing on them.
#[cfg(test)]
pub(crate) fn insert_corrupt_agent_row(db: &Database, id: &str, enabled: bool) -> Result<()> {
    use chrono::Utc;
    let conn = db
        .conn
        .lock()
        .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
    conn.execute(
        "INSERT INTO agents (id, prompt, trigger_type, trigger_config, cli, enabled, created_at, log_path, timeout_minutes, trigger_count)
         VALUES (?1, 'corrupt test agent', 'cron', ?2, 'opencode', ?3, ?4, ?5, 15, 0)",
        rusqlite::params![
            id,
            "not a trigger json {",
            enabled,
            Utc::now().to_rfc3339(),
            format!("/tmp/{id}.log"),
        ],
    )?;
    Ok(())
}
