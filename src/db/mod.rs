use anyhow::Result;
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

/// Bumped whenever a migration changes table/column names in a way that an
/// older binary's `CREATE TABLE IF NOT EXISTS` / `ADD COLUMN IF NOT EXISTS`
/// guards can't detect (they'd silently recreate the old names as empty
/// rather than erroring) — see `check_schema_version`. Version 4 adds
/// the `activity_log` table (CM20 bitácora).
/// Version 5 renames the loop→graph schema (tables, `loop_id` columns, and
/// `break`→`error` edge conditions) — see `migrate_loop_to_graph_schema` (CC3).
/// Version 6 changes the `ensembles` entry/exit FKs from `ON DELETE CASCADE`
/// / `ON DELETE SET NULL` to `ON DELETE RESTRICT` — see
/// `migrate_cb52_ensemble_fk` (CB52).
/// Version 7 rewrites `activity_log.source` rows that carry the retired
/// `loop:` attribution prefix to `graph:` — see
/// `migrate_activity_attribution_prefix` (CB63).
/// Version 8 adds the `graph_events` durable per-graph event log — see
/// `migrate_graph_events_table` (CM46).
/// Version 10 adds the `crews`/`crew_members` tables and the ensemble
/// binding columns — see `migrate_crews_tables` (CM49).
/// Version 11 adds the `action`/`target_session_id`/`target_session_name`
/// columns to `agents` for `send_to_session` delivery (CM53).
const SCHEMA_VERSION: i64 = 11;

/// CM40: how long every daemon SQLite connection waits for a concurrent writer
/// before failing with `SQLITE_BUSY`. Applied once, at open, in
/// `db::connection`; the `graph_import` busy message renders this same value so
/// the timeout and the message can never disagree.
pub(crate) const DB_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Thread-safe `SQLite` database wrapper.
///
/// Uses an `Arc<Mutex<Connection>>` so the handle can be cheaply cloned and
/// shared across threads (e.g. for background file-scanning tasks) while still
/// serialising all SQLite writes through a single connection.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
    /// Wakes node runs waiting for named mutexes whenever leases or the
    /// waiter queue change (grant path re-checks under the connection lock,
    /// so a notify that lands before the wait is harmless).
    mutex_notify: Arc<tokio::sync::Notify>,
}

