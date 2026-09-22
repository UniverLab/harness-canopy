use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Error as IoError, ErrorKind};

use crate::db::Database;
use crate::domain::graphs::{
    ArchiveGraphOutcome, Graph, GraphCompletionHook, GraphCompletionHookRun, GraphDetails,
    GraphEdge, GraphEdgeCondition, GraphHookEvent, GraphNode, GraphNodeKind, GraphNodeRun,
    GraphResetOutcome, GraphRunStatus, GraphSpec, GraphSpecDetails, GraphSpecStatus, GraphStatus,
    SpecAdminStatusOutcome,
};
use crate::domain::models::Trigger;

/// CB43: one platform+model pair this installation has run in the window —
/// when it last ran, how many times, and how the most recent run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentModelUsage {
    pub platform: String,
    pub model: Option<String>,
    pub last_run: DateTime<Utc>,
    pub count: i64,
    pub last_outcome: String,
}

/// CB43/CB46: shared window for installation recent usage.
pub const RECENT_USAGE_WINDOW_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct RecentPairSource {
    pub platform: String,
    pub model: Option<String>,
    pub started_at: DateTime<Utc>,
    pub status: String,
    pub source: RecentPairSourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecentPairSourceKind {
    GraphNode {
        graph_id: String,
        graph_name: String,
        node_id: String,
        node_name: String,
    },
    CompletionHook {
        graph_id: String,
        graph_name: String,
        event: String,
        hook_index: i64,
    },
    BackgroundAgent {
        agent_id: String,
        prompt: String,
    },
    SubagentSpawn {
        workdir: String,
    },
}

impl Database {
    pub fn delete_graph(&self, graph_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute("DELETE FROM graphs WHERE id = ?1", params![graph_id])?;
        Ok(())
    }

    pub fn insert_graph(&self, lp: &Graph) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let (trigger_type, trigger_config) = encode_graph_trigger(lp.trigger.as_ref())?;
        let on_completed = encode_graph_completion_hook(
            lp.hooks
                .get(&GraphHookEvent::OnCompleted)
                .and_then(|v| v.first()),
        )?;
        let hooks = encode_graph_hooks(&lp.hooks)?;
        conn.execute(
            "INSERT INTO graphs (id, name, description, workdir, status, trigger_type, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
            params![
                &lp.id,
                &lp.name,
                &lp.description,
                &lp.workdir,
                lp.status.as_str(),
                trigger_type,
                trigger_config,
                lp.created_at.timestamp(),
                lp.started_at.map(|value| value.timestamp()),
                lp.completed_at.map(|value| value.timestamp()),
                lp.autorun_at.map(|value| value.timestamp()),
                &lp.active_run_queue_id,
                on_completed,
                lp.auto_continue_at.map(|value| value.timestamp()),
                &lp.auto_continue_action,
                lp.archived,
                lp.paused_by_reconciliation,
                &lp.infra_node_id,
                hooks,
                lp.allow_dirty_start,
            ],
        )?;
        Ok(())
    }

    /// Schedule a one-shot resume for a graph at `at`. The scheduler fires it
    /// once `at` is reached (if the graph is fireable) and clears the field —
    /// see [`crate::domain::graphs::Graph::is_autorun_due`].
    pub fn schedule_graph_autorun(&self, graph_id: &str, at: DateTime<Utc>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET autorun_at = ?1 WHERE id = ?2",
            params![at.timestamp(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear a graph's pending one-shot autorun schedule, without touching status.
    pub fn clear_graph_autorun(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET autorun_at = NULL WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Graphs with a pending one-shot autorun schedule (regardless of trigger).
    pub fn list_pending_autorun_graphs(&self) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
             FROM graphs WHERE autorun_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], map_graph_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Schedule a one-shot deferred resume for a *paused* graph at `at`: the
    /// scheduler fires the `graph_continue` action `action` (defaults to
    /// `retry_current_node` when `None`) once `at` is reached, but only if
    /// the graph is still `Paused` — see
    /// [`crate::domain::graphs::Graph::is_auto_continue_due`]. Distinct from
    /// [`Self::schedule_graph_autorun`], which resets-and-relaunches instead
    /// of resuming in place.
    pub fn schedule_graph_auto_continue(
        &self,
        graph_id: &str,
        at: DateTime<Utc>,
        action: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET auto_continue_at = ?1, auto_continue_action = ?2 WHERE id = ?3",
            params![at.timestamp(), action, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear a graph's pending one-shot auto-continue schedule, without
    /// touching status.
    pub fn clear_graph_auto_continue(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET auto_continue_at = NULL, auto_continue_action = NULL WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Graphs with a pending one-shot auto-continue schedule (regardless of
    /// trigger or current status — callers gate firing on
    /// [`crate::domain::graphs::Graph::is_auto_continue_due`]).
    pub fn list_pending_auto_continue_graphs(&self) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
             FROM graphs WHERE auto_continue_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], map_graph_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Replace a graph's trigger (cron/watch/manual). Passing `None` clears any
    /// existing trigger, making the graph manual-only.
    pub fn update_graph_trigger(&self, graph_id: &str, trigger: Option<&Trigger>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let (trigger_type, trigger_config) = encode_graph_trigger(trigger)?;
        let rows = conn.execute(
            "UPDATE graphs SET trigger_type = ?1, trigger_config = ?2 WHERE id = ?3",
            params![trigger_type, trigger_config, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Replace a graph's `on_completed` hook config (N2). Passing `None`
    /// clears it, making the graph's completion behave exactly as it did
    /// before N2 (no hook). Writes both the legacy `on_completed` column
    /// and the new `hooks` map for rollback compatibility. Preserves
    /// hooks registered for other events.
    pub fn update_graph_completion_hook(
        &self,
        graph_id: &str,
        hook: Option<&GraphCompletionHook>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let on_completed = encode_graph_completion_hook(hook)?;
        // Read the current hooks map, update on_completed, write it back
        let current_hooks_json: Option<String> = conn
            .query_row(
                "SELECT hooks FROM graphs WHERE id = ?1",
                params![graph_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let mut hooks_map: std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>> =
            current_hooks_json
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or_default();
        if let Some(h) = hook {
            hooks_map.insert(GraphHookEvent::OnCompleted, vec![h.clone()]);
        } else {
            hooks_map.remove(&GraphHookEvent::OnCompleted);
        }
        let hooks_json = encode_graph_hooks(&hooks_map)?;
        let rows = conn.execute(
            "UPDATE graphs SET on_completed = ?1, hooks = ?2 WHERE id = ?3",
            params![on_completed, hooks_json, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Replace the full hooks map for a graph. Used by MCP graph_update with
    /// the new event-keyed `hooks` parameter.
    pub fn update_graph_hooks(
        &self,
        graph_id: &str,
        hooks: &std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let hooks_json = encode_graph_hooks(hooks)?;
        // Also sync the legacy `on_completed` column for rollback compat
        let on_completed_legacy = hooks
            .get(&GraphHookEvent::OnCompleted)
            .and_then(|v| v.first())
            .map(serde_json::to_string)
            .transpose()?;
        let rows = conn.execute(
            "UPDATE graphs SET hooks = ?1, on_completed = ?2 WHERE id = ?3",
            params![hooks_json, on_completed_legacy, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Graphs that fire on a cron schedule (their trigger is `Cron`).
    pub fn list_cron_graphs(&self) -> Result<Vec<Graph>> {
        self.list_graphs_where_trigger("cron")
    }

    /// Graphs that fire on a file-system watch (their trigger is `Watch`).
    pub fn list_watch_graphs(&self) -> Result<Vec<Graph>> {
        self.list_graphs_where_trigger("watch")
    }

    fn list_graphs_where_trigger(&self, trigger_type: &str) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
             FROM graphs WHERE trigger_type = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![trigger_type], map_graph_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_graph_details(
        &self,
        graph_id: &str,
        name: Option<&str>,
        description: Option<Option<&str>>,
        workdir: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs
             SET name = COALESCE(?1, name),
                 description = CASE
                     WHEN ?2 IS NULL THEN description
                     ELSE ?3
                 END,
                 workdir = COALESCE(?4, workdir)
             WHERE id = ?5",
            params![
                name,
                description.map(|_| 1),
                description.flatten(),
                workdir,
                graph_id
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_infra_node_id(
        &self,
        graph_id: &str,
        infra_node_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET infra_node_id = ?1 WHERE id = ?2",
            params![infra_node_id, graph_id],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_allow_dirty_start(&self, graph_id: &str, value: bool) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE graphs SET allow_dirty_start = ?1 WHERE id = ?2",
            params![value, graph_id],
        )?;
        Ok(())
    }

    pub fn get_graph(&self, graph_id: &str) -> Result<Option<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
             FROM graphs WHERE id = ?1",
        )?;

        stmt.query_row(params![graph_id], map_graph_row)
            .optional()
            .map_err(Into::into)
    }

    /// List graphs, optionally narrowed to one `workdir`. `include_archived`
    /// controls whether archived graphs are included: `false` is the
    /// "browsing" view (sidebar, `canopy graph list`, MCP `graph_list`) — an
    /// archived graph is excluded by the query itself (not filtered in
    /// memory), never by loading every row and discarding some. Pass `true`
    /// for a lookup that must still resolve an archived graph (e.g. `canopy
    /// graph info` by id/name) or to list the archived set for the archive
    /// view.
    pub fn list_graphs(&self, workdir: Option<&str>, include_archived: bool) -> Result<Vec<Graph>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let archived_clause = if include_archived {
            ""
        } else {
            " AND archived = 0"
        };
        let sql = if workdir.is_some() {
            format!(
                "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
                 FROM graphs WHERE workdir = ?1{archived_clause} ORDER BY created_at DESC"
            )
        } else {
            format!(
                "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
                 FROM graphs WHERE 1=1{archived_clause} ORDER BY created_at DESC"
            )
        };
        let mut stmt = conn.prepare(&sql)?;
        let rows = if let Some(workdir) = workdir {
            stmt.query_map(params![workdir], map_graph_row)?
        } else {
            stmt.query_map([], map_graph_row)?
        };

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Archive a graph: it leaves every browsing listing (`list_graphs` with
    /// `include_archived: false`) but its row, specs, and run history are
    /// untouched — a single-row, atomic flag flip, never a delete/recreate
    /// or a move to another table (the graph keeps its id and every foreign
    /// key into it). Refuses a `running` graph (archiving is for work that's
    /// finished with; pause it first) and a graph that's already archived.
    pub fn archive_graph(&self, graph_id: &str) -> Result<ArchiveGraphOutcome> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT status, archived FROM graphs WHERE id = ?1",
                params![graph_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, archived)) = row else {
            return Ok(ArchiveGraphOutcome::NotFound);
        };
        if GraphStatus::from_str(&status) == GraphStatus::Running
            || GraphStatus::from_str(&status) == GraphStatus::Pausing
        {
            return Ok(ArchiveGraphOutcome::Running);
        }
        if archived != 0 {
            return Ok(ArchiveGraphOutcome::AlreadyArchived);
        }
        conn.execute(
            "UPDATE graphs SET archived = 1 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(ArchiveGraphOutcome::Archived)
    }

    /// Restore an archived graph back to the main browsing list. A single-row,
    /// atomic flag flip — everything the graph carries (specs, run history)
    /// was never touched by archiving in the first place. Returns `true` when
    /// a row was actually flipped (i.e. it existed and was archived).
    pub fn restore_graph(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET archived = 0 WHERE id = ?1 AND archived = 1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Count of archived graphs — the always-visible number that makes the
    /// archive non-invisible (see the F4-archive spec). A dedicated `COUNT(*)`
    /// query, not `list_graphs(...).len()`, so the main view never pays for
    /// loading every archived row just to show a number.
    pub fn count_archived_graphs(&self) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT COUNT(*) FROM graphs WHERE archived = 1",
            [],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub fn update_graph_status(
        &self,
        graph_id: &str,
        status: GraphStatus,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        // C1: every status transition through this generic path is explicit
        // (an operator's pause, a run finishing, a launch claiming the graph)
        // — never `reconcile_orphaned_graphs`'s own automatic pause, which
        // writes `paused_by_reconciliation` through its own dedicated
        // statement. Clearing it here unconditionally keeps that flag scoped
        // to exactly the graphs reconciliation itself paused.
        let rows = conn.execute(
            "UPDATE graphs
             SET status = ?1,
                 started_at = COALESCE(?2, started_at),
                 completed_at = COALESCE(?3, completed_at),
                 paused_by_reconciliation = 0
             WHERE id = ?4",
            params![
                status.as_str(),
                started_at.map(|value| value.timestamp()),
                completed_at.map(|value| value.timestamp()),
                graph_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Mark a running graph as pausing (pause requested). Returns true if the
    /// graph was running and is now pausing, false otherwise. Does NOT terminate
    /// running nodes — the engine checks this state between node executions.
    pub fn request_pause_pending(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1 WHERE id = ?2 AND status = 'running'",
            params![GraphStatus::Pausing.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Transition a graph from pausing to paused. Called by the engine after
    /// the running node completes naturally.
    pub fn complete_pause(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1 WHERE id = ?2 AND status = 'pausing'",
            params![GraphStatus::Paused.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Atomically claim `graph_id` for a run by flipping it to `Running`, but
    /// only if it is not *already* `Running` (B42). Returns `true` when this
    /// call won the claim (the graph was fireable and is now `Running`), `false`
    /// when the graph was already `Running` — i.e. another dispatch is live and
    /// this launch must be treated as a no-op rather than starting a duplicate,
    /// superseding run.
    ///
    /// This is a single-statement compare-and-set, so two dispatches racing to
    /// launch the same graph (the classic autorun-vs-resume check-then-act race:
    /// one reads the graph as `failed`, the other hasn't written `running` yet)
    /// serialize on the connection lock and exactly one wins — the guard is on
    /// the status *transition* itself, not on a separate earlier read.
    pub fn claim_graph_for_run(&self, graph_id: &str, started_at: DateTime<Utc>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs
             SET status = ?1,
                 started_at = COALESCE(?2, started_at)
             WHERE id = ?3 AND status != ?1",
            params![
                GraphStatus::Running.as_str(),
                started_at.timestamp(),
                graph_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Persist (or, with `None`, clear) the queue a run against `graph_id` is
    /// currently drawing from. Called once when a run starts — including a
    /// resumed run, so a failed queue run that gets auto-reset-and-relaunched
    /// re-persists the same queue rather than losing it — and cleared again
    /// only when a run finishes genuinely. See
    /// [`crate::domain::graphs::Graph::active_run_queue_id`].
    pub fn set_graph_active_run_queue(
        &self,
        graph_id: &str,
        queue_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET active_run_queue_id = ?1 WHERE id = ?2",
            params![queue_id, graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Reset a graph back to `Draft` (the status `graph_run` accepts) and clear
    /// `completed_at`, so a `failed` or `completed` graph can be relaunched via
    /// `graph_reset` + `graph_run` instead of being stuck forever.
    pub fn reset_graph_status(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET status = ?1, completed_at = NULL WHERE id = ?2",
            params![GraphStatus::Draft.as_str(), graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Reset a graph spec back to `Pending`, clearing `started_at` and
    /// `completed_at` unconditionally (unlike [`Self::update_graph_spec_status`],
    /// which only overwrites when a new value is given). Used by `graph_reset`.
    ///
    /// `clear_cross_run_attempts` (C19) additionally zeroes the spec's
    /// persisted cross-run attempt counter — but only when the caller passed
    /// this exact spec explicitly. [`Self::reset_graph`] wires that in: an
    /// operator naming a spec by id is the deliberate "I fixed this" signal
    /// decision 6 asks for; a blanket reset of every non-completed spec is
    /// not, so the counter must survive it — otherwise the very relaunch
    /// this budget exists to guard against would silently get a fresh one
    /// every time.
    pub fn reset_graph_spec_status(
        &self,
        spec_id: &str,
        clear_cross_run_attempts: bool,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let updated_at = Utc::now().timestamp_millis();
        let rows = conn.execute(
            "UPDATE graph_specs SET status = ?1, started_at = NULL, completed_at = NULL, spec_start_head = NULL, spec_committed_head = NULL,
                 completed_via = CASE WHEN ?3 THEN NULL ELSE completed_via END,
                 completed_via_reason = CASE WHEN ?3 THEN NULL ELSE completed_via_reason END,
                 completed_via_at = CASE WHEN ?3 THEN NULL ELSE completed_via_at END,
                 cross_run_attempts = CASE WHEN ?3 THEN 0 ELSE cross_run_attempts END,
                 updated_at = ?4
             WHERE id = ?2",
            params![
                GraphSpecStatus::Pending.as_str(),
                spec_id,
                clear_cross_run_attempts,
                updated_at,
            ],
        )?;
        Ok(rows > 0)
    }

    /// C19: the spec's persisted cross-run attempt count — how many separate
    /// graph executions it has failed with a genuine (non-infrastructure)
    /// verdict. `0` for a spec that has never failed this way (including
    /// every pre-migration row). See [`Self::increment_graph_spec_cross_run_attempts`].
    pub fn get_graph_spec_cross_run_attempts(&self, spec_id: &str) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT cross_run_attempts FROM graph_specs WHERE id = ?1",
            params![spec_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Increment `spec_id`'s persisted cross-run attempt count and return the
    /// new value, atomically under the single connection lock so a
    /// concurrent read never observes a torn increment. Called by
    /// `GraphEngine::record_spec_attempt` exactly once per spec-execution
    /// that ends in a genuine (non-infrastructure) `Failed` — never for an
    /// infra failure, and never more than once per attempt.
    pub fn increment_graph_spec_cross_run_attempts(&self, spec_id: &str) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE graph_specs SET cross_run_attempts = cross_run_attempts + 1, updated_at = ?2 WHERE id = ?1",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        conn.query_row(
            "SELECT cross_run_attempts FROM graph_specs WHERE id = ?1",
            params![spec_id],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Administratively transition a standalone spec's status. The transition
    /// is recorded with provenance (completed_via = 'admin') and the given reason.
    pub fn set_spec_admin_status(
        &self,
        spec_id: &str,
        status: GraphSpecStatus,
        reason: &str,
    ) -> Result<SpecAdminStatusOutcome> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        // 1. Check if spec exists
        let mut stmt = conn.prepare("SELECT id, graph_id FROM graph_specs WHERE id = ?1")?;
        let (_, graph_id): (String, Option<String>) = match stmt
            .query_row(params![spec_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()?
        {
            Some(row) => row,
            None => return Ok(SpecAdminStatusOutcome::NotFound),
        };

        // 2. Reject if spec is bound to a graph (not standalone)
        if let Some(graph_id) = graph_id {
            return Ok(SpecAdminStatusOutcome::NotStandalone(graph_id));
        }

        // 3. Check for active graph run
        if let Some(run) = active_graph_run_for_spec_locked(&conn, spec_id)? {
            return Ok(SpecAdminStatusOutcome::ActiveRun {
                graph_id: run.graph_id,
                run_id: run.id,
            });
        }

        // 4. Update the spec with admin status
        let now = Utc::now().timestamp();
        let updated_at = Utc::now().timestamp_millis();
        let (completed_at, completed_via_at) = match status {
            GraphSpecStatus::Pending => (None, None),
            _ => (Some(now), Some(now)),
        };

        conn.execute(
            "UPDATE graph_specs
             SET status = ?1, completed_at = ?2, completed_via = 'admin',
                 completed_via_reason = ?3, completed_via_at = ?4, updated_at = ?6
             WHERE id = ?5",
            params![
                status.as_str(),
                completed_at,
                reason,
                completed_via_at,
                spec_id,
                updated_at,
            ],
        )?;

        Ok(SpecAdminStatusOutcome::Success)
    }

    /// The single state-transition path behind `graph_reset` — resets a graph
    /// (and, without `specs`, every non-completed spec except an
    /// administratively skipped spec) back to `pending` so it can be
    /// relaunched. Shared by the `graph_reset` MCP tool and the scheduler's
    /// auto-reset-and-resume of a `failed` graph on autorun, so there is exactly
    /// one place that knows how to unstick a graph. The returned
    /// `skipped_count` reports administratively skipped specs preserved by a
    /// blanket reset.
    ///
    /// When the graph's last run was against a queue (`active_run_queue_id` is
    /// set), the queue's *members* are what actually need resetting — the
    /// graph's own bound specs are typically empty for a queue run — so they're
    /// folded into the same eligible set as the graph's bound specs, both for
    /// validating an explicit `specs` list and for the "every non-completed"
    /// default. This is the one reset implementation both `graph_reset` and
    /// the scheduler's autorun share; it must not be forked.
    pub fn reset_graph(
        &self,
        graph_id: &str,
        specs: Option<&[String]>,
    ) -> Result<GraphResetOutcome> {
        let Some(lp) = self.get_graph(graph_id)? else {
            return Ok(GraphResetOutcome::NotFound);
        };

        // Ground truth for "is this graph actually busy right now" is the
        // `graph_runs` table, not `lp.status` — a sibling node's
        // `graph_report_blocker` can flip status to `paused` while a
        // different node under the same graph keeps executing (status and a
        // run's lifetime are independent). Resetting underneath that live
        // run is exactly what corrupted the 2026-08-05 incident: the reset
        // killed-and-reset the run's spec while its `execute_node` future
        // was still in flight, so its late completion routed an edge and
        // failed the graph out from under the fresh dispatch this reset then
        // launched. Refuse outright instead — the caller's next move is an
        // informed wait or a deliberate kill, not a race.
        if let Some(run) = self.list_running_graph_runs(graph_id)?.into_iter().next() {
            return Ok(GraphResetOutcome::InFlight {
                run_id: run.id,
                node_id: run.node_id,
                started_at: run.started_at,
            });
        }

        let bound_specs = self.list_graph_specs(graph_id)?;
        let queue_specs: Vec<GraphSpec> = match &lp.active_run_queue_id {
            Some(queue_id) => self
                .list_queue_member_spec_ids(queue_id)?
                .into_iter()
                .filter_map(|spec_id| self.get_graph_spec(&spec_id).transpose())
                .collect::<Result<Vec<_>>>()?,
            None => Vec::new(),
        };
        let eligible_specs: Vec<&GraphSpec> =
            bound_specs.iter().chain(queue_specs.iter()).collect();

        let valid_ids: std::collections::HashSet<&str> =
            eligible_specs.iter().map(|spec| spec.id.as_str()).collect();

        let target_ids: Vec<String> = match specs {
            Some(ids) => {
                for id in ids {
                    if !valid_ids.contains(id.as_str()) {
                        return Ok(GraphResetOutcome::InvalidSpec(id.clone()));
                    }
                }
                ids.to_vec()
            }
            None => eligible_specs
                .iter()
                .filter(|spec| {
                    spec.status != GraphSpecStatus::Completed
                        && !(spec.status == GraphSpecStatus::Skipped
                            && spec.completed_via.as_deref() == Some("admin"))
                })
                .map(|spec| spec.id.clone())
                .collect(),
        };

        // No spec being reset can have a live `running` node-run row left:
        // the guard above already confirmed zero `running` rows exist
        // anywhere under this graph, and every eligible spec's runs are
        // recorded under this same `graph_id` (bound or drawn live from a
        // queue — see `list_graph_runs_for_graph`), so there is nothing left
        // to terminate here.
        //
        // C19: only an explicitly-named `specs` list clears the target
        // spec(s)' cross-run attempt count — see `reset_graph_spec_status`'s
        // doc. A blanket reset (`specs: None`) resets every non-completed
        // spec's status the same as always, but leaves each one's count
        // exactly where it was.
        let clear_cross_run_attempts = specs.is_some();
        for spec_id in &target_ids {
            self.reset_graph_spec_status(spec_id, clear_cross_run_attempts)?;
        }
        self.reset_graph_status(graph_id)?;

        let skipped_count = if specs.is_none() {
            eligible_specs
                .iter()
                .filter(|spec| {
                    spec.status == GraphSpecStatus::Skipped
                        && spec.completed_via.as_deref() == Some("admin")
                })
                .count()
        } else {
            0
        };

        Ok(GraphResetOutcome::Reset {
            spec_count: target_ids.len(),
            skipped_count,
        })
    }

    pub fn insert_graph_spec(&self, spec: &GraphSpec) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, spec_committed_head, spec_end_dirty, spec_end_dirty_paths, completed_via, completed_via_reason, completed_via_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                &spec.id,
                &spec.graph_id,
                &spec.name,
                &spec.description,
                spec.position,
                spec.parallelizable,
                spec.status.as_str(),
                spec.started_at.map(|value| value.timestamp()),
                spec.completed_at.map(|value| value.timestamp()),
                &spec.spec_start_head,
                spec.spec_start_dirty,
                &spec.workdir,
                &spec.spec_committed_head,
                spec.spec_end_dirty,
                spec.spec_end_dirty_paths.as_ref().map(|paths| paths.join("\n")),
                &spec.completed_via,
                &spec.completed_via_reason,
                spec.completed_via_at.map(|value| value.timestamp()),
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(())
    }

    pub fn list_graph_specs(&self, graph_id: &str) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths
             FROM graph_specs WHERE graph_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_spec_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_graph_spec(&self, spec_id: &str) -> Result<Option<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths
             FROM graph_specs WHERE id = ?1",
        )?;

        stmt.query_row(params![spec_id], map_graph_spec_row)
            .optional()
            .map_err(Into::into)
    }

    /// A single spec's own graph (nodes/edges), resolved by spec id alone —
    /// independent of whether the spec is bound to a graph (`graph_specs.graph_id`)
    /// or a standalone queue member. Lets the graph engine drive a queue spec
    /// through the same lookup path as a bound spec (see `graph_engine::run`).
    pub fn get_graph_spec_details(&self, spec_id: &str) -> Result<Option<GraphSpecDetails>> {
        let Some(spec) = self.get_graph_spec(spec_id)? else {
            return Ok(None);
        };
        let nodes = self.list_graph_nodes(spec_id)?;
        let edges = self.list_graph_edges(spec_id)?;
        Ok(Some(GraphSpecDetails { spec, nodes, edges }))
    }

    /// Standalone specs, i.e. the backlog: specs not (yet) assigned to any
    /// graph, optionally filtered by their `workdir` tag and/or status.
    /// `unassigned_only` additionally filters to `graph_id IS NULL` — set it
    /// to `false` to see every spec regardless of graph assignment.
    pub fn list_specs(
        &self,
        workdir: Option<&str>,
        status: Option<GraphSpecStatus>,
        unassigned_only: bool,
    ) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths
             FROM graph_specs
             WHERE (?1 IS NULL OR workdir = ?1)
               AND (?2 IS NULL OR status = ?2)
               AND (?3 = 0 OR graph_id IS NULL)
             ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(
            params![
                workdir,
                status.map(GraphSpecStatus::as_str),
                unassigned_only
            ],
            map_graph_spec_row,
        )?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Return the newest write for backlog (unassigned) specs in the given
    /// workdir, independent of the capped list used to render the Knowledge
    /// face. Mirrors [`Database::max_project_knowledge_updated_at`].
    pub fn max_backlog_updated_at(&self, workdir: Option<&str>) -> Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT MAX(updated_at) FROM graph_specs
             WHERE (?1 IS NULL OR workdir = ?1) AND graph_id IS NULL",
            rusqlite::params![workdir],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    /// Update a standalone/backlog spec's name, description, and/or workdir
    /// tag. Unlike [`Self::update_graph_spec_details`] (position/parallelizable,
    /// used by `graph_update_spec`), this is for `spec_update` and never
    /// touches graph assignment or ordering.
    pub fn update_spec_tag_details(
        &self,
        spec_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        workdir: Option<Option<&str>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let updated_at = Utc::now().timestamp_millis();
        let rows = conn.execute(
            "UPDATE graph_specs
             SET name = COALESCE(?1, name),
                 description = COALESCE(?2, description),
                 workdir = CASE WHEN ?3 IS NULL THEN workdir ELSE ?4 END,
                 updated_at = ?6
             WHERE id = ?5",
            params![
                name,
                description,
                workdir.map(|_| 1),
                workdir.flatten(),
                spec_id,
                updated_at,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn list_all_specs_for_conversion(&self) -> Result<Vec<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, name, description, position, parallelizable, status, started_at, completed_at, spec_start_head, spec_start_dirty, workdir, completed_via, completed_via_reason, completed_via_at, spec_committed_head, spec_end_dirty, spec_end_dirty_paths
             FROM graph_specs
             ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map([], map_graph_spec_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn update_spec_description_if_not_running(
        &self,
        spec_id: &str,
        new_description: &str,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET description = ?1, updated_at = ?3 WHERE id = ?2 AND status != 'running'",
            params![new_description, spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    /// Delete a spec outright. Callers must enforce the graph-binding guard
    /// (see `spec_delete`'s handler) before calling this — this function
    /// performs no such check itself.
    pub fn delete_graph_spec(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM graph_specs WHERE id = ?1", params![spec_id])?;
        Ok(rows > 0)
    }

    /// Unbind a spec from its graph (set graph_id = NULL), making it standalone.
    /// Does NOT delete the spec row — execution history is preserved.
    /// Returns true if a row was updated, false if spec_id not found.
    pub fn unbind_graph_spec(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET graph_id = NULL, updated_at = ?2 WHERE id = ?1 AND graph_id IS NOT NULL",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    /// Record the workdir's git HEAD at the moment a spec starts running.
    /// Called once per spec (not per node) — see [`crate::graph_engine`]'s
    /// `{{spec_start_head}}` placeholder. `head = None` means the workdir
    /// isn't a git repo; the column is cleared rather than left stale.
    #[allow(dead_code)]
    pub fn set_graph_spec_start_head(&self, spec_id: &str, head: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_start_head = ?1, updated_at = ?3 WHERE id = ?2",
            params![head, spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    pub fn set_graph_spec_start_state(
        &self,
        spec_id: &str,
        head: Option<&str>,
        dirty: Option<i64>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_start_head = ?1, spec_start_dirty = ?2, updated_at = ?3 WHERE id = ?4",
            params![head, dirty, Utc::now().timestamp_millis(), spec_id],
        )?;
        Ok(rows > 0)
    }

    /// CM29: the workdir's git-dirty state captured once, at the moment
    /// this spec's attempt actually ends (Completed/Failed/Interrupted) —
    /// same lifecycle as `set_graph_spec_start_state`, the other boundary.
    /// `paths` is truncated to 20 entries by the caller before this is
    /// called; stored newline-joined.
    pub fn set_graph_spec_end_state(
        &self,
        spec_id: &str,
        dirty: Option<i64>,
        paths: &[String],
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let joined = (!paths.is_empty()).then(|| paths.join("\n"));
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_end_dirty = ?1, spec_end_dirty_paths = ?2, updated_at = ?3 WHERE id = ?4",
            params![dirty, joined, Utc::now().timestamp_millis(), spec_id],
        )?;
        Ok(rows > 0)
    }

    /// CM29: the most-recently-touched spec that ended without completing,
    /// for `workdir` — either a graph-bound spec whose graph's `workdir`
    /// matches, or a standalone spec whose own `workdir` tag matches.
    /// Excludes `exclude_spec_id` (the spec about to be dispatched next) so
    /// a spec that is itself `Interrupted` and about to be legitimately
    /// resumed is never mistaken for "the previous spec" that left the mess
    /// (CM29 guideline). `pending`/`running` are never candidates — those
    /// are either not-yet-run or still in flight, never "previous".
    pub fn find_dirty_predecessor_spec(
        &self,
        workdir: &str,
        exclude_spec_id: Option<&str>,
    ) -> Result<Option<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths
             FROM graph_specs gs
             LEFT JOIN graphs g ON g.id = gs.graph_id
             WHERE (g.workdir = ?1 OR gs.workdir = ?1)
               AND gs.status IN ('failed', 'interrupted', 'skipped')
               AND (?2 IS NULL OR gs.id != ?2)
             ORDER BY gs.updated_at DESC
             LIMIT 1",
            params![workdir, exclude_spec_id],
            map_graph_spec_row,
        )
        .optional()
        .map_err(Into::into)
    }

    /// CM29: specs reconciliation just marked `Interrupted` that haven't had
    /// their end-dirty state captured yet — `spec_end_dirty IS NULL` is the
    /// idempotency guard (a spec resumed and re-ended later overwrites this
    /// again through the normal `run_spec` path, so this only ever picks up
    /// genuinely-uncaptured rows, including ones a previous boot's capture
    /// pass somehow missed). Returns each spec paired with its resolved
    /// workdir (graph's `workdir`, or its own `workdir` tag), `None` when
    /// neither is known.
    pub fn list_interrupted_specs_missing_end_dirty(
        &self,
    ) -> Result<Vec<(GraphSpec, Option<String>)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT gs.id, gs.graph_id, gs.name, gs.description, gs.position, gs.parallelizable, gs.status, gs.started_at, gs.completed_at, gs.spec_start_head, gs.spec_start_dirty, gs.workdir, gs.completed_via, gs.completed_via_reason, gs.completed_via_at, gs.spec_committed_head, gs.spec_end_dirty, gs.spec_end_dirty_paths, g.workdir
             FROM graph_specs gs
             LEFT JOIN graphs g ON g.id = gs.graph_id
             WHERE gs.status = 'interrupted' AND gs.spec_end_dirty IS NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let spec = map_graph_spec_row(row)?;
            let graph_workdir: Option<String> = row.get(18)?;
            Ok((spec, graph_workdir))
        })?;
        rows.map(|r| {
            r.map(|(spec, graph_workdir)| {
                let resolved = graph_workdir.or_else(|| spec.workdir.clone());
                (spec, resolved)
            })
            .map_err(Into::into)
        })
        .collect()
    }

    /// Record the workdir's git HEAD immediately after a `commit_rights:
    /// true` node's own execution actually moved it (C15) — see
    /// [`crate::graph_engine`]'s `{{spec_committed_head}}` placeholder.
    /// Unlike `spec_start_head`, which is captured once and answers "has
    /// anything been committed since this spec began", this answers "did
    /// *this run's own committer* land a commit" — the distinction a
    /// concurrent commit from outside this run (another agent, a human
    /// sharing the worktree) would otherwise slip past. Overwritten every
    /// time the committer node visits and moves HEAD again (e.g. a
    /// review/retry cycle that re-commits), so it always reflects the
    /// latest commit this run itself produced.
    pub fn set_graph_spec_committed_head(&self, spec_id: &str, head: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET spec_committed_head = ?1, updated_at = ?3 WHERE id = ?2",
            params![head, spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_spec_details(
        &self,
        spec_id: &str,
        name: Option<&str>,
        description: Option<&str>,
        position: Option<i64>,
        parallelizable: Option<bool>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs
             SET name = COALESCE(?1, name),
                 description = COALESCE(?2, description),
                 position = COALESCE(?3, position),
                 parallelizable = COALESCE(?4, parallelizable),
                 updated_at = ?6
             WHERE id = ?5",
            params![
                name,
                description,
                position,
                parallelizable,
                spec_id,
                Utc::now().timestamp_millis()
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_spec_status(
        &self,
        spec_id: &str,
        status: GraphSpecStatus,
        started_at: Option<DateTime<Utc>>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs
             SET status = ?1,
                 started_at = COALESCE(?2, started_at),
                 completed_at = COALESCE(?3, completed_at),
                 updated_at = ?5
             WHERE id = ?4",
            params![
                status.as_str(),
                started_at.map(|value| value.timestamp()),
                completed_at.map(|value| value.timestamp()),
                spec_id,
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_graph_node(&self, node: &GraphNode) -> Result<()> {
        validate_single_target(node.spec_id.as_deref(), node.graph_id.as_deref())?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
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
        Ok(())
    }

    pub fn list_graph_nodes(&self, spec_id: &str) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes WHERE spec_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_graph_node_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Nodes belonging to a graph's top-level graph (as opposed to any one
    /// spec's graph).
    pub fn list_graph_nodes_for_graph(&self, graph_id: &str) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes WHERE graph_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_node_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Every graph node in the database, across every graph and spec — unlike
    /// [`list_graph_nodes`]/[`list_graph_nodes_for_graph`], which scope to one
    /// graph. Used by the `graph_audit_node_configs` MCP tool to find nodes
    /// already carrying a config key their kind will never read (e.g. a
    /// `prompt` key on an agent node — see
    /// `daemon::handler::validate_node_config`), which write-time validation
    /// alone can't catch for nodes created before it existed.
    pub fn list_all_graph_nodes(&self) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], map_graph_node_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_graph_node(&self, node_id: &str) -> Result<Option<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes WHERE id = ?1",
        )?;
        stmt.query_row(params![node_id], map_graph_node_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_graph_node_details(
        &self,
        node_id: &str,
        name: Option<&str>,
        kind: Option<GraphNodeKind>,
        config: Option<&Value>,
        position: Option<i64>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_nodes
             SET name = COALESCE(?1, name),
                 kind = COALESCE(?2, kind),
                 config = COALESCE(?3, config),
                 position = COALESCE(?4, position)
             WHERE id = ?5",
            params![
                name,
                kind.map(|value| value.as_str()),
                config.map(serde_json::to_string).transpose()?,
                position,
                node_id,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn insert_graph_edge(&self, edge: &GraphEdge) -> Result<()> {
        validate_single_target(edge.spec_id.as_deref(), edge.graph_id.as_deref())?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition, route)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                &edge.id,
                &edge.spec_id,
                &edge.graph_id,
                &edge.from_node,
                &edge.to_node,
                edge.condition.as_str(),
                edge.condition.route_label(),
            ],
        )?;
        Ok(())
    }

    /// Insert a node together with any edges wiring it, in one transaction, so
    /// a copy (`graph_copy_node`) can never leave a node half-wired. Every edge
    /// and the node must target the same single graph (spec or graph).
    pub fn insert_node_with_edges(&self, node: &GraphNode, edges: &[GraphEdge]) -> Result<()> {
        validate_single_target(node.spec_id.as_deref(), node.graph_id.as_deref())?;
        for edge in edges {
            validate_single_target(edge.spec_id.as_deref(), edge.graph_id.as_deref())?;
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
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
        for edge in edges {
            tx.execute(
                "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition, route)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &edge.id,
                    &edge.spec_id,
                    &edge.graph_id,
                    &edge.from_node,
                    &edge.to_node,
                    edge.condition.as_str(),
                    edge.condition.route_label(),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_graph_edges(&self, spec_id: &str) -> Result<Vec<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, from_node, to_node, condition, route
             FROM graph_edges WHERE spec_id = ?1 ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_graph_edge_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Edges belonging to a graph's top-level graph (as opposed to any one
    /// spec's graph).
    pub fn list_graph_edges_for_graph(&self, graph_id: &str) -> Result<Vec<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, from_node, to_node, condition, route
             FROM graph_edges WHERE graph_id = ?1 ORDER BY rowid ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_edge_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_graph_edge(&self, edge_id: &str) -> Result<Option<GraphEdge>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, from_node, to_node, condition, route
             FROM graph_edges WHERE id = ?1",
        )?;
        stmt.query_row(params![edge_id], map_graph_edge_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_graph_edge_condition(
        &self,
        edge_id: &str,
        condition: &GraphEdgeCondition,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_edges
             SET condition = ?1,
                 route = ?2
             WHERE id = ?3",
            params![condition.as_str(), condition.route_label(), edge_id],
        )?;
        Ok(rows > 0)
    }

    /// Repoint an existing edge at a new target node — used to rewire a
    /// router route to a different destination without dropping and
    /// re-creating the edge (which would lose its id).
    pub fn update_graph_edge_target(&self, edge_id: &str, to_node: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_edges SET to_node = ?1 WHERE id = ?2",
            params![to_node, edge_id],
        )?;
        Ok(rows > 0)
    }

    /// Drop a single edge — used when a router's routes editor removes a
    /// declared route that already had an edge wired to it, so the edge
    /// never outlives the route it named (see
    /// [`crate::domain::graphs::validate_router_edges_declared`]).
    pub fn delete_graph_edge(&self, edge_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM graph_edges WHERE id = ?1", params![edge_id])?;
        Ok(rows > 0)
    }

    pub fn insert_graph_run(&self, run: &GraphNodeRun) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                &run.id,
                &run.graph_id,
                &run.spec_id,
                &run.node_id,
                run.status.as_str(),
                run.input
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.output
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.started_at.timestamp(),
                run.completed_at.map(|value| value.timestamp()),
                run.iteration,
                run.pid,
                &run.boot_id,
                &run.session_id,
                &run.executed_platform,
                &run.executed_model,
            ],
        )?;
        Ok(())
    }

    /// Record the OS process-group leader spawned for `run_id`'s node
    /// execution, and the boot it was spawned under. Called right after a
    /// successful `spawn()` — before that, the run row (inserted by the
    /// caller before execution starts) has `pid = NULL`, meaning "no live
    /// process to kill" (e.g. a gate node, or an agent/check node that
    /// hasn't finished spawning yet).
    pub fn set_graph_run_pid(&self, run_id: &str, pid: i64, boot_id: Option<&str>) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs SET pid = ?1, boot_id = ?2 WHERE id = ?3",
            params![pid, boot_id, run_id],
        )?;
        Ok(rows > 0)
    }

    /// Record the harness session id captured for a node run (RS1), so the
    /// session can be resumed later (RS2/RS3).
    pub fn set_graph_run_session_id(&self, run_id: &str, session_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs SET session_id = ?1 WHERE id = ?2",
            params![session_id, run_id],
        )?;
        Ok(rows > 0)
    }

    /// RS2/CM26: the earlier run this run's session was continued from, if any.
    /// A resumed run's own row carries the SAME session_id it continued
    /// (`run_agent_process` writes it back verbatim, never a fresh uuid — see
    /// its RS2 comment), while a cold-started run always mints a brand-new
    /// session id (or none) that can never collide with an earlier row's. So a
    /// match here can only mean this run resumed that earlier one. Ordered by
    /// `rowid` (insertion order) rather than `started_at` (only second-
    /// granularity) to disambiguate deterministically when two runs on the same
    /// node start within the same second. Display-only — never consulted by the
    /// engine's own resume logic, which uses `resumable_sessions` in-memory.
    pub fn find_resumed_from_run_id(
        &self,
        run_id: &str,
        node_id: &str,
        session_id: &str,
    ) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let found: Option<String> = conn
            .query_row(
                "SELECT id FROM graph_runs
                 WHERE session_id = ?1 AND node_id = ?2 AND id != ?3
                   AND rowid < (SELECT rowid FROM graph_runs WHERE id = ?3)
                 ORDER BY rowid DESC LIMIT 1",
                params![session_id, node_id, run_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found)
    }

    /// The active (`running`) node run for `spec_id`, if any. Mirrors
    /// [`Self::get_active_graph_run_for_node`] but scoped to a whole spec —
    /// used by `graph_reset`, which resets a spec wholesale rather than one
    /// node at a time.
    pub fn get_active_graph_run_for_spec(&self, spec_id: &str) -> Result<Option<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        active_graph_run_for_spec_locked(&conn, spec_id).map_err(Into::into)
    }

    /// Every node run still `running` across every graph — used at daemon
    /// shutdown to terminate every process this boot owns before exiting.
    pub fn list_all_running_graph_runs(&self) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE status = 'running'",
        )?;
        let rows = stmt.query_map(params![], map_graph_run_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn list_graph_runs_for_spec(&self, spec_id: &str) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE spec_id = ?1 ORDER BY started_at ASC, iteration ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_graph_run_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// All node runs recorded against `graph_id`, regardless of whether the
    /// spec they belong to is bound (`graph_specs.graph_id`) or was picked up
    /// live from a queue (queue members always keep `graph_id: None` on their
    /// own row — see `GraphEngine::run_graph`). `graph_runs.graph_id` is set on
    /// every insert either way, so this is the only reliable way to find a
    /// queue-driven graph's current/recent activity without a queue id in hand.
    pub fn list_graph_runs_for_graph(&self, graph_id: &str) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE graph_id = ?1 ORDER BY started_at ASC, iteration ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_run_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Page through a graph's node runs, most-recent-first, optionally
    /// narrowed to one spec and/or one node — the query a failure
    /// investigation needs (`graph_node_runs_list`): find what ran, in what
    /// order, without knowing a node id ahead of time. Unlike
    /// [`Self::list_graph_runs_for_graph`] (oldest-first, unbounded — built for
    /// the engine replaying a whole run), this is bounded by `limit`/`offset`
    /// so a graph with hundreds of runs stays a usable response.
    ///
    /// `graph_id` alone rides `idx_graph_runs_graph_started(graph_id,
    /// started_at DESC)` directly, matching this query's default order.
    /// Adding `spec_id`/`node_id` applies as a residual filter on top of that
    /// same index scan — both columns already carry their own index
    /// (`idx_graph_runs_spec_started`, `idx_graph_runs_node_iteration`) for
    /// other call sites, but the scan here is bounded by `graph_id` first
    /// either way, so no additional composite index is needed.
    pub fn list_graph_node_runs(
        &self,
        graph_id: &str,
        spec_id: Option<&str>,
        node_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let mut sql = String::from(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE graph_id = ?",
        );
        let mut query_params: Vec<&dyn rusqlite::ToSql> = vec![&graph_id];
        if let Some(spec_id) = spec_id.as_ref() {
            sql.push_str(" AND spec_id = ?");
            query_params.push(spec_id);
        }
        if let Some(node_id) = node_id.as_ref() {
            sql.push_str(" AND node_id = ?");
            query_params.push(node_id);
        }
        sql.push_str(" ORDER BY started_at DESC, iteration DESC LIMIT ? OFFSET ?");
        query_params.push(&limit);
        query_params.push(&offset);

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(query_params.as_slice(), map_graph_run_row)?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn count_graph_node_runs_filtered(
        &self,
        graph_id: &str,
        spec_id: Option<&str>,
        node_id: Option<&str>,
    ) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let mut sql = String::from("SELECT COUNT(*) FROM graph_runs WHERE graph_id = ?");
        let mut query_params: Vec<&dyn rusqlite::ToSql> = vec![&graph_id];
        if let Some(spec_id) = spec_id.as_ref() {
            sql.push_str(" AND spec_id = ?");
            query_params.push(spec_id);
        }
        if let Some(node_id) = node_id.as_ref() {
            sql.push_str(" AND node_id = ?");
            query_params.push(node_id);
        }

        conn.query_row(&sql, query_params.as_slice(), |row| row.get(0))
            .map_err(Into::into)
    }

    /// CB43: platform+model pairs this installation has actually run since
    /// `since`, across every run table (graph node runs, hook runs,
    /// background agent runs, subagent runs). One lock acquisition; each
    /// branch is bounded by its `started_at` predicate. Ordered by most
    /// recent use, capped at `limit`. Rows with no recorded pair
    /// (`executed_platform IS NULL` — every run predating CB43) are omitted,
    /// never guessed at. Never reads a node's current config.
    pub fn list_recent_platform_model_usage(
        &self,
        since: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<RecentModelUsage>> {
        use std::collections::HashMap;

        struct Sample {
            started_at: DateTime<Utc>,
            status: String,
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        let since_epoch = since.timestamp();
        let mut grouped: HashMap<(String, Option<String>), Vec<Sample>> = HashMap::new();

        // graph_runs + graph_completion_hook_runs store epoch INTEGERs.
        for table in ["graph_runs", "graph_completion_hook_runs"] {
            let sql = format!(
                "SELECT executed_platform, executed_model, status, started_at FROM {table} \
                 WHERE executed_platform IS NOT NULL AND started_at >= ?1"
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params![since_epoch], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?;
            for row in rows {
                let (platform, model, status, started_at) = row?;
                let started_at = from_timestamp(started_at)?;
                grouped
                    .entry((platform, model))
                    .or_default()
                    .push(Sample { started_at, status });
            }
        }

        // runs + subagent_runs store RFC3339 TEXT timestamps.
        {
            let mut stmt = conn.prepare(
                "SELECT executed_platform, executed_model, status, started_at FROM runs \
                 WHERE executed_platform IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (platform, model, status, started_at): (
                    String,
                    Option<String>,
                    String,
                    String,
                ) = row?;
                let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(&started_at)
                    .map(|dt| dt.with_timezone(&Utc))
                else {
                    continue;
                };
                if started_at < since {
                    continue;
                }
                grouped
                    .entry((platform, model))
                    .or_default()
                    .push(Sample { started_at, status });
            }
        }

        // CB43: `subagent_runs.platform`/`model` are the pair resolved at
        // dispatch (see schema comment); `platform` is NOT NULL by schema so
        // every row in the window counts. NULL model = the CLI's default.
        {
            let mut stmt =
                conn.prepare("SELECT platform, model, status, started_at FROM subagent_runs")?;
            let rows = stmt.query_map(params![], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            for row in rows {
                let (platform, model, status, started_at): (
                    String,
                    Option<String>,
                    String,
                    String,
                ) = row?;
                let Ok(started_at) = chrono::DateTime::parse_from_rfc3339(&started_at)
                    .map(|dt| dt.with_timezone(&Utc))
                else {
                    continue;
                };
                if started_at < since {
                    continue;
                }
                grouped
                    .entry((platform, model))
                    .or_default()
                    .push(Sample { started_at, status });
            }
        }

        let mut out: Vec<RecentModelUsage> = grouped
            .into_iter()
            .map(|((platform, model), mut samples)| {
                samples.sort_by_key(|sample| std::cmp::Reverse(sample.started_at));
                let last = &samples[0];
                RecentModelUsage {
                    last_run: last.started_at,
                    count: samples.len() as i64,
                    last_outcome: last.status.clone(),
                    platform,
                    model,
                }
            })
            .collect();
        out.sort_by_key(|usage| std::cmp::Reverse(usage.last_run));
        out.truncate(limit.max(0) as usize);
        Ok(out)
    }

    /// Return every recent run event with the identity that produced it.
    /// This is the installation-wide source data for `agent_probe_recent`;
    /// callers group it by platform and model while retaining attribution.
    pub fn list_recent_platform_model_usage_sources(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<RecentPairSource>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let since_epoch = since.timestamp();
        let mut out = Vec::new();

        let mut stmt = conn.prepare(
            "SELECT gr.executed_platform, gr.executed_model, gr.status, gr.started_at,
                    g.id, g.name, gn.id, gn.name
             FROM graph_runs gr
             JOIN graphs g ON g.id = gr.graph_id
             JOIN graph_nodes gn ON gn.id = gr.node_id
             WHERE gr.executed_platform IS NOT NULL AND gr.started_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since_epoch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, graph_id, graph_name, node_id, node_name) =
                row?;
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at: from_timestamp(started_at)?,
                source: RecentPairSourceKind::GraphNode {
                    graph_id,
                    graph_name,
                    node_id,
                    node_name,
                },
            });
        }

        let mut stmt = conn.prepare(
            "SELECT h.executed_platform, h.executed_model, h.status, h.started_at,
                    g.id, g.name, h.event, h.hook_index
             FROM graph_completion_hook_runs h
             JOIN graphs g ON g.id = h.graph_id
             WHERE h.executed_platform IS NOT NULL AND h.started_at >= ?1",
        )?;
        let rows = stmt.query_map(params![since_epoch], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, graph_id, graph_name, event, hook_index) =
                row?;
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at: from_timestamp(started_at)?,
                source: RecentPairSourceKind::CompletionHook {
                    graph_id,
                    graph_name,
                    event,
                    hook_index,
                },
            });
        }

        let mut stmt = conn.prepare(
            "SELECT r.executed_platform, r.executed_model, r.status, r.started_at,
                    r.background_agent_id, a.prompt
             FROM runs r JOIN agents a ON a.id = r.background_agent_id
             WHERE r.executed_platform IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, agent_id, prompt) = row?;
            let Ok(started_at) =
                DateTime::parse_from_rfc3339(&started_at).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if started_at < since {
                continue;
            }
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at,
                source: RecentPairSourceKind::BackgroundAgent { agent_id, prompt },
            });
        }

        let mut stmt =
            conn.prepare("SELECT platform, model, status, started_at, workdir FROM subagent_runs")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        for row in rows {
            let (platform, model, status, started_at, workdir) = row?;
            let Ok(started_at) =
                DateTime::parse_from_rfc3339(&started_at).map(|dt| dt.with_timezone(&Utc))
            else {
                continue;
            };
            if started_at < since {
                continue;
            }
            out.push(RecentPairSource {
                platform,
                model,
                status,
                started_at,
                source: RecentPairSourceKind::SubagentSpawn { workdir },
            });
        }
        Ok(out)
    }

    /// Most recent `graph_runs.started_at` per graph, across every graph in a
    /// single query — the sidebar's "last activity" signal. Unlike
    /// [`Self::list_graph_specs`] (a graph's own bound specs, empty for a
    /// queue-driven run whose specs live on the queue instead), this reads
    /// `graph_runs.graph_id`, which is set on every insert regardless of how
    /// the spec was bound (see [`Self::list_graph_runs_for_graph`]), so it
    /// reflects real execution for every graph kind. A graph absent from the
    /// returned map has never recorded a run.
    pub fn list_graph_last_run_times(&self) -> Result<HashMap<String, DateTime<Utc>>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt =
            conn.prepare("SELECT graph_id, MAX(started_at) FROM graph_runs GROUP BY graph_id")?;
        let rows = stmt.query_map(params![], |row| {
            let graph_id: String = row.get(0)?;
            let started_at: i64 = row.get(1)?;
            Ok((graph_id, started_at))
        })?;
        let mut result = HashMap::new();
        for row in rows {
            let (graph_id, started_at) = row?;
            result.insert(graph_id, from_timestamp(started_at)?);
        }
        Ok(result)
    }

    pub fn get_graph_run(&self, run_id: &str) -> Result<Option<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE id = ?1",
        )?;

        stmt.query_row(params![run_id], map_graph_run_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_active_graph_run_for_node(&self, node_id: &str) -> Result<Option<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs
             WHERE node_id = ?1 AND status = 'running'
             ORDER BY started_at DESC
             LIMIT 1",
        )?;

        stmt.query_row(params![node_id], map_graph_run_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_graph_run_result(
        &self,
        run_id: &str,
        status: GraphRunStatus,
        output: Option<&Value>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs
             SET status = ?1,
                 output = COALESCE(?2, output),
                 completed_at = COALESCE(?3, completed_at),
                 pid = NULL
             WHERE id = ?4",
            params![
                status.as_str(),
                output.map(serde_json::to_string).transpose()?,
                completed_at.map(|value| value.timestamp()),
                run_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Mark a running node run as interrupted by an operator. Does NOT kill
    /// the process — the caller is responsible for termination.
    pub fn interrupt_graph_run(&self, run_id: &str, reason: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_runs
             SET status = ?1,
                 output = ?2,
                 completed_at = ?3,
                 pid = NULL
             WHERE id = ?4 AND status = 'running'",
            params![
                GraphRunStatus::Interrupted.as_str(),
                serde_json::to_string(&serde_json::json!({
                    "interrupted": true,
                    "reason": reason
                }))?,
                chrono::Utc::now().timestamp(),
                run_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// CB31: flag every run of `node_ids` under `spec_id` at `iteration` as
    /// `paused_through` — the node(s) finalized with their own verdict while a
    /// wait-for-completion `graph_pause` was pending. A single node has one such
    /// row; an ensemble step has one per member plus its join. The flag tells
    /// [`Self::last_spec_node_run_was_operator_paused`] that `graph_continue`
    /// re-executing this cursor must reuse the same iteration number rather
    /// than spend a fresh one. Returns the number of rows flagged.
    pub fn mark_spec_node_runs_paused_through(
        &self,
        spec_id: &str,
        node_ids: &[String],
        iteration: i64,
    ) -> Result<usize> {
        if node_ids.is_empty() {
            return Ok(0);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let placeholders = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "UPDATE graph_runs SET paused_through = 1
             WHERE spec_id = ?1 AND iteration = ?2 AND node_id IN ({placeholders})"
        );
        let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> =
            vec![Box::new(spec_id.to_string()), Box::new(iteration)];
        for id in node_ids {
            sql_params.push(Box::new(id.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| b.as_ref()).collect();
        let rows = conn.execute(&sql, refs.as_slice())?;
        Ok(rows)
    }

    /// CB31: whether the most recent run among `node_ids` under `spec_id` was
    /// ended by an operator rather than by the node's own work — recorded
    /// `Interrupted` (explicit `graph_pause(interrupt: true)`), or finalized
    /// with its own verdict while a wait-for-completion pause was pending
    /// (`paused_through`). The engine consults this before incrementing the
    /// per-node iteration counter on a resumed dispatch: an operator pause or
    /// interrupt is not a node attempt and must not consume one of the node's
    /// `DEFAULT_MAX_ITERATIONS_PER_NODE`.
    pub fn last_spec_node_run_was_operator_paused(
        &self,
        spec_id: &str,
        node_ids: &[String],
    ) -> Result<bool> {
        if node_ids.is_empty() {
            return Ok(false);
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let placeholders = vec!["?"; node_ids.len()].join(",");
        let sql = format!(
            "SELECT status, paused_through FROM graph_runs
             WHERE spec_id = ?1 AND node_id IN ({placeholders})
             ORDER BY started_at DESC, rowid DESC LIMIT 1"
        );
        let mut sql_params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(spec_id.to_string())];
        for id in node_ids {
            sql_params.push(Box::new(id.clone()));
        }
        let refs: Vec<&dyn rusqlite::ToSql> = sql_params.iter().map(|b| b.as_ref()).collect();
        let row = conn
            .query_row(&sql, refs.as_slice(), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })
            .optional()?;
        Ok(match row {
            Some((status, paused_through)) => {
                GraphRunStatus::from_str(&status) == GraphRunStatus::Interrupted
                    || paused_through != 0
            }
            None => false,
        })
    }

    /// CT3: append one live-output chunk for a running check node. Called
    /// from the engine's stdout/stderr reader tasks; `stream` must be
    /// `stdout` or `stderr` (enforced by the table CHECK constraint).
    pub fn append_graph_run_output(&self, run_id: &str, stream: &str, chunk: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_run_output (run_id, stream, chunk) VALUES (?1, ?2, ?3)",
            params![run_id, stream, chunk],
        )?;
        Ok(())
    }

    /// CT3: live tail for the tail dialog. Reads each stream's chunks
    /// newest-first and stops once it has [`TAIL_MAX_BYTES`] of that stream,
    /// so a node that has emitted gigabytes still costs only a few dozen row
    /// reads per poll (NFR: following a very chatty node must not block the
    /// TUI or consume unbounded memory). Keeps the last `max_lines` lines of
    /// each stream; when neither stream has any chunks (after completion when
    /// only the snapshot remains, or a zero-output hang) falls back to the
    /// `graph_runs.stdout_tail`/`stderr_tail` snapshot columns.
    pub fn get_graph_run_tail(&self, run_id: &str, max_lines: usize) -> Result<(String, String)> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;

        // Pull one stream's trailing chunks in reverse insertion order,
        // stopping as soon as we hold enough bytes to satisfy the byte cap
        // that `take_last_bytes` would apply anyway. Bounds the scan to
        // O(TAIL_MAX_BYTES) regardless of how much total output exists.
        let read_stream_tail = |stream: &str| -> Result<(String, bool)> {
            let mut stmt = conn.prepare(
                "SELECT chunk FROM graph_run_output
                 WHERE run_id = ?1 AND stream = ?2
                 ORDER BY id DESC",
            )?;
            let mut rows = stmt.query(params![run_id, stream])?;
            let mut parts: Vec<String> = Vec::new();
            let mut bytes = 0usize;
            let mut any = false;
            while let Some(row) = rows.next()? {
                any = true;
                let chunk: String = row.get(0)?;
                bytes += chunk.len();
                parts.push(chunk);
                if bytes >= TAIL_MAX_BYTES {
                    break;
                }
            }
            parts.reverse();
            Ok((parts.concat(), any))
        };

        let (stdout_buf, any_stdout) = read_stream_tail("stdout")?;
        let (stderr_buf, any_stderr) = read_stream_tail("stderr")?;

        if !any_stdout && !any_stderr {
            let (snap_out, snap_err): (Option<String>, Option<String>) = conn
                .query_row(
                    "SELECT stdout_tail, stderr_tail FROM graph_runs WHERE id = ?1",
                    params![run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap_or((None, None));
            return Ok((
                take_last_lines(snap_out.as_deref().unwrap_or(""), max_lines),
                take_last_lines(snap_err.as_deref().unwrap_or(""), max_lines),
            ));
        }
        Ok((
            take_last_lines(take_last_bytes(&stdout_buf), max_lines),
            take_last_lines(take_last_bytes(&stderr_buf), max_lines),
        ))
    }

    /// CT3: cache the final truncated per-stream tails on the run row at
    /// completion, so the dialog can show post-completion output without
    /// scanning the chunk table.
    pub fn set_graph_run_tail_snapshot(
        &self,
        run_id: &str,
        stdout_tail: Option<&str>,
        stderr_tail: Option<&str>,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE graph_runs SET stdout_tail = ?1, stderr_tail = ?2 WHERE id = ?3",
            params![stdout_tail, stderr_tail, run_id],
        )?;
        Ok(())
    }

    /// Record one firing of a graph's `on_completed` hook (N2), started as
    /// `Running` before the process is spawned — mirrors [`Self::insert_graph_run`]'s
    /// pattern of a row that exists before the child does, so a crash mid-spawn
    /// still leaves a `Running` row behind rather than nothing.
    pub fn insert_graph_completion_hook_run(&self, run: &GraphCompletionHookRun) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_completion_hook_runs (id, graph_id, status, output, summary, started_at, completed_at, pid, boot_id, event, hook_index, executed_platform, executed_model)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                &run.id,
                &run.graph_id,
                run.status.as_str(),
                run.output.as_ref().map(serde_json::to_string).transpose()?,
                &run.summary,
                run.started_at.timestamp(),
                run.completed_at.map(|value| value.timestamp()),
                run.pid,
                &run.boot_id,
                run.event.as_str(),
                run.hook_index,
                &run.executed_platform,
                &run.executed_model,
            ],
        )?;
        Ok(())
    }

    /// Same B12 treatment as [`Self::set_graph_run_pid`]: record the spawned
    /// process-group leader so an abnormal end can `killpg` it.
    pub fn set_graph_completion_hook_run_pid(
        &self,
        run_id: &str,
        pid: i64,
        boot_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_completion_hook_runs SET pid = ?1, boot_id = ?2 WHERE id = ?3",
            params![pid, boot_id, run_id],
        )?;
        Ok(rows > 0)
    }

    pub fn update_graph_completion_hook_run_result(
        &self,
        run_id: &str,
        status: GraphRunStatus,
        output: Option<&Value>,
        summary: Option<&str>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_completion_hook_runs
             SET status = ?1,
                 output = COALESCE(?2, output),
                 summary = COALESCE(?3, summary),
                 completed_at = COALESCE(?4, completed_at),
                 pid = NULL
             WHERE id = ?5",
            params![
                status.as_str(),
                output.map(serde_json::to_string).transpose()?,
                summary,
                completed_at.map(|value| value.timestamp()),
                run_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Every past hook firing for a graph, oldest first — surfaced
    /// via `graph_get`/`canopy graph info` alongside the graph's node runs.
    pub fn list_graph_completion_hook_runs(
        &self,
        graph_id: &str,
    ) -> Result<Vec<GraphCompletionHookRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, status, output, summary, started_at, completed_at, pid, boot_id, event, hook_index, executed_platform, executed_model
             FROM graph_completion_hook_runs WHERE graph_id = ?1 ORDER BY started_at ASC, rowid ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_completion_hook_run_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Mark a graph as having been launched by a hook (CH4). This is a
    /// transient flag that lives only for the duration of the run — it is
    /// cleared when the run completes (via [`Self::clear_graph_hook_launched`]).
    pub fn mark_graph_as_hook_launched(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET hook_launched = 1 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Clear the hook-launched flag for a graph. Called when the run completes.
    pub fn clear_graph_hook_launched(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graphs SET hook_launched = 0 WHERE id = ?1",
            params![graph_id],
        )?;
        Ok(rows > 0)
    }

    /// Whether the graph was launched by a hook (depth = 1).
    pub fn is_graph_hook_launched(&self, graph_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare("SELECT hook_launched FROM graphs WHERE id = ?1")?;
        let result: Option<bool> = stmt
            .query_row(params![graph_id], |row| row.get(0))
            .ok()
            .map(|v: i64| v != 0);
        Ok(result.unwrap_or(false))
    }

    /// Record provenance: which graph and event launched this graph (CH4).
    /// Stored in `graph_hook_launches` for traceability.
    pub fn record_hook_launch_provenance(
        &self,
        target_graph_id: &str,
        source_graph_id: &str,
        event: &str,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_hook_launches (target_graph_id, source_graph_id, event, launched_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                target_graph_id,
                source_graph_id,
                event,
                chrono::Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }

    /// Node runs still `running` for a graph.
    pub fn list_running_graph_runs(&self, graph_id: &str) -> Result<Vec<GraphNodeRun>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
             FROM graph_runs WHERE graph_id = ?1 AND status = 'running'",
        )?;
        let rows = stmt.query_map(params![graph_id], map_graph_run_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Reconcile graphs orphaned by a daemon restart.
    ///
    /// No graph run survives the process that spawned it, so any graph still
    /// `Running` at startup was interrupted mid-execution by the previous
    /// daemon. Pause it, mark its dangling node runs as failed/interrupted,
    /// and mark its in-flight spec (graph-bound or queue member — either way
    /// `run.spec_id` names it) `Interrupted` (not `Pending` — the run was cut
    /// short by something external, not a failure of the work), all in one
    /// transaction so there is no window where the graph is recoverable but
    /// the spec is not (B18). The engine never touches git here — no more
    /// `git stash` — so any uncommitted work the interrupted run left behind
    /// stays exactly where it is; restarting the spec from its entry node on
    /// resume finds it there, and its node prompt says so. Leaving the spec
    /// `running` made it invisible to queue selection
    /// (`queue_next_pending_spec_id` only ever picks a `pending` or
    /// `interrupted` member), permanently orphaning it — `Interrupted` is
    /// just as selectable as `Pending`, so this still can't happen.
    /// `graph_continue` alone is enough to resume it (no `graph_pause` detour
    /// needed). Idempotent: a graph already `Paused` isn't touched by a later
    /// call.
    ///
    /// Daemon-lifecycle recovery only — **never** call this from anything
    /// other than the daemon's own startup path. On 2026-08-03, `canopy
    /// bridge`'s embedded stdio fallback (spawned when the reachability probe
    /// to a live daemon lost a race under concurrent load) ran this via
    /// `run_stdio_server`'s startup sequence: a short-lived helper process
    /// declared a graph the *live* daemon owned "orphaned", SIGKILLed its
    /// node's process group, `git stash`ed the workdir, and paused the graph
    /// — four times, silently, before it was noticed. The doc comment above
    /// only holds for the process that actually owns the daemon's lifecycle;
    /// it is false for any other process that happens to open the same
    /// database. `run_stdio_server` must never call this again. `data_dir`
    /// is used for the ownership gate below, which is the second line of
    /// defence against exactly that mistake, not a substitute for keeping
    /// the caller list to one entry.
    pub fn reconcile_orphaned_graphs(&self, data_dir: &std::path::Path) -> Result<usize> {
        // Ownership gate: if the on-disk pid file names a *live* process
        // that isn't us, some other process owns the daemon lifecycle right
        // now and this call has no business touching graph state — signal
        // nothing, pause nothing, quarantine nothing. Belt and braces with
        // keeping `run_stdio_server` from calling this at all: that removes
        // the caller, this makes the function itself safe to call by
        // mistake.
        if let Some(pid) = crate::daemon::process::read_pid(data_dir) {
            if pid != std::process::id() && crate::daemon::process::is_process_running(pid) {
                return Ok(0);
            }
        }

        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;

        let orphaned: Vec<Graph> = {
            let mut stmt = tx.prepare(
                "SELECT id, name, description, workdir, status, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks, allow_dirty_start
                 FROM graphs WHERE status IN (?1, ?2)",
            )?;
            // CB31: a graph caught mid-`Pausing` (wait-for-completion pause
            // requested, running node not yet finished) by a daemon restart is
            // just as orphaned as a `Running` one — its dispatch is gone and
            // the pending pause will never complete on its own. Reconcile it
            // the same way: pause, mark the dangling run interrupted, mark the
            // spec interrupted, so `graph_continue` alone can resume it.
            let rows = stmt.query_map(
                params![GraphStatus::Running.as_str(), GraphStatus::Pausing.as_str()],
                map_graph_row,
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        for lp in &orphaned {
            let dangling_runs: Vec<GraphNodeRun> = {
                let mut stmt = tx.prepare(
                    "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
                     FROM graph_runs WHERE graph_id = ?1 AND status = 'running'",
                )?;
                let rows = stmt.query_map(params![lp.id], map_graph_run_row)?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if dangling_runs.is_empty() {
                tracing::warn!(
                    "Reconciling orphaned graph '{}': no active node run found; pausing.",
                    lp.id
                );
            }
            for run in &dangling_runs {
                tracing::warn!(
                    "Reconciling orphaned graph '{}': was running node '{}' (spec '{}') when the daemon last stopped; pausing graph, marking its run as interrupted, and marking the spec interrupted.",
                    lp.id,
                    run.node_id,
                    run.spec_id
                );
                // B12: this new daemon process never held a `Child` for
                // `run` — it may not even share the previous process's
                // memory — so a persisted pid is all reconciliation has to
                // go on, and a pid alone can't tell a genuine survivor from
                // an unrelated process that reused the same pid after a
                // reboot recycled the pid space. Only attempt the kill when
                // the run's recorded boot id still matches the machine's
                // current one (same boot, i.e. the *daemon* crashed/restarted
                // without the OS rebooting) — otherwise the pid is
                // meaningless and killing it could hit an unrelated process.
                if let (Some(pid), Some(run_boot_id)) = (run.pid, run.boot_id.as_deref()) {
                    if crate::system::boot_id().as_deref() == Some(run_boot_id) {
                        let ancestors = crate::daemon::process::ancestor_pids();
                        if ancestors.contains(&(pid as u32)) {
                            tracing::warn!(
                                "Reconciling orphaned graph '{}': skipping kill of pid {} — it is an ancestor of this process",
                                lp.id,
                                pid
                            );
                        } else {
                            tracing::warn!(
                                "Reconciling orphaned graph '{}': attempting best-effort kill of survivor pid {} from the same boot.",
                                lp.id,
                                pid
                            );
                            crate::daemon::process::terminate_process_group_async(
                                pid,
                                crate::daemon::process::KILL_GRACE,
                            );
                        }
                    }
                }
                let output = serde_json::json!({
                    "interrupted": true,
                    "reason": "daemon restarted while this node was running"
                });

                let now = Utc::now();
                tx.execute(
                    "UPDATE graph_runs
                     SET status = ?1, output = ?2, completed_at = ?3, pid = NULL
                     WHERE id = ?4",
                    params![
                        GraphRunStatus::Fail.as_str(),
                        serde_json::to_string(&output)?,
                        now.timestamp(),
                        run.id,
                    ],
                )?;
                // The engine never touches git (no more `git stash`): the
                // partial work an interrupted run left in the workdir stays
                // exactly where it is. Marking the spec `Interrupted` (not
                // reset to `Pending`) is what used to be the stash's job —
                // it's what tells the next pickup's rendered prompt to say
                // "a previous attempt exists, continue it" instead of
                // silently starting fresh. `spec_start_head` and
                // `spec_committed_head` (C15) are still cleared: the next
                // attempt captures its own fresh baseline and starts with no
                // committed-head evidence of its own (`run_spec` only reuses
                // a persisted baseline for a same-attempt resume of a
                // `Running` spec, which an `Interrupted` pickup is not) — a
                // restart must never let a *stale* `spec_committed_head` from
                // the interrupted attempt pass a check for an attempt that
                // hasn't committed anything itself yet.
                tx.execute(
                    "UPDATE graph_specs
                     SET status = ?1, started_at = NULL, completed_at = NULL, spec_start_head = NULL, spec_committed_head = NULL,
                         updated_at = ?3
                     WHERE id = ?2",
                    params![
                        GraphSpecStatus::Interrupted.as_str(),
                        run.spec_id,
                        Utc::now().timestamp_millis()
                    ],
                )?;
            }
            // C1: flag this pause as reconciliation's own, distinct from an
            // operator's `graph_pause`/`graph_report_blocker` (both go through
            // `update_graph_status`, which always clears this flag) — see
            // [`crate::domain::graphs::Graph::paused_by_reconciliation`].
            tx.execute(
                "UPDATE graphs SET status = ?1, paused_by_reconciliation = 1 WHERE id = ?2",
                params![GraphStatus::Paused.as_str(), lp.id],
            )?;
        }

        tx.commit()?;
        Ok(orphaned.len())
    }

    /// Reset queue-member specs stuck `running` with no active node run in
    /// this daemon's lifetime back to `pending`. Covers the gap between
    /// `reconcile_orphaned_graphs` (which only touches graphs that were
    /// themselves `Running` at boot) and a queue member left `running` by a
    /// path that paused the graph without resetting the spec (e.g. a
    /// BLOCKER-reported spec that was never cleaned up). Called at server
    /// startup after `reconcile_orphaned_graphs`.
    pub fn reconcile_stranded_queue_specs(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;

        let paused_graphs: Vec<(String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT id, active_run_queue_id FROM graphs
                 WHERE status = ?1 AND active_run_queue_id IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![GraphStatus::Paused.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut reset_count = 0;
        let boot_id = crate::system::boot_id();
        let current_boot_id = boot_id.as_deref();

        for (graph_id, queue_id) in &paused_graphs {
            let stranded_specs: Vec<String> = {
                let mut stmt = tx.prepare(
                    "SELECT pm.spec_id FROM queue_members pm
                     JOIN graph_specs ls ON ls.id = pm.spec_id
                     WHERE pm.queue_id = ?1 AND ls.status = ?2
                     AND NOT EXISTS (
                         SELECT 1 FROM graph_runs lr
                         WHERE lr.spec_id = pm.spec_id
                         AND lr.status = 'running'
                         AND lr.boot_id = ?3
                     )
                     ORDER BY pm.position ASC",
                )?;
                let rows = stmt.query_map(
                    params![queue_id, GraphSpecStatus::Running.as_str(), current_boot_id],
                    |row| row.get::<_, String>(0),
                )?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            for spec_id in &stranded_specs {
                tracing::warn!(
                    "Reconciling stranded queue spec '{}' in graph '{}': \
                     was 'running' with no active node run in this daemon's \
                     lifetime; checking for evidence it was genuinely interrupted.",
                    spec_id,
                    graph_id
                );

                // B36: the `NOT EXISTS` above only proves no run for this
                // spec claims the *current* boot — a `graph_runs` row can
                // still be sitting at `status = 'running'` from a previous
                // boot (e.g. the daemon died before a graceful path like
                // `graph_report_blocker` could finalize it). Identify it from
                // recorded facts only — a boot id that isn't this one, or a
                // pid that's no longer alive — and mark it distinctly as
                // interrupted (never as a plain node failure). The engine
                // never touches git: no quarantine, no `git stash` — the
                // interrupted run's worktree changes are left exactly as
                // they were.
                let stale_run: Option<(String, Option<i64>, Option<String>)> = tx
                    .query_row(
                        "SELECT id, pid, boot_id FROM graph_runs
                         WHERE spec_id = ?1 AND status = 'running'
                         ORDER BY started_at DESC LIMIT 1",
                        params![spec_id],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()?;

                // Only evidence of a genuinely interrupted run (a stale-boot
                // or dead-pid `graph_runs` row) earns `Interrupted` instead of
                // a plain reset to `Pending` — a spec stuck `running` with no
                // run row behind it at all has no such evidence, so it's
                // reset exactly as before.
                let mut interrupted = false;
                if let Some((run_id, pid, run_boot_id)) = stale_run {
                    let boot_mismatch = match (run_boot_id.as_deref(), current_boot_id) {
                        (Some(a), Some(b)) => a != b,
                        _ => false,
                    };
                    let pid_dead = pid
                        .and_then(|p| u32::try_from(p).ok())
                        .map(|p| !crate::daemon::process::is_process_running(p))
                        .unwrap_or(false);

                    if boot_mismatch || pid_dead {
                        interrupted = true;
                        let output = serde_json::json!({
                            "interrupted": true,
                            "reason": "daemon restarted or its process died while this node was running"
                        });

                        tx.execute(
                            "UPDATE graph_runs
                             SET status = ?1, output = ?2, completed_at = ?3, pid = NULL
                             WHERE id = ?4",
                            params![
                                GraphRunStatus::Fail.as_str(),
                                serde_json::to_string(&output)?,
                                Utc::now().timestamp(),
                                run_id,
                            ],
                        )?;
                    }
                }

                let new_status = if interrupted {
                    GraphSpecStatus::Interrupted
                } else {
                    GraphSpecStatus::Pending
                };
                tx.execute(
                    "UPDATE graph_specs
                     SET status = ?1, started_at = NULL, completed_at = NULL,
                         spec_start_head = NULL, spec_committed_head = NULL,
                         updated_at = ?3
                     WHERE id = ?2",
                    params![new_status.as_str(), spec_id, Utc::now().timestamp_millis()],
                )?;
                reset_count += 1;
            }
        }

        tx.commit()?;
        Ok(reset_count)
    }

    pub fn get_graph_details(&self, graph_id: &str) -> Result<Option<GraphDetails>> {
        let Some(lp) = self.get_graph(graph_id)? else {
            return Ok(None);
        };
        let graph_nodes = self.list_graph_nodes_for_graph(graph_id)?;
        let graph_edges = self.list_graph_edges_for_graph(graph_id)?;
        let specs = self
            .list_graph_specs(graph_id)?
            .into_iter()
            .map(|spec| {
                let nodes = self.list_graph_nodes(&spec.id)?;
                let edges = self.list_graph_edges(&spec.id)?;
                Ok(GraphSpecDetails { spec, nodes, edges })
            })
            .collect::<Result<Vec<_>>>()?;
        let completion_hook_runs = self.list_graph_completion_hook_runs(graph_id)?;

        Ok(Some(GraphDetails {
            lp,
            graph_nodes,
            graph_edges,
            specs,
            completion_hook_runs,
        }))
    }

    fn resolve_id_prefix(
        table: &str,
        prefix: &str,
        conn: &std::sync::MutexGuard<'_, rusqlite::Connection>,
    ) -> Result<Option<String>> {
        if prefix.is_empty() {
            return Ok(None);
        }
        let sql = format!("SELECT id FROM {table} WHERE id = ?1");
        let exists: bool = conn
            .query_row(&sql, params![prefix], |_| Ok(true))
            .optional()
            .map_err(|e| anyhow!("{}", e))?
            .unwrap_or(false);
        if exists {
            return Ok(Some(prefix.to_string()));
        }
        let escaped_prefix = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let like_sql = format!("SELECT id FROM {table} WHERE id LIKE ?1 || '%' ESCAPE '\\'");
        let mut stmt = conn.prepare(&like_sql)?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![escaped_prefix], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        match ids.len() {
            0 => Ok(None),
            1 => Ok(Some(ids.into_iter().next().unwrap())),
            _ => Err(anyhow!(
                "Ambiguous {} id prefix '{}' matches {} ids: {}",
                table,
                prefix,
                ids.len(),
                ids.join(", ")
            )),
        }
    }

    pub fn resolve_spec_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_specs", prefix, &conn)
    }

    pub fn resolve_graph_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graphs", prefix, &conn)
    }

    pub fn resolve_graph_node_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_nodes", prefix, &conn)
    }

    pub fn resolve_run_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_runs", prefix, &conn)
    }

    pub fn resolve_edge_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("graph_edges", prefix, &conn)
    }

    pub fn resolve_ensemble_id_by_prefix(&self, prefix: &str) -> Result<Option<String>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Self::resolve_id_prefix("ensembles", prefix, &conn)
    }
}

/// A graph node/edge must target exactly one of `spec_id`/`graph_id` — never
/// both (ambiguous ownership) and never neither (orphaned row no graph
/// would ever load). Checked here, before the row ever reaches the DB, so
/// callers get an actionable message instead of a raw `CHECK constraint
/// failed` from SQLite.
fn validate_single_target(spec_id: Option<&str>, graph_id: Option<&str>) -> Result<()> {
    match (spec_id, graph_id) {
        (Some(_), Some(_)) => Err(anyhow!(
            "Graph node/edge must target exactly one of spec_id or graph_id, not both."
        )),
        (None, None) => Err(anyhow!(
            "Graph node/edge must target exactly one of spec_id or graph_id."
        )),
        _ => Ok(()),
    }
}

/// CT3: byte bound for one served tail stream — mirrors the engine's
/// 64KB check-output truncation so a chunk flood (e.g. one 100KB line)
/// can never blow the TUI's memory through the line cap alone.
const TAIL_MAX_BYTES: usize = 64 * 1024;

/// CT3: keep the last [`TAIL_MAX_BYTES`] bytes of `text` on a char
/// boundary. Applied before the line cap so giant single lines are
/// bounded too.
fn take_last_bytes(text: &str) -> &str {
    if text.len() <= TAIL_MAX_BYTES {
        return text;
    }
    let mut start = text.len() - TAIL_MAX_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// CT3: keep the last `max_lines` lines of `text`. Line-oriented (not
/// byte-oriented) so the dialog's on-screen cap bounds memory without
/// splitting a line; `max_lines == 0` yields an empty string.
fn take_last_lines(text: &str, max_lines: usize) -> String {
    if max_lines == 0 || text.is_empty() {
        return String::new();
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

fn map_graph_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Graph> {
    let trigger = row
        .get::<_, Option<String>>(5)?
        .as_deref()
        .map(decode_graph_trigger)
        .transpose()?;
    let hooks = decode_graph_hooks(
        row.get::<_, Option<String>>(17)?.as_deref(),
        row.get::<_, Option<String>>(11)?.as_deref(),
    )?;
    Ok(Graph {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        workdir: row.get(3)?,
        status: GraphStatus::from_str(&row.get::<_, String>(4)?),
        trigger,
        created_at: from_timestamp(row.get(6)?)?,
        started_at: row
            .get::<_, Option<i64>>(7)?
            .map(from_timestamp)
            .transpose()?,
        completed_at: row
            .get::<_, Option<i64>>(8)?
            .map(from_timestamp)
            .transpose()?,
        autorun_at: row
            .get::<_, Option<i64>>(9)?
            .map(from_timestamp)
            .transpose()?,
        active_run_queue_id: row.get(10)?,
        hooks,
        auto_continue_at: row
            .get::<_, Option<i64>>(12)?
            .map(from_timestamp)
            .transpose()?,
        auto_continue_action: row.get(13)?,
        archived: row.get(14)?,
        paused_by_reconciliation: row.get(15)?,
        infra_node_id: row.get(16)?,
        allow_dirty_start: row.get(18)?,
    })
}

/// Encode a graph trigger into the `(trigger_type, trigger_config)` column pair,
/// mirroring how agents persist their trigger: a short type label plus the full
/// trigger serialized as JSON.
fn encode_graph_trigger(trigger: Option<&Trigger>) -> Result<(Option<String>, Option<String>)> {
    match trigger {
        Some(trigger) => Ok((
            Some(trigger.type_str().to_string()),
            Some(serde_json::to_string(trigger)?),
        )),
        None => Ok((None, None)),
    }
}

/// Decode the `trigger_config` JSON back into a [`Trigger`].
fn decode_graph_trigger(raw: &str) -> rusqlite::Result<Trigger> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })
}

/// Encode a graph's hooks map as JSON for the `hooks` column. `None` (no
/// hooks configured) stores `NULL` — exactly today's (pre-N2) row shape.
fn encode_graph_hooks(
    hooks: &std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
) -> Result<Option<String>> {
    if hooks.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(hooks)?))
}

/// Decode the `hooks` column JSON back into the event-keyed map. Falls
/// back to the legacy `on_completed` column when `hooks` is NULL/empty.
fn decode_graph_hooks(
    hooks_raw: Option<&str>,
    on_completed_raw: Option<&str>,
) -> rusqlite::Result<std::collections::BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>> {
    if let Some(raw) = hooks_raw {
        if !raw.is_empty() {
            return serde_json::from_str(raw).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    11,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            });
        }
    }
    // Fallback: legacy `on_completed` column
    if let Some(raw) = on_completed_raw {
        let hook = decode_graph_completion_hook(raw)?;
        let mut map = std::collections::BTreeMap::new();
        map.insert(GraphHookEvent::OnCompleted, vec![hook]);
        return Ok(map);
    }
    Ok(std::collections::BTreeMap::new())
}

/// Encode a graph's `on_completed` hook config as JSON for the `on_completed`
/// column. `None` (no hook configured) stores `NULL` — exactly today's
/// (pre-N2) row shape.
fn encode_graph_completion_hook(hook: Option<&GraphCompletionHook>) -> Result<Option<String>> {
    hook.map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

/// Decode the `on_completed` column JSON back into a [`GraphCompletionHook`].
fn decode_graph_completion_hook(raw: &str) -> rusqlite::Result<GraphCompletionHook> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(11, rusqlite::types::Type::Text, Box::new(error))
    })
}

fn map_graph_spec_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphSpec> {
    Ok(GraphSpec {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        position: row.get(4)?,
        parallelizable: row.get(5)?,
        status: GraphSpecStatus::from_str(&row.get::<_, String>(6)?),
        started_at: row
            .get::<_, Option<i64>>(7)?
            .map(from_timestamp)
            .transpose()?,
        completed_at: row
            .get::<_, Option<i64>>(8)?
            .map(from_timestamp)
            .transpose()?,
        spec_start_head: row.get(9)?,
        spec_start_dirty: row.get(10)?,
        workdir: row.get(11)?,
        completed_via: row.get(12)?,
        completed_via_reason: row.get(13)?,
        completed_via_at: row
            .get::<_, Option<i64>>(14)?
            .map(from_timestamp)
            .transpose()?,
        spec_committed_head: row.get(15)?,
        spec_end_dirty: row.get(16)?,
        spec_end_dirty_paths: row
            .get::<_, Option<String>>(17)?
            .map(|raw| raw.lines().map(str::to_string).collect()),
    })
}

fn map_graph_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let kind = GraphNodeKind::from_str(&row.get::<_, String>(4)?).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                "Invalid graph node kind",
            )),
        )
    })?;
    let config_raw: String = row.get(5)?;
    let config = parse_json_value(&config_raw)?;

    Ok(GraphNode {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        graph_id: row.get(2)?,
        name: row.get(3)?,
        kind,
        config,
        position: row.get(6)?,
        created_at: from_timestamp(row.get(7)?)?,
    })
}

fn map_graph_edge_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdge> {
    let condition_tag: String = row.get(5)?;
    let route_label: Option<String> = row.get(6)?;
    let condition =
        GraphEdgeCondition::from_parts(&condition_tag, route_label).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(IoError::new(
                    ErrorKind::InvalidData,
                    "Invalid graph edge condition",
                )),
            )
        })?;

    Ok(GraphEdge {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        graph_id: row.get(2)?,
        from_node: row.get(3)?,
        to_node: row.get(4)?,
        condition,
    })
}

fn active_graph_run_for_spec_locked(
    conn: &rusqlite::Connection,
    spec_id: &str,
) -> rusqlite::Result<Option<GraphNodeRun>> {
    let mut stmt = conn.prepare(
        "SELECT id, graph_id, spec_id, node_id, status, input, output, started_at, completed_at, iteration, pid, boot_id, session_id, executed_platform, executed_model
         FROM graph_runs
         WHERE spec_id = ?1 AND status = 'running'
         ORDER BY started_at DESC
         LIMIT 1",
    )?;
    stmt.query_row(params![spec_id], map_graph_run_row)
        .optional()
}

fn map_graph_run_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNodeRun> {
    Ok(GraphNodeRun {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        spec_id: row.get(2)?,
        node_id: row.get(3)?,
        status: GraphRunStatus::from_str(&row.get::<_, String>(4)?),
        input: row
            .get::<_, Option<String>>(5)?
            .as_deref()
            .map(parse_json_value)
            .transpose()?,
        output: row
            .get::<_, Option<String>>(6)?
            .as_deref()
            .map(parse_json_value)
            .transpose()?,
        started_at: from_timestamp(row.get(7)?)?,
        completed_at: row
            .get::<_, Option<i64>>(8)?
            .map(from_timestamp)
            .transpose()?,
        iteration: row.get(9)?,
        pid: row.get(10)?,
        boot_id: row.get(11)?,
        session_id: row.get(12)?,
        executed_platform: row.get(13)?,
        executed_model: row.get(14)?,
    })
}

fn map_graph_completion_hook_run_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<GraphCompletionHookRun> {
    let event_str: String = row.get(9)?;
    let event = GraphHookEvent::from_str(&event_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            9,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Invalid graph hook event",
            )),
        )
    })?;
    Ok(GraphCompletionHookRun {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        event,
        hook_index: row.get(10)?,
        status: GraphRunStatus::from_str(&row.get::<_, String>(2)?),
        output: row
            .get::<_, Option<String>>(3)?
            .as_deref()
            .map(parse_json_value)
            .transpose()?,
        summary: row.get(4)?,
        started_at: from_timestamp(row.get(5)?)?,
        completed_at: row
            .get::<_, Option<i64>>(6)?
            .map(from_timestamp)
            .transpose()?,
        pid: row.get(7)?,
        boot_id: row.get(8)?,
        executed_platform: row.get(11)?,
        executed_model: row.get(12)?,
    })
}

fn from_timestamp(value: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(value, 0).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                "Invalid timestamp value",
            )),
        )
    })
}

fn parse_json_value(raw: &str) -> rusqlite::Result<Value> {
    serde_json::from_str(raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{Graph, GraphStatus};
    use chrono::Utc;
    use std::collections::BTreeMap;
    use tempfile::tempdir;

    fn test_db() -> Database {
        let dir = tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn sample_graph(id: &str) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: format!("Graph {id}"),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: BTreeMap::new(),
        }
    }

    #[test]
    fn insert_and_get_graph() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let retrieved = db.get_graph("loop1").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.id, "loop1");
        assert_eq!(retrieved.name, "Graph loop1");
    }

    #[test]
    fn get_graph_not_found() {
        let db = test_db();
        let result = db.get_graph("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn delete_graph() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        db.delete_graph("loop1").unwrap();
        let retrieved = db.get_graph("loop1").unwrap();
        assert!(retrieved.is_none());
    }

    #[test]
    fn list_graphs_empty() {
        let db = test_db();
        let graphs = db.list_graphs(None, false).unwrap();
        assert!(graphs.is_empty());
    }

    #[test]
    fn list_graphs_with_graphs() {
        let db = test_db();
        let loop1 = sample_graph("loop1");
        let loop2 = sample_graph("loop2");
        db.insert_graph(&loop1).unwrap();
        db.insert_graph(&loop2).unwrap();

        let graphs = db.list_graphs(None, false).unwrap();
        assert_eq!(graphs.len(), 2);
    }

    #[test]
    fn update_graph_status() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        db.update_graph_status("loop1", GraphStatus::Running, None, None)
            .unwrap();
        let retrieved = db.get_graph("loop1").unwrap().unwrap();
        assert_eq!(retrieved.status, GraphStatus::Running);
    }

    #[test]
    fn schedule_and_clear_graph_autorun() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let scheduled_at = Utc::now() + chrono::Duration::hours(1);
        db.schedule_graph_autorun("loop1", scheduled_at).unwrap();

        let retrieved = db.get_graph("loop1").unwrap().unwrap();
        assert!(retrieved.autorun_at.is_some());

        db.clear_graph_autorun("loop1").unwrap();
        let retrieved = db.get_graph("loop1").unwrap().unwrap();
        assert!(retrieved.autorun_at.is_none());
    }

    #[test]
    fn list_pending_autorun_graphs_empty() {
        let db = test_db();
        let graphs = db.list_pending_autorun_graphs().unwrap();
        assert!(graphs.is_empty());
    }

    #[test]
    fn list_pending_autorun_graphs_with_scheduled() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let scheduled_at = Utc::now() - chrono::Duration::hours(1);
        db.schedule_graph_autorun("loop1", scheduled_at).unwrap();

        let graphs = db.list_pending_autorun_graphs().unwrap();
        assert_eq!(graphs.len(), 1);
    }

    #[test]
    fn validate_single_target_both_none() {
        let result = validate_single_target(None, None);
        assert!(result.is_err());
    }

    #[test]
    fn validate_single_target_both_some() {
        let result = validate_single_target(Some("spec1"), Some("loop1"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_single_target_spec_only() {
        let result = validate_single_target(Some("spec1"), None);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_single_target_graph_only() {
        let result = validate_single_target(None, Some("loop1"));
        assert!(result.is_ok());
    }

    #[test]
    fn encode_and_decode_graph_trigger_none() {
        let (type_str, config) = encode_graph_trigger(None).unwrap();
        assert!(type_str.is_none());
        assert!(config.is_none());
    }

    #[test]
    fn parse_json_value_valid() {
        let result = parse_json_value(r#"{"key": "value"}"#);
        assert!(result.is_ok());
    }

    #[test]
    fn parse_json_value_invalid() {
        let result = parse_json_value("invalid json");
        assert!(result.is_err());
    }

    #[test]
    fn list_graph_last_run_times_empty_when_no_runs() {
        let db = test_db();
        let graph_obj = sample_graph("loop1");
        db.insert_graph(&graph_obj).unwrap();

        let times = db.list_graph_last_run_times().unwrap();
        assert!(
            times.is_empty(),
            "a graph with no recorded runs must not appear in the map"
        );
    }

    #[test]
    fn list_graph_last_run_times_returns_max_started_at_per_graph() {
        let db = test_db();
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop1', 'l1', '/tmp', 'draft', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop2', 'l2', '/tmp', 'draft', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec1', 'loop1', 's1', 0, 0, 'pending')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES ('n1', 'spec1', NULL, 'node1', 'agent', '{}', 0, 0)",
            params![],
        )
        .unwrap();
        // Two runs on loop1 — the later one (started_at 500) must win.
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES ('run1', 'loop1', 'spec1', 'n1', 'success', 100, 1)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES ('run2', 'loop1', 'spec1', 'n1', 'success', 500, 2)",
            params![],
        )
        .unwrap();
        drop(conn);

        let times = db.list_graph_last_run_times().unwrap();
        assert_eq!(times.len(), 1, "loop2 has no runs, so must be absent");
        assert_eq!(
            times.get("loop1").unwrap().timestamp(),
            500,
            "must report the MAX started_at, not the first run"
        );
        assert!(!times.contains_key("loop2"));
    }

    /// Seeds loop1 (spec1: nodes n1/n2, spec2: node n3) and loop2 (spec3:
    /// node n4) with runs at increasing `started_at`, so tests can assert
    /// default ordering, spec/node filters, and paging against a fixture
    /// that mirrors a real multi-spec, multi-node graph.
    fn seed_node_runs_fixture(db: &Database) {
        let conn = db.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop1', 'l1', '/tmp', 'running', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graphs (id, name, workdir, status, created_at) VALUES ('loop2', 'l2', '/tmp', 'running', 0)",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec1', 'loop1', 's1', 0, 0, 'pending')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec2', 'loop1', 's2', 1, 0, 'pending')",
            params![],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO graph_specs (id, graph_id, name, position, parallelizable, status) VALUES ('spec3', 'loop2', 's3', 0, 0, 'pending')",
            params![],
        )
        .unwrap();
        for (node_id, spec_id, position) in [
            ("n1", "spec1", 0),
            ("n2", "spec1", 1),
            ("n3", "spec2", 0),
            ("n4", "spec3", 0),
        ] {
            conn.execute(
                "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at) VALUES (?1, ?2, NULL, ?1, 'agent', '{}', ?3, 0)",
                params![node_id, spec_id, position],
            )
            .unwrap();
        }
        // loop1: run1 (spec1/n1, t=100), run2 (spec1/n2, t=200), run3
        // (spec2/n3, t=300, fail). loop2: run4 (spec3/n4, t=400) — must never
        // leak into a loop1 query.
        for (id, graph_id, spec_id, node_id, status, started_at) in [
            ("run1", "loop1", "spec1", "n1", "pass", 100),
            ("run2", "loop1", "spec1", "n2", "pass", 200),
            ("run3", "loop1", "spec2", "n3", "fail", 300),
            ("run4", "loop2", "spec3", "n4", "pass", 400),
        ] {
            conn.execute(
                "INSERT INTO graph_runs (id, graph_id, spec_id, node_id, status, started_at, iteration) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)",
                params![id, graph_id, spec_id, node_id, status, started_at],
            )
            .unwrap();
        }
        drop(conn);
    }

    #[test]
    fn list_graph_node_runs_defaults_to_most_recent_first_scoped_to_graph() {
        let db = test_db();
        seed_node_runs_fixture(&db);

        let runs = db.list_graph_node_runs("loop1", None, None, 10, 0).unwrap();
        let ids: Vec<&str> = runs.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["run3", "run2", "run1"],
            "most-recent-started_at run must lead, and loop2's run4 must never appear"
        );
    }

    #[test]
    fn list_graph_node_runs_filters_by_spec_and_node() {
        let db = test_db();
        seed_node_runs_fixture(&db);

        let by_spec = db
            .list_graph_node_runs("loop1", Some("spec1"), None, 10, 0)
            .unwrap();
        assert_eq!(
            by_spec.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run2", "run1"]
        );

        let by_node = db
            .list_graph_node_runs("loop1", None, Some("n3"), 10, 0)
            .unwrap();
        assert_eq!(
            by_node.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run3"]
        );

        let by_both = db
            .list_graph_node_runs("loop1", Some("spec1"), Some("n1"), 10, 0)
            .unwrap();
        assert_eq!(
            by_both.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run1"]
        );
    }

    #[test]
    fn list_graph_node_runs_pages_with_limit_and_offset() {
        let db = test_db();
        seed_node_runs_fixture(&db);

        let first_page = db.list_graph_node_runs("loop1", None, None, 2, 0).unwrap();
        assert_eq!(
            first_page.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["run3", "run2"]
        );

        let second_page = db.list_graph_node_runs("loop1", None, None, 2, 2).unwrap();
        assert_eq!(
            second_page
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            vec!["run1"],
            "offset must skip past the first page's runs, not repeat them"
        );
    }

    #[test]
    fn list_all_graph_nodes_returns_every_node_regardless_of_spec_or_graph_scope() {
        let db = test_db();
        let lp = sample_graph("graph-1");
        db.insert_graph(&lp).unwrap();
        let spec = GraphSpec {
            id: "spec-1".to_string(),
            graph_id: Some(lp.id.clone()),
            name: "Spec".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        db.insert_graph_spec(&spec).unwrap();

        let spec_scoped = GraphNode {
            id: "node-spec".to_string(),
            spec_id: Some(spec.id),
            graph_id: None,
            name: "Spec Node".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 1,
            created_at: Utc::now(),
        };
        let graph_scoped = GraphNode {
            id: "node-graph".to_string(),
            spec_id: None,
            graph_id: Some(lp.id),
            name: "Graph Node".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 1,
            created_at: Utc::now(),
        };
        db.insert_graph_node(&spec_scoped).unwrap();
        db.insert_graph_node(&graph_scoped).unwrap();

        let all_ids: Vec<String> = db
            .list_all_graph_nodes()
            .unwrap()
            .into_iter()
            .map(|node| node.id)
            .collect();
        assert_eq!(all_ids.len(), 2);
        assert!(all_ids.contains(&"node-spec".to_string()));
        assert!(all_ids.contains(&"node-graph".to_string()));
    }

    #[test]
    fn find_dirty_predecessor_spec_excludes_given_id_and_completed_status() {
        let db = test_db();
        let workdir = "/tmp/dirty-predecessor-test";
        let mk = |id: &str, status: GraphSpecStatus, workdir: Option<&str>| GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: workdir.map(str::to_string),
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        };
        // Completed shares the workdir but is never a candidate; Interrupted
        // sits on an unrelated workdir so it can never be mistaken for this
        // workdir's predecessor either — leaving exactly one qualifying spec.
        db.insert_graph_spec(&mk(
            "spec-completed",
            GraphSpecStatus::Completed,
            Some(workdir),
        ))
        .unwrap();
        db.insert_graph_spec(&mk("spec-failed", GraphSpecStatus::Failed, Some(workdir)))
            .unwrap();
        db.insert_graph_spec(&mk(
            "spec-interrupted",
            GraphSpecStatus::Interrupted,
            Some("/tmp/unrelated-workdir"),
        ))
        .unwrap();

        let found = db
            .find_dirty_predecessor_spec(workdir, None)
            .unwrap()
            .expect("the failed spec on this workdir should be found");
        assert_eq!(found.id, "spec-failed");

        let excluded = db
            .find_dirty_predecessor_spec(workdir, Some("spec-failed"))
            .unwrap();
        assert!(
            excluded.is_none(),
            "excluding the only qualifying spec must fall through to None"
        );
    }

    // ── CT3: live tail storage ──────────────────────────────────────

    fn tail_test_run(db: &Database, run_id: &str) {
        use crate::domain::graphs::GraphRunStatus;
        db.insert_graph(&sample_graph("tail-graph")).unwrap();
        db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
            id: "tail-spec".to_string(),
            graph_id: Some("tail-graph".to_string()),
            name: "Tail spec".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Running,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
        })
        .unwrap();
        db.insert_graph_node(&crate::domain::graphs::GraphNode {
            id: "tail-node".to_string(),
            spec_id: Some("tail-spec".to_string()),
            graph_id: None,
            name: "Tail node".to_string(),
            kind: crate::domain::graphs::GraphNodeKind::Check,
            config: serde_json::json!({"command": "true"}),
            position: 0,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_run(&crate::domain::graphs::GraphNodeRun {
            id: run_id.to_string(),
            graph_id: "tail-graph".to_string(),
            spec_id: "tail-spec".to_string(),
            node_id: "tail-node".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
    }

    #[test]
    fn append_and_get_graph_run_tail_returns_chunks_in_order() {
        let db = test_db();
        tail_test_run(&db, "run-tail-1");
        db.append_graph_run_output("run-tail-1", "stdout", "out-a\n")
            .unwrap();
        db.append_graph_run_output("run-tail-1", "stderr", "err-a\n")
            .unwrap();
        db.append_graph_run_output("run-tail-1", "stdout", "out-b\n")
            .unwrap();

        let (stdout, stderr) = db.get_graph_run_tail("run-tail-1", 1000).unwrap();
        assert_eq!(stdout, "out-a\nout-b");
        assert_eq!(stderr, "err-a");
    }

    #[test]
    fn get_graph_run_tail_caps_lines_to_max() {
        let db = test_db();
        tail_test_run(&db, "run-tail-2");
        for i in 0..10 {
            db.append_graph_run_output("run-tail-2", "stdout", &format!("line {i}\n"))
                .unwrap();
        }
        let (stdout, _) = db.get_graph_run_tail("run-tail-2", 3).unwrap();
        assert_eq!(stdout, "line 7\nline 8\nline 9");
    }

    #[test]
    fn set_graph_run_tail_snapshot_serves_post_completion_tail() {
        let db = test_db();
        tail_test_run(&db, "run-tail-3");
        // Zero-output hang: no chunks at all — the snapshot is the only
        // source, and an empty one must read back as empty, not error.
        db.set_graph_run_tail_snapshot("run-tail-3", Some("final out"), Some(""))
            .unwrap();
        let (stdout, stderr) = db.get_graph_run_tail("run-tail-3", 1000).unwrap();
        assert_eq!(stdout, "final out");
        assert_eq!(stderr, "");
    }

    #[test]
    fn append_graph_run_output_rejects_unknown_stream() {
        let db = test_db();
        tail_test_run(&db, "run-tail-4");
        assert!(
            db.append_graph_run_output("run-tail-4", "stdin", "x")
                .is_err(),
            "CHECK constraint must reject streams other than stdout/stderr"
        );
    }
}
