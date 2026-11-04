#![allow(dead_code)]
//! SQLite repositories for projects and RAG queue.
//!
//! RAG chunks and embeddings are stored in LanceDB (`~/.canopy/rag/vectors.lancedb`).
//! Only the indexing queue (`rag_queue`) remains in SQLite for coordination.

mod activity;
mod history;
mod rag_events;
mod rag_queue;
mod remap;

pub use activity::ProjectActivity;

use anyhow::Result;
use std::path::Path;

use crate::db::Database;
use crate::domain::project::{extract_readme_description, Project, RemapCounts, RemapKind};

#[cfg(test)]
use crate::domain::project::workdir_hash;
#[cfg(test)]
use history::parse_rfc3339_timestamp;

/// Outcome of a project remap (real or previewed): which case it was, the
/// hashes/path involved, and the per-table row counts touched.
#[derive(Debug, Clone)]
pub struct RemapOutcome {
    pub kind: RemapKind,
    pub old_hash: String,
    pub new_hash: String,
    pub new_path: String,
    pub counts: RemapCounts,
}

/// Resolve `path` to the canonical form a remap should key dependents on.
/// Refuses a path that doesn't exist on disk unless `force` is set, in which
/// case the path is absolutized (relative to cwd) but left otherwise
/// unverified — there's nothing on disk to canonicalize against.
pub fn resolve_remap_path(path: &Path, force: bool) -> Result<String> {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Ok(canonical.to_string_lossy().to_string()),
        Err(_) if force => {
            let abs = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            Ok(abs.to_string_lossy().to_string())
        }
        Err(e) => Err(anyhow::anyhow!(
            "New path {} does not exist ({e}); use --force to remap anyway",
            path.display()
        )),
    }
}

#[derive(Debug, Clone)]
pub struct RagQueueItem {
    pub source_path: String,
    pub status: String,
    pub queued_at: i64,
}

/// One row in a project's persisted History tab: a finished graph or a past
/// (no longer live) interactive/terminal session, scoped to the project's
/// workdir and ordered newest-first. Unlike the live agent/graph providers
/// (which only know about what's running in *this* TUI process), this reads
/// straight from SQLite so history survives a restart.
#[derive(Debug, Clone)]
pub struct ProjectHistoryEntry {
    pub kind: ProjectHistoryKind,
    pub name: String,
    pub status: String,
    /// Unix timestamp used for sorting and relative-time display.
    pub at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectHistoryKind {
    Graph,
    InteractiveSession,
    TerminalSession,
}

#[derive(Debug, Clone, Default)]
pub struct RagInfoSummary {
    pub total_chunks: i64,
    pub indexed_files: i64,
    pub queued_items: i64,
    pub processing_items: i64,
}

impl RagInfoSummary {
    pub fn has_rag_activity(&self) -> bool {
        self.total_chunks > 0 || self.queued_items > 0 || self.processing_items > 0
    }
}

impl Database {
    // ── projects ───────────────────────────────────────────────────────

    pub fn upsert_project(&self, p: &Project) -> Result<()> {
        {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            conn.execute(
                "INSERT INTO projects (hash, path, name, description, tags, indexed_at, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(hash) DO UPDATE SET
                   path=excluded.path,
                   name=excluded.name,
                   description=COALESCE(description, excluded.description),
                   tags=COALESCE(tags, excluded.tags),
                   indexed_at=COALESCE(projects.indexed_at, excluded.indexed_at)",
                rusqlite::params![
                    p.hash,
                    p.path,
                    p.name,
                    p.description,
                    p.tags,
                    p.indexed_at,
                    p.created_at
                ],
            )?;
        }

        // Keep the intelligence graph root in sync: every registered project
        // must exist as a kind='project' node or link_projects and the TUI
        // relation picker have nothing to operate on.
        self.ensure_project_node(p)?;
        // Recompute derived containment (best-effort: never fail a register
        // on a graph nicety).
        if let Err(e) = self.rebuild_containment_edges() {
            tracing::debug!("rebuild_containment_edges after upsert failed: {e}");
        }
        Ok(())
    }