/// Base schema: every table and index created on a fresh database. Kept out of
/// the function body (clippy fn-length) and split into ordered `execute_batch`
/// chunks; concatenated, the chunks are the original single batch.
const BASE_SCHEMA_BATCHES: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS agents (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                cli TEXT NOT NULL,
                model TEXT,
                effort TEXT,
                working_dir TEXT,
                enabled BOOLEAN NOT NULL DEFAULT 1,
                enable_at TEXT,
                created_at TEXT NOT NULL,
                log_path TEXT NOT NULL,
                timeout_minutes INTEGER NOT NULL DEFAULT 15,
                expires_at TEXT,
                last_run_at TEXT,
                last_run_ok BOOLEAN,
                last_triggered_at TEXT,
                trigger_count INTEGER NOT NULL DEFAULT 0,
                notify_on_success BOOLEAN NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                background_agent_id TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                trigger_type TEXT NOT NULL,
                summary TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                exit_code INTEGER,
                timeout_at TEXT,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE TABLE IF NOT EXISTS daemon_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS interactive_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                cli TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                args TEXT,
                started_at TEXT NOT NULL,
                exited_at TEXT,
                exit_code INTEGER,
                status TEXT NOT NULL DEFAULT 'active',
                session_type TEXT NOT NULL DEFAULT 'interactive',
                pid INTEGER,
                boot_id TEXT,
                harness_conversation_id TEXT
            );

            CREATE TABLE IF NOT EXISTS terminal_sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                shell TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                created_at TEXT NOT NULL,
                last_active TEXT,
                status TEXT NOT NULL DEFAULT 'idle'
            );

            CREATE INDEX IF NOT EXISTS idx_interactive_sessions_workdir
                ON interactive_sessions(working_dir, started_at DESC);

            CREATE INDEX IF NOT EXISTS idx_terminal_sessions_workdir
                ON terminal_sessions(working_dir, created_at DESC);

            CREATE TABLE IF NOT EXISTS groups (
                id TEXT PRIMARY KEY,
                orientation TEXT NOT NULL DEFAULT 'horizontal',
                session_a TEXT NOT NULL,
                session_b TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sync_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workdir TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                agent_name TEXT NOT NULL,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS activity_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workdir TEXT NOT NULL,
                source TEXT NOT NULL,
                source_id TEXT,
                kind TEXT NOT NULL,
                message TEXT NOT NULL,
                payload TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_activity_log_workdir_created
                ON activity_log(workdir, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_activity_log_kind
                ON activity_log(kind);

            CREATE TABLE IF NOT EXISTS sync_locks (
                id TEXT PRIMARY KEY,
                workdir TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                lock_type TEXT NOT NULL,
                resource TEXT NOT NULL,
                acquired_at INTEGER NOT NULL,
                expires_at INTEGER,
                released_at INTEGER
            );

            CREATE TABLE IF NOT EXISTS projects (
                hash TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                tags TEXT,
                indexed_at INTEGER,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rag_queue (
                source_path TEXT NOT NULL PRIMARY KEY,
                status TEXT NOT NULL,
                queued_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS rag_file_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_path TEXT NOT NULL,
                event_type TEXT NOT NULL,
                detail TEXT,
                occurred_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_rag_file_events_path
                ON rag_file_events(file_path);

            CREATE TABLE IF NOT EXISTS intelligence_nodes (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'noted',
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                content_touched_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_kind_updated
                ON intelligence_nodes(kind, updated_at DESC);
            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_project_hash
                ON intelligence_nodes(project_hash);
            CREATE INDEX IF NOT EXISTS idx_intelligence_nodes_session_id
                ON intelligence_nodes(session_id);

            CREATE TABLE IF NOT EXISTS intelligence_edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                from_node_id TEXT NOT NULL,
                to_node_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                weight REAL NOT NULL DEFAULT 1.0,
                created_at INTEGER NOT NULL,
                FOREIGN KEY(from_node_id) REFERENCES intelligence_nodes(id) ON DELETE CASCADE,
                FOREIGN KEY(to_node_id) REFERENCES intelligence_nodes(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_intelligence_edges_from
                ON intelligence_edges(from_node_id);
            CREATE INDEX IF NOT EXISTS idx_intelligence_edges_to
                ON intelligence_edges(to_node_id);

            CREATE TABLE IF NOT EXISTS operational_sessions (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                body TEXT NOT NULL,
                metadata TEXT,
                project_hash TEXT,
                session_id TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_operational_sessions_project_hash
                ON operational_sessions(project_hash);
            CREATE INDEX IF NOT EXISTS idx_operational_sessions_updated
                ON operational_sessions(updated_at DESC);

            CREATE TABLE IF NOT EXISTS graphs (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT,
                workdir TEXT NOT NULL,
                status TEXT NOT NULL,
                trigger_type TEXT,
                trigger_config TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                autorun_at INTEGER,
                spec_queue TEXT,
                active_run_queue_id TEXT,
                active_run_idea TEXT,
                require_spec_type INTEGER NOT NULL DEFAULT 0,
                on_completed TEXT,
                auto_continue_at INTEGER,
                auto_continue_action TEXT,
                hooks TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_graphs_workdir_created
                ON graphs(workdir, created_at DESC);

            CREATE TABLE IF NOT EXISTS graph_specs (
                id TEXT PRIMARY KEY,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                description TEXT,
                position INTEGER NOT NULL,
                parallelizable INTEGER NOT NULL DEFAULT 0,
                status TEXT NOT NULL,
                started_at INTEGER,
                completed_at INTEGER,
                spec_start_head TEXT,
                spec_start_dirty INTEGER,
                workdir TEXT,
                spec_type TEXT,
                spec_scope TEXT,
                spec_code TEXT,
                completed_via TEXT,
                completed_via_reason TEXT,
                completed_via_at INTEGER,
                spec_committed_head TEXT,
                spec_end_dirty INTEGER,
                spec_end_dirty_paths TEXT,
                cross_run_attempts INTEGER NOT NULL DEFAULT 0,
                -- CT17: monotonic change signal for the panel's backlog event.
                -- Milliseconds since epoch; every write to this row refreshes
                -- it (see `insert_graph_spec` and the `UPDATE graph_specs`
                -- writers in `db/graphs.rs`). Never rendered, so the unit is
                -- free — millis make same-tick edits distinguishable where
                -- seconds would tie. Legacy rows backfilled below hold
                -- seconds-scale values and are always smaller.
                updated_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_specs_position
                ON graph_specs(graph_id, position);

            CREATE TABLE IF NOT EXISTS spec_code_counters (
                workdir TEXT NOT NULL,
                spec_type TEXT NOT NULL,
                next INTEGER NOT NULL,
                PRIMARY KEY (workdir, spec_type)
            );

            CREATE TABLE IF NOT EXISTS graph_nodes (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                position INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_graph_nodes_position
                ON graph_nodes(spec_id, position);

            CREATE TABLE IF NOT EXISTS graph_edges (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                to_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                condition TEXT NOT NULL,
                route TEXT,
                CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
            );

            CREATE INDEX IF NOT EXISTS idx_graph_edges_spec_from
                ON graph_edges(spec_id, from_node);

            CREATE TABLE IF NOT EXISTS graph_runs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                input TEXT,
                output TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                iteration INTEGER NOT NULL DEFAULT 1,
                pid INTEGER,
                boot_id TEXT,
                session_id TEXT,
                paused_through INTEGER NOT NULL DEFAULT 0,
                executed_platform TEXT,
                executed_model TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_graph_runs_spec_started
                ON graph_runs(spec_id, started_at ASC);

            CREATE INDEX IF NOT EXISTS idx_graph_runs_node_iteration
                ON graph_runs(node_id, iteration DESC);

            -- Speeds the sidebar's last-activity-per-graph aggregate
            -- (MAX(started_at) GROUP BY graph_id) into a loose index scan
            -- instead of a full table scan.
            CREATE INDEX IF NOT EXISTS idx_graph_runs_graph_started
                ON graph_runs(graph_id, started_at DESC);

            -- N2: firings of a graph's `on_completed` hook. Deliberately not
            -- `graph_runs` — that table's spec_id/node_id are NOT NULL FKs into
            -- a spec's graph, which a completion hook (no spec, no graph node)
            -- can never satisfy.
            CREATE TABLE IF NOT EXISTS graph_completion_hook_runs (
                id TEXT PRIMARY KEY,
                graph_id TEXT NOT NULL REFERENCES graphs(id) ON DELETE CASCADE,
                status TEXT NOT NULL,
                output TEXT,
                summary TEXT,
                started_at INTEGER NOT NULL,
                completed_at INTEGER,
                pid INTEGER,
                boot_id TEXT,
                event TEXT NOT NULL DEFAULT 'on_completed',
                hook_index INTEGER NOT NULL DEFAULT 0,
                executed_platform TEXT,
                executed_model TEXT,
                hook_config TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_graph_completion_hook_runs_graph_started
                ON graph_completion_hook_runs(graph_id, started_at ASC);

            -- F1: an ensemble unit -- the members and join are ordinary
            -- graph_nodes/graph_edges rows (the engine's graph-walking code is
            -- reused as-is); this row is what lets graph_get/graph_update_ensemble
            -- address the whole ensemble as one thing instead of N+1 nodes.
            CREATE TABLE IF NOT EXISTS ensembles (
                id TEXT PRIMARY KEY,
                spec_id TEXT REFERENCES graph_specs(id) ON DELETE CASCADE,
                graph_id TEXT REFERENCES graphs(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                prompt_template TEXT NOT NULL,
                join_node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                entry_from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT,
                entry_condition TEXT NOT NULL,
                min_pass INTEGER NOT NULL,
                straggler_timeout_minutes INTEGER,
                quorum_grace_minutes INTEGER,
                timeout_minutes INTEGER NOT NULL,
                infra_retry_limit INTEGER,
                infra_crash_max_seconds INTEGER,
                infra_backoff_seconds INTEGER,
                on_pass_to TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE RESTRICT,
                on_fail_to TEXT REFERENCES graph_nodes(id) ON DELETE RESTRICT,
                commit_rights INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                mutex TEXT,
                CHECK ((spec_id IS NULL) <> (graph_id IS NULL))
            );

            CREATE TABLE IF NOT EXISTS ensemble_members (
                ensemble_id TEXT NOT NULL REFERENCES ensembles(id) ON DELETE CASCADE,
                node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                platform TEXT NOT NULL,
                model TEXT,
                prompt_override TEXT,
                timeout_minutes INTEGER,
                infra_retry_limit INTEGER,
                infra_crash_max_seconds INTEGER,
                infra_backoff_seconds INTEGER,
                PRIMARY KEY (ensemble_id, node_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_ensemble_members_position
                ON ensemble_members(ensemble_id, position);

            CREATE INDEX IF NOT EXISTS idx_ensemble_members_node
                ON ensemble_members(node_id);

            CREATE INDEX IF NOT EXISTS idx_ensembles_join_node
                ON ensembles(join_node_id);

            -- F1: ensemble blueprints -- a whole ensemble's shared prompt +
            -- member list (unlike `blueprints`, which templates a single
            -- node), seeded with the builtin ensemble-proposers pattern.
            CREATE TABLE IF NOT EXISTS ensemble_blueprints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                prompt_template TEXT NOT NULL,
                members TEXT NOT NULL,
                min_pass INTEGER,
                quorum_grace_minutes INTEGER,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queues (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS queue_members (
                queue_id TEXT NOT NULL REFERENCES queues(id) ON DELETE CASCADE,
                spec_id TEXT NOT NULL REFERENCES graph_specs(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                group_name TEXT,
                PRIMARY KEY (queue_id, spec_id)
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_queue_members_position
                ON queue_members(queue_id, position);

            CREATE TABLE IF NOT EXISTS seed_sessions (
                session_id TEXT PRIMARY KEY,
                seed_id TEXT NOT NULL,
                bound_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES interactive_sessions(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_seed_sessions_seed
                ON seed_sessions(seed_id);

            CREATE TABLE IF NOT EXISTS blueprints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                config TEXT NOT NULL,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS scheduled_sends (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                target_session_id TEXT NOT NULL,
                fire_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                provenance TEXT
            );

            CREATE TABLE IF NOT EXISTS failed_scheduled_sends (
                id TEXT PRIMARY KEY,
                prompt TEXT NOT NULL,
                target_session_id TEXT NOT NULL,
                workdir TEXT,
                failed_at INTEGER NOT NULL,
                provenance TEXT
            );

            -- U8: the prompt builder's last-sent prompt per project, recalled
            -- with Ctrl+L. Insert-only with a timestamp (rather than one row
            -- per workdir) so this can grow into a browsable history later —
            -- today's reads take the most recent row per workdir (LIMIT 1).
            CREATE TABLE IF NOT EXISTS last_prompts (
                id TEXT PRIMARY KEY,
                workdir TEXT NOT NULL,
                prompt_text TEXT NOT NULL,
                builder_state TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_last_prompts_workdir_created
                ON last_prompts(workdir, created_at DESC);

            -- CM5: ephemeral subagent runs. Not `agents` (no trigger, no
            -- schedule, no permanent state) and not `graph_runs` (no spec/graph).
            -- A row lives only until it is collected or its TTL (`expires_at`)
            -- passes; the health routine deletes both on its periodic tick.
            -- CB43: `platform`/`model` below mean RESOLVED at dispatch (the
            -- model actually handed to the CLI argv; NULL when the platform's
            -- `model_flag` cannot select one) — never the requested value when
            -- it was not applied.
            CREATE TABLE IF NOT EXISTS subagent_runs (
                id TEXT PRIMARY KEY,
                platform TEXT NOT NULL,
                model TEXT,
                prompt TEXT NOT NULL,
                workdir TEXT NOT NULL,
                mcp_surface TEXT,
                status TEXT NOT NULL DEFAULT 'running',
                exit_code INTEGER,
                stdout TEXT,
                stderr TEXT,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                collected_at TEXT,
                expires_at TEXT NOT NULL,
                pid INTEGER,
                boot_id TEXT
            );",
    "

            -- CM18: tombstones for blocking `subagent_spawn` deliveries. A
            -- blocking spawn returns its result inline and deletes the
            -- `subagent_runs` row immediately, so a later `subagent_collect`
            -- would otherwise be indistinguishable from never-existed.
            -- A tombstone records the delivered id until the original
            -- `expires_at`, letting `collect` answer already-delivered.
            -- Async rows never touch this table (no change to async
            -- storage or TTL).
            CREATE TABLE IF NOT EXISTS subagent_delivered_tombstones (
                id TEXT PRIMARY KEY,
                delivered_at TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sandbox_runs (
                id TEXT PRIMARY KEY,
                project_hash TEXT NOT NULL,
                base_branch TEXT NOT NULL,
                sandbox_branch TEXT NOT NULL,
                worktree_path TEXT NOT NULL,
                cli_name TEXT NOT NULL,
                original_workdir TEXT NOT NULL,
                owner_type TEXT NOT NULL,
                owner_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'active',
                cleanup_error TEXT
            );

            -- CM48: the availability roster. A person or an agent declares
            -- which platform/model pairs can run; the engine skips a pair that
            -- is effectively unavailable when it dispatches a member, node or
            -- hook. `model_pattern` is NULL (every model of the platform), an
            -- exact model id, or a prefix ending in `*`. `until` (RFC3339 UTC)
            -- is evaluated at read time, never by a timer, so a daemon restart
            -- cannot lose an expiry.
            CREATE TABLE IF NOT EXISTS roster_entries (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                platform TEXT NOT NULL,
                model_pattern TEXT,
                state TEXT NOT NULL CHECK(state IN ('available','unavailable')),
                reason TEXT,
                until TEXT,
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now')),
                UNIQUE(platform, model_pattern)
            );
            CREATE INDEX IF NOT EXISTS idx_roster_entries_platform
                ON roster_entries(platform);",
    "
            -- CM49: crews — named, ordered member lists that ensembles,
            -- agent nodes and agent hooks bind to. `crew_members.position`
            -- is the meaningful order (cascade tries it, round_robin
            -- rotates through it, a bound agent node takes the first
            -- available member). `version` bumps on every member-list
            -- write; bound ensembles record the version they applied.
            CREATE TABLE IF NOT EXISTS crews (
                id TEXT PRIMARY KEY,
                name TEXT UNIQUE NOT NULL,
                description TEXT,
                version INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );
            CREATE TABLE IF NOT EXISTS crew_members (
                crew_id TEXT NOT NULL REFERENCES crews(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                platform TEXT NOT NULL,
                model TEXT,
                timeout_minutes INTEGER,
                infra_retry_limit INTEGER,
                infra_crash_max_seconds INTEGER,
                infra_backoff_seconds INTEGER,
                PRIMARY KEY (crew_id, position)
            );",
    "
            -- CM49 import: entry sources of a crew-bound ensemble whose member
            -- nodes are not materialized yet (see
            -- domain/graph_transfer/pending.rs). The sync path reads them
            -- alongside the `ensembles` row and wires the fan-out once the crew
            -- supplies members, then deletes them.
            CREATE TABLE IF NOT EXISTS ensemble_pending_entry_sources (
                ensemble_id TEXT NOT NULL REFERENCES ensembles(id) ON DELETE CASCADE,
                from_node TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
                condition TEXT NOT NULL,
                PRIMARY KEY (ensemble_id, from_node, condition)
            );",
    mutex_store::MUTEX_TABLES_SCHEMA,
];

impl Database {
    fn init(&self) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        Self::check_schema_version(&conn)?;
        Self::migrate_legacy_queue_schema(&conn)?;
        Self::migrate_loop_to_graph_schema(&conn)?;
        Self::migrate_cb52_ensemble_fk(&conn)?;
        Self::migrate_activity_attribution_prefix(&conn)?;

        Self::create_base_schema(&conn)?;
        Self::migrate_sandbox_cleanup_error_column(&conn)?;
        Self::migrate_operational_sessions_from_intelligence(&conn)?;
        Self::migrate_intelligence_nodes_status_column(&conn)?;
        Self::migrate_scheduled_send_columns(&conn)?;
        Self::migrate_interactive_session_columns(&conn)?;
        Self::migrate_interactive_harness_column(&conn)?;
        Self::migrate_agent_optin_columns(&conn)?;
        Self::migrate_graph_trigger_columns(&conn)?;
        Self::migrate_graph_schedule_columns(&conn)?;
        Self::migrate_spec_start_columns(&conn)?;
        Self::migrate_graph_specs_nullable_graph_id(&conn)?;
        Self::migrate_spec_workdir_column(&conn)?;
        Self::migrate_graph_targeting_rebuild(&conn)?;
        Self::migrate_graph_targeting_indexes(&conn)?;
        Self::migrate_graphs_registry_columns(&conn)?;
        Self::migrate_graph_runs_identity_columns(&conn)?;
        Self::migrate_spec_completed_via_columns(&conn)?;
        Self::migrate_queue_member_group_name_column(&conn)?;
        Self::migrate_edge_route_column(&conn)?;
        Self::migrate_ensemble_member_columns(&conn)?;
        Self::migrate_graph_roster_columns(&conn)?;
        Self::migrate_graph_active_run_idea(&conn)?;
        Self::migrate_spec_commit_evidence_columns(&conn)?;
        Self::migrate_spec_cross_run_attempts_column(&conn)?;
        Self::migrate_spec_type_columns(&conn)?;
        Self::migrate_intelligence_touch_columns(&conn)?;
        Self::migrate_ensemble_execution_columns(&conn)?;
        Self::migrate_graph_run_output_tails(&conn)?;
        Self::migrate_run_paused_through_column(&conn)?;
        Self::migrate_graph_hook_event_columns(&conn)?;
        Self::migrate_graph_hook_config_column(&conn)?;
        Self::migrate_hook_launch_tracking(&conn)?;
        Self::migrate_executed_platform_columns(&conn)?;
        Self::migrate_ensemble_infra_columns(&conn)?;
        Self::migrate_graph_events_table(&conn)?;
        Self::migrate_spec_notes_table(&conn)?;
        Self::migrate_roster_entries_table(&conn)?;
        Self::migrate_crews_tables(&conn)?;
        Self::migrate_agent_action_columns(&conn)?;
        Self::migrate_ensemble_mutex_column(&conn)?;
        Self::migrate_graph_events_mutex_kinds(&conn)?;

        Self::set_schema_version(&conn)?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SubagentRunRecord {
    pub id: String,
    pub platform: String,
    pub model: Option<String>,
    pub prompt: String,
    pub workdir: String,
    pub mcp_surface: Option<String>,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub collected_at: Option<String>,
    pub expires_at: String,
}

/// Parameters for [`Database::insert_subagent_run`]: bundled so the insert
/// stays within the argument-count lint without changing the row it writes.
#[derive(Debug, Clone, Copy)]
pub struct NewSubagentRun<'a> {
    pub id: &'a str,
    pub platform: &'a str,
    pub model: Option<&'a str>,
    pub prompt: &'a str,
    pub workdir: &'a str,
    pub started_at: &'a str,
    pub expires_at: &'a str,
}

pub mod achievements;
pub mod activity_log;
pub mod agent;
pub mod blueprints;
pub mod clean;
pub(crate) mod connection;
pub mod crew_binding;
pub mod crew_bindings;
pub(crate) mod crew_member_config;
pub mod crews;
pub mod ensembles;
pub mod gamification;
pub mod graph_crew;
pub mod graph_transfer;
pub mod graphs;
pub mod group;
pub mod health;
pub mod intelligence;
pub mod last_prompts;
mod migration;
pub mod mutex_store;
pub mod project;
pub mod queues;
pub(crate) mod roster_entries;
pub mod run;
pub mod sandbox;
pub mod scheduled_sends;
pub mod scheduled_sends_agent;
mod schema;
pub mod seeds;
pub mod session;
pub mod session_target;
pub mod state;
mod subagent_runs;
pub mod sync;

#[cfg(test)]
pub use crate::application::ports::{AgentRepository, RunRepository, StateRepository};

#[cfg(test)]
mod agent_action_tests;

#[cfg(test)]
mod tests;