    pub fn register_project_path(&self, path: &Path) -> Result<Project> {
        // A path inside a sandbox worktree registers the project the sandbox
        // was cut from — a sandbox never registers itself as a project.
        // `register_project_explicit` inherits this mapping.
        let mapped = self.map_workdir_out_of_sandbox(&path.to_string_lossy())?;
        let canonical = std::fs::canonicalize(&mapped)?;
        let canonical_str = canonical.to_string_lossy().to_string();
        let mut project = Project::new(&canonical_str);

        let readme_path = canonical.join("README.md");
        if readme_path.exists() {
            let readme = std::fs::read_to_string(&readme_path)?;
            project.description = extract_readme_description(&readme);
        }

        self.upsert_project(&project)?;
        Ok(project)
    }

    /// Explicit registration entry point (the spec's "explicit call"):
    /// canonicalizes `path` and registers it, marker file or not.
    pub fn register_project_explicit(&self, path: &Path) -> Result<Project> {
        self.register_project_path(path)
    }

    pub fn delete_project(&self, hash: &str) -> Result<()> {
        {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            conn.execute(
                "DELETE FROM projects WHERE hash=?1",
                rusqlite::params![hash],
            )?;
            // Remove the graph root node; incident edges CASCADE via FKs.
            conn.execute(
                "DELETE FROM intelligence_nodes WHERE id=?1",
                rusqlite::params![format!("project:{hash}")],
            )?;
        }
        if let Err(e) = self.rebuild_containment_edges() {
            tracing::debug!("rebuild_containment_edges after delete failed: {e}");
        }
        Ok(())
    }

    pub fn unregister_project_path(&self, path: &Path) -> Result<()> {
        if let Ok(Some(project)) = self.get_project_by_path(path) {
            self.delete_project(&project.hash)?;
        }
        Ok(())
    }

    pub fn get_project(&self, hash: &str) -> Result<Option<Project>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT hash,path,name,description,tags,indexed_at,created_at
             FROM projects WHERE hash=?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![hash], row_to_project)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_project_by_path(&self, path: &Path) -> Result<Option<Project>> {
        let canonical = std::fs::canonicalize(path)?;
        let canonical_str = canonical.to_string_lossy().to_string();
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT hash,path,name,description,tags,indexed_at,created_at
             FROM projects WHERE path=?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![canonical_str], row_to_project)?;
        Ok(rows.next().transpose()?)
    }

    pub fn get_project_by_path_or_ancestor(&self, path: &Path) -> Result<Option<Project>> {
        let canonical = std::fs::canonicalize(path)?;
        let projects = self.list_projects()?;

        Ok(projects
            .into_iter()
            .filter_map(|project| {
                let project_path = std::path::PathBuf::from(&project.path);
                canonical
                    .starts_with(&project_path)
                    .then_some((project_path.components().count(), project))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, project)| project))
    }

    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT hash,path,name,description,tags,indexed_at,created_at
             FROM projects ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_project)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn search_projects(&self, query: &str) -> Result<Vec<Project>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let pattern = format!("%{}%", query.to_lowercase());
        let mut stmt = conn.prepare(
            "SELECT hash,path,name,description,tags,indexed_at,created_at FROM projects
             WHERE lower(name) LIKE ?1 OR lower(description) LIKE ?1
             ORDER BY created_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], row_to_project)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn update_project_meta(
        &self,
        hash: &str,
        description: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let tags_str = tags.map(|t| t.join(","));
        let n = conn.execute(
            "UPDATE projects SET
               description = COALESCE(?2, description),
               tags = COALESCE(?3, tags)
             WHERE hash = ?1",
            rusqlite::params![hash, description, tags_str],
        )?;
        Ok(n > 0)
    }

    pub fn mark_project_indexed(&self, hash: &str, indexed_at: i64) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let n = conn.execute(
            "UPDATE projects SET indexed_at = ?2 WHERE hash = ?1",
            rusqlite::params![hash, indexed_at],
        )?;
        Ok(n > 0)
    }
}

fn row_to_project(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        hash: row.get(0)?,
        path: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        tags: row.get(4)?,
        indexed_at: row.get(5)?,
        created_at: row.get(6)?,
    })
}

// ── rag_file_events ────────────────────────────────────────────────────────

/// A recorded lifecycle event for an indexed file.
#[derive(Debug, Clone)]
pub struct RagFileEvent {
    pub id: i64,
    pub file_path: String,
    /// `"indexed"` | `"deleted"` | `"error"` | `"failed"` (permanent give-up)
    /// | `"skipped_oversize"` (exceeds `FILE_MAX_BYTES`, `detail` holds size in bytes)
    pub event_type: String,
    pub detail: Option<String>,
    pub occurred_at: i64,
}

// ── RagPerFileStatus ────────────────────────────────────────────────────────

/// Aggregated per-file RAG status for the TUI preview panel.
#[derive(Debug, Clone)]
pub struct RagPerFileStatus {
    pub file_path: String,
    /// Last recorded event type: `"indexed"` | `"deleted"` | `"error"`
    pub last_event_type: String,
    /// Detail from the last event (error message, etc.)
    pub last_detail: Option<String>,
    /// How many times this file has been successfully indexed.
    pub times_indexed: i64,
    /// Timestamp of the most recent event.
    pub last_at: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::AgentRepository;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn sample_project(hash: &str) -> Project {
        Project {
            hash: hash.to_string(),
            path: format!("/tmp/{hash}"),
            name: format!("Project {hash}"),
            description: Some("Test project".to_string()),
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        }
    }

    #[test]
    fn upsert_and_get_project() {
        let db = test_db();
        let project = sample_project("abc123");
        db.upsert_project(&project).unwrap();

        let retrieved = db.get_project("abc123").unwrap();
        assert!(retrieved.is_some());
        let retrieved = retrieved.unwrap();
        assert_eq!(retrieved.hash, "abc123");
        assert_eq!(retrieved.name, "Project abc123");
    }

    #[test]
    fn get_project_not_found() {
        let db = test_db();
        let result = db.get_project("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_projects_empty() {
        let db = test_db();
        let projects = db.list_projects().unwrap();
        assert!(projects.is_empty());
    }

    #[test]
    fn list_projects_with_projects() {
        let db = test_db();
        let project1 = sample_project("proj1");
        let project2 = sample_project("proj2");
        db.upsert_project(&project1).unwrap();
        db.upsert_project(&project2).unwrap();

        let projects = db.list_projects().unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn delete_project() {
        let db = test_db();
        let project = sample_project("test-proj");
        db.upsert_project(&project).unwrap();

        db.delete_project("test-proj").unwrap();
        let retrieved = db.get_project("test-proj").unwrap();
        assert!(retrieved.is_none());
    }

    #[test]
    fn update_project_description() {
        let db = test_db();
        let project = sample_project("test-proj");
        db.upsert_project(&project).unwrap();

        db.update_project_meta("test-proj", Some("New description"), None)
            .unwrap();
        let retrieved = db.get_project("test-proj").unwrap().unwrap();
        assert_eq!(retrieved.description, Some("New description".to_string()));
    }

    #[test]
    fn search_projects_by_name() {
        let db = test_db();
        let project1 = Project {
            hash: "proj1".to_string(),
            path: "/tmp/proj1".to_string(),
            name: "Rust Project".to_string(),
            description: Some("A Rust project".to_string()),
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        let project2 = Project {
            hash: "proj2".to_string(),
            path: "/tmp/proj2".to_string(),
            name: "Python Project".to_string(),
            description: Some("A Python project".to_string()),
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        db.upsert_project(&project1).unwrap();
        db.upsert_project(&project2).unwrap();

        let results = db.search_projects("Rust").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].hash, "proj1");
    }

    #[test]
    fn search_projects_by_description() {
        let db = test_db();
        let project = Project {
            hash: "proj1".to_string(),
            path: "/tmp/proj1".to_string(),
            name: "My Project".to_string(),
            description: Some("Contains Rust code".to_string()),
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        };
        db.upsert_project(&project).unwrap();

        let results = db.search_projects("Rust").unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn parse_rfc3339_timestamp_valid() {
        let ts = parse_rfc3339_timestamp("2024-01-15T10:30:00Z");
        assert!(ts > 0);
    }

    #[test]
    fn parse_rfc3339_timestamp_invalid() {
        let ts = parse_rfc3339_timestamp("invalid");
        assert_eq!(ts, 0);
    }

    // ── resolve_remap_path ──────────────────────────────────────────────

    #[test]
    fn resolve_remap_path_canonicalizes_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let resolved = resolve_remap_path(dir.path(), false).unwrap();
        assert_eq!(
            resolved,
            std::fs::canonicalize(dir.path()).unwrap().to_string_lossy()
        );
    }

    #[test]
    fn resolve_remap_path_refuses_missing_dir_without_force() {
        let err = resolve_remap_path(Path::new("/definitely/does/not/exist"), false).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn resolve_remap_path_allows_missing_dir_with_force() {
        let resolved = resolve_remap_path(Path::new("/definitely/does/not/exist"), true).unwrap();
        assert_eq!(resolved, "/definitely/does/not/exist");
    }

    // ── remap_project / remap_preview ───────────────────────────────────

    fn seed_full_dependents(db: &Database, hash: &str, workdir: &str) {
        db.insert_interactive_session(
            "sess-1",
            "sess-1",
            "opencode",
            workdir,
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.insert_terminal_session("term-1", "term-1", "bash", workdir)
            .unwrap();
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "graph-1".to_string(),
            name: "graph-1".to_string(),
            description: None,
            workdir: workdir.to_string(),
            status: crate::domain::graphs::GraphStatus::Completed,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
        db.insert_graph_spec(&crate::domain::graphs::GraphSpec {
            id: "spec-standalone-1".to_string(),
            graph_id: None,
            name: "standalone".to_string(),
            description: None,
            position: 0,
            parallelizable: false,
            status: crate::domain::graphs::GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: Some(workdir.to_string()),
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
        db.insert_sync_message(
            workdir,
            "agent-1",
            "Agent One",
            crate::domain::sync::MessageKind::Info,
            "hello",
            None,
        )
        .unwrap();
        db.insert_sync_lock_for_test("lock-1", workdir).unwrap();
        db.insert_last_prompt(
            "prompt-1",
            workdir,
            "do the thing",
            None,
            chrono::Utc::now(),
        )
        .unwrap();
        db.insert_scheduled_prompt(
            "send-1",
            "ping",
            "sess-1",
            Some(workdir),
            chrono::Utc::now(),
            None,
            None,
        )
        .unwrap();
        db.insert_failed_scheduled_prompt(
            "failed-1",
            "ping",
            "sess-1",
            Some(workdir),
            chrono::Utc::now(),
            None,
        )
        .unwrap();
        db.upsert_agent(&crate::domain::models::Agent {
            id: "agent-bg-1".to_string(),
            prompt: "do stuff".to_string(),
            trigger: None,
            cli: crate::domain::models::Cli::new("opencode"),
            model: None,
            effort: None,
            working_dir: Some(workdir.to_string()),
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: "/tmp/agent-bg-1.log".to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
            action: crate::domain::models::AgentAction::Headless,
        })
        .unwrap();
        db.upsert_intelligence_node(crate::db::intelligence::IntelligenceNodeInput {
            id: None,
            kind: Some("fact".to_string()),
            status: None,
            title: Some("a fact".to_string()),
            body: Some("body".to_string()),
            body_replace: None,
            metadata: None,
            project_hash: Some(Some(hash.to_string())),
            session_id: None,
            relations: None,
        })
        .unwrap();
    }

    #[test]
    fn remap_project_move_rekeys_every_dependent_table() {
        let db = test_db();
        let old_hash = "hash-old";
        let old_path = "/proj/old-location";
        let new_path = "/proj/new-location";

        db.upsert_project(&sample_project_at(old_hash, old_path))
            .unwrap();
        seed_full_dependents(&db, old_hash, old_path);

        let outcome = db.remap_project(old_hash, new_path).unwrap();

        assert_eq!(outcome.kind, RemapKind::Move);
        assert_eq!(outcome.old_hash, old_hash);
        assert_eq!(outcome.new_hash, workdir_hash(new_path));
        assert_eq!(outcome.new_path, new_path);
        // Every seeded table contributed exactly one dependent row.
        assert_eq!(outcome.counts.interactive_sessions, 1);
        assert_eq!(outcome.counts.terminal_sessions, 1);
        assert_eq!(outcome.counts.graphs, 1);
        assert_eq!(outcome.counts.graph_specs, 1);
        assert_eq!(outcome.counts.sync_messages, 1);
        assert_eq!(outcome.counts.sync_locks, 1);
        assert_eq!(outcome.counts.last_prompts, 1);
        assert_eq!(outcome.counts.scheduled_prompts, 1);
        assert_eq!(outcome.counts.failed_scheduled_prompts, 1);
        assert_eq!(outcome.counts.agents, 1);
        // The project-root node `upsert_project` auto-creates, plus the fact
        // node `seed_full_dependents` inserts.
        assert_eq!(outcome.counts.intelligence_nodes, 2);
        assert_eq!(outcome.counts.total(), 12);

        // The project kept its identity but moved hash/path.
        assert!(db.get_project(old_hash).unwrap().is_none());
        let moved = db.get_project(&outcome.new_hash).unwrap().unwrap();
        assert_eq!(moved.path, new_path);

        // Every dependent now points at the new path/hash, and nothing is
        // left behind at the old one.
        assert_eq!(db.project_dependent_counts(old_path).unwrap().graphs, 0);
        assert_eq!(db.project_dependent_counts(new_path).unwrap().graphs, 1);
        assert_eq!(
            db.project_dependent_counts(new_path)
                .unwrap()
                .interactive_sessions,
            1
        );
        assert_eq!(
            db.project_dependent_counts(new_path)
                .unwrap()
                .terminal_sessions,
            1
        );
        let agent = db.get_agent("agent-bg-1").unwrap().unwrap();
        assert_eq!(agent.working_dir.as_deref(), Some(new_path));
    }

    #[test]
    fn remap_project_merge_reassigns_dependents_and_removes_stale_row() {
        let db = test_db();
        let old_hash = "hash-cadforge";
        let old_path = "/proj/cadforge";
        let new_path = "/proj/cadspec";
        let new_hash = workdir_hash(new_path);

        db.upsert_project(&sample_project_at(old_hash, old_path))
            .unwrap();
        seed_full_dependents(&db, old_hash, old_path);
        // The rename target is already a registered project (the C2 orphan
        // scenario from the spec: cadforge renamed to cadspec, and cadspec
        // was separately re-registered under its own hash).
        db.upsert_project(&sample_project_at(&new_hash, new_path))
            .unwrap();

        let outcome = db.remap_project(old_hash, new_path).unwrap();

        assert_eq!(outcome.kind, RemapKind::Merge);
        assert_eq!(outcome.new_hash, new_hash);
        assert_eq!(outcome.counts.total(), 12);

        // Exactly one project row survives, at the new hash.
        assert!(db.get_project(old_hash).unwrap().is_none());
        assert!(db.get_project(&new_hash).unwrap().is_some());
        assert_eq!(
            db.list_projects()
                .unwrap()
                .iter()
                .filter(|p| p.path == new_path || p.path == old_path)
                .count(),
            1
        );

        // Dependents were reassigned to the new path/hash.
        assert_eq!(db.project_dependent_counts(new_path).unwrap().graphs, 1);
        assert_eq!(db.project_dependent_counts(old_path).unwrap().graphs, 0);
    }

    #[test]
    fn remap_project_unrelated_project_left_intact() {
        let db = test_db();
        db.upsert_project(&sample_project_at("hash-a", "/proj-a"))
            .unwrap();
        db.upsert_project(&sample_project_at("hash-b", "/proj-b"))
            .unwrap();
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "graph-b".to_string(),
            name: "graph-b".to_string(),
            description: None,
            workdir: "/proj-b".to_string(),
            status: crate::domain::graphs::GraphStatus::Completed,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();

        db.remap_project("hash-a", "/proj-a-moved").unwrap();

        assert!(db.get_project("hash-b").unwrap().is_some());
        assert_eq!(db.project_dependent_counts("/proj-b").unwrap().graphs, 1);
    }

    #[test]
    fn remap_project_errors_when_hash_not_registered() {
        let db = test_db();
        let err = db.remap_project("no-such-hash", "/proj/new").unwrap_err();
        assert!(err.to_string().contains("no-such-hash"));
    }

    #[test]
    fn remap_project_refuses_when_new_path_resolves_to_same_project() {
        let db = test_db();
        let path = "/proj/unchanged";
        let hash = workdir_hash(path);
        db.upsert_project(&sample_project_at(&hash, path)).unwrap();

        let err = db.remap_project(&hash, path).unwrap_err();
        assert!(err.to_string().contains("same project"));
    }

    #[test]
    fn remap_project_move_tolerates_a_missing_project_root_node() {
        let db = test_db();
        let old_hash = "hash-no-root";
        let old_path = "/proj/no-root";
        let new_path = "/proj/no-root-moved";

        db.upsert_project(&sample_project_at(old_hash, old_path))
            .unwrap();
        db.delete_intelligence_node(&format!("project:{old_hash}"))
            .unwrap();

        let outcome = db.remap_project(old_hash, new_path).unwrap();

        assert_eq!(outcome.kind, RemapKind::Move);
        assert!(db.get_project(old_hash).unwrap().is_none());
        assert!(db.get_project(&workdir_hash(new_path)).unwrap().is_some());
    }

    #[test]
    fn remap_preview_reports_counts_without_changing_anything() {
        let db = test_db();
        let old_hash = "hash-preview";
        let old_path = "/proj/preview-old";
        let new_path = "/proj/preview-new";

        db.upsert_project(&sample_project_at(old_hash, old_path))
            .unwrap();
        seed_full_dependents(&db, old_hash, old_path);

        let preview = db.remap_preview(old_hash, new_path).unwrap();
        assert_eq!(preview.kind, RemapKind::Move);
        assert_eq!(preview.counts.total(), 12);

        // Nothing actually moved.
        assert!(db.get_project(old_hash).unwrap().is_some());
        assert!(db.get_project(&workdir_hash(new_path)).unwrap().is_none());
        assert_eq!(db.project_dependent_counts(old_path).unwrap().graphs, 1);
        assert_eq!(db.project_dependent_counts(new_path).unwrap().graphs, 0);

        // A real remap right after reports the same counts.
        let applied = db.remap_project(old_hash, new_path).unwrap();
        assert_eq!(applied.counts, preview.counts);
    }

    #[test]
    fn remap_preview_merge_kind_when_target_already_registered() {
        let db = test_db();
        let old_hash = "hash-preview-merge";
        let old_path = "/proj/preview-merge-old";
        let new_path = "/proj/preview-merge-new";
        let new_hash = workdir_hash(new_path);

        db.upsert_project(&sample_project_at(old_hash, old_path))
            .unwrap();
        db.upsert_project(&sample_project_at(&new_hash, new_path))
            .unwrap();

        let preview = db.remap_preview(old_hash, new_path).unwrap();
        assert_eq!(preview.kind, RemapKind::Merge);

        // Still just previewing: both project rows remain.
        assert!(db.get_project(old_hash).unwrap().is_some());
        assert!(db.get_project(&new_hash).unwrap().is_some());
    }

    fn sample_project_at(hash: &str, path: &str) -> Project {
        Project {
            hash: hash.to_string(),
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        }
    }
}
