use anyhow::Result;
use rusqlite::params;

use crate::db::Database;
use crate::domain::project::{workdir_hash, RemapCounts, RemapKind};

use super::RemapOutcome;

impl Database {
    /// Preview what [`Database::remap_project`] would do, without changing
    /// anything: which rows would move and whether it's a MOVE or a MERGE.
    /// `new_canonical_path` must already be resolved (see
    /// [`resolve_remap_path`]) — this function does no filesystem I/O.
    pub fn remap_preview(&self, old_hash: &str, new_canonical_path: &str) -> Result<RemapOutcome> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.unchecked_transaction()?;

        let old_path: String = tx
            .query_row(
                "SELECT path FROM projects WHERE hash = ?1",
                params![old_hash],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("No project registered with hash '{old_hash}'"))?;

        let new_hash = workdir_hash(new_canonical_path);
        if new_hash == old_hash {
            anyhow::bail!(
                "New path resolves to the same project (hash unchanged) — nothing to remap"
            );
        }

        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE hash = ?1)",
            params![new_hash],
            |row| row.get::<_, i64>(0),
        )? != 0;
        let counts = count_remap_targets(&tx, &old_path, old_hash)?;

        Ok(RemapOutcome {
            kind: RemapKind::decide(target_exists),
            old_hash: old_hash.to_string(),
            new_hash,
            new_path: new_canonical_path.to_string(),
            counts,
        })
    }

    /// Remap a project registered at `old_hash` onto `new_canonical_path`:
    /// re-key every dependent row (interactive/terminal sessions, graphs,
    /// standalone specs, sync state, prompts, scheduled sends, agents,
    /// intelligence nodes) from the old path/hash to the new one, in a
    /// single transaction. If no project is registered at the new path
    /// (MOVE), the project row itself is updated in place; if one already
    /// exists (MERGE), dependents are folded into it and the stale row is
    /// removed. `new_canonical_path` must already be resolved (see
    /// [`resolve_remap_path`]) — this function does no filesystem I/O.
    ///
    /// A crash mid-transaction leaves every dependent attached to exactly
    /// one project: either the whole re-key applied (and committed) or none
    /// of it did.
    pub fn remap_project(&self, old_hash: &str, new_canonical_path: &str) -> Result<RemapOutcome> {
        // Scope the connection guard: the post-commit containment rebuild
        // re-locks the connection, so the guard must be dropped first
        // (a held std Mutex guard here would deadlock).
        let outcome = {
            let mut conn = self
                .conn
                .lock()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            let tx = conn.transaction()?;

            let outcome = Self::remap_project_tx(&tx, old_hash, new_canonical_path)?;
            tx.commit()?;
            outcome
        };

        if let Err(e) = self.rebuild_containment_edges() {
            tracing::debug!("rebuild_containment_edges after remap failed: {e}");
        }

        Ok(outcome)
    }

    /// The transaction body of [`Database::remap_project`], extracted so the
    /// sandbox-attribution migration (`migrate_sandbox_intelligence_attribution`)
    /// can reuse the exact same re-keying inside its own transaction. Caller
    /// commits (or rolls back); `new_canonical_path` must already be resolved —
    /// this function does no filesystem I/O.
    pub(in crate::db) fn remap_project_tx(
        tx: &rusqlite::Transaction<'_>,
        old_hash: &str,
        new_canonical_path: &str,
    ) -> Result<RemapOutcome> {
        let old_path: String = tx
            .query_row(
                "SELECT path FROM projects WHERE hash = ?1",
                params![old_hash],
                |row| row.get(0),
            )
            .map_err(|_| anyhow::anyhow!("No project registered with hash '{old_hash}'"))?;

        let new_hash = workdir_hash(new_canonical_path);
        if new_hash == old_hash {
            anyhow::bail!(
                "New path resolves to the same project (hash unchanged) — nothing to remap"
            );
        }

        let target_exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM projects WHERE hash = ?1)",
            params![new_hash],
            |row| row.get::<_, i64>(0),
        )? != 0;
        let kind = RemapKind::decide(target_exists);
        let counts = count_remap_targets(tx, &old_path, old_hash)?;

        Self::rekey_project_dependents(tx, &old_path, old_hash, new_canonical_path, &new_hash)?;

        // Keep the `project:{hash}` graph root in sync with the hash re-key:
        // CASCADE won't fire here (no row delete in MOVE), so rename
        // explicitly. MERGE drops the stale root (edges CASCADE).
        let old_node_id = format!("project:{old_hash}");
        let new_node_id = format!("project:{new_hash}");
        let old_node: Option<(String, String, Option<String>)> = match tx.query_row(
            "SELECT title, body, metadata FROM intelligence_nodes WHERE id = ?1",
            params![old_node_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ) {
            Ok(v) => Some(v),
            Err(rusqlite::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        match kind {
            RemapKind::Move => {
                Self::move_project_node_root(
                    tx,
                    &old_node_id,
                    &new_node_id,
                    &new_hash,
                    old_node,
                    new_canonical_path,
                )?;
            }
            RemapKind::Merge => {
                tx.execute(
                    "DELETE FROM intelligence_nodes WHERE id = ?1",
                    params![old_node_id],
                )?;
            }
        }

        match kind {
            RemapKind::Move => {
                tx.execute(
                    "UPDATE projects SET hash = ?2, path = ?3 WHERE hash = ?1",
                    params![old_hash, new_hash, new_canonical_path],
                )?;
            }
            RemapKind::Merge => {
                tx.execute("DELETE FROM projects WHERE hash = ?1", params![old_hash])?;
            }
        }

        Ok(RemapOutcome {
            kind,
            old_hash: old_hash.to_string(),
            new_hash,
            new_path: new_canonical_path.to_string(),
            counts,
        })
    }

    /// Re-key every dependent row of a remapped project from `old_path`/`old_hash`
    /// to the new path/hash. Statement order is integrity — never reorder.
    fn rekey_project_dependents(
        tx: &rusqlite::Transaction,
        old_path: &str,
        old_hash: &str,
        new_canonical_path: &str,
        new_hash: &str,
    ) -> Result<()> {
        tx.execute(
            "UPDATE interactive_sessions SET working_dir = ?2 WHERE working_dir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE terminal_sessions SET working_dir = ?2 WHERE working_dir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE graphs SET workdir = ?2 WHERE workdir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE graph_specs SET workdir = ?2, updated_at = ?3 WHERE workdir = ?1",
            params![
                old_path,
                new_canonical_path,
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        tx.execute(
            "UPDATE sync_messages SET workdir = ?2 WHERE workdir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE sync_locks SET workdir = ?2 WHERE workdir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE last_prompts SET workdir = ?2 WHERE workdir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE scheduled_sends SET workdir = ?2 WHERE workdir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE failed_scheduled_sends SET workdir = ?2 WHERE workdir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE agents SET working_dir = ?2 WHERE working_dir = ?1",
            params![old_path, new_canonical_path],
        )?;
        tx.execute(
            "UPDATE intelligence_nodes SET project_hash = ?2 WHERE project_hash = ?1",
            params![old_hash, new_hash],
        )?;
        Ok(())
    }

    /// Metadata for the `project:{hash}` root when the project moves: the old
    /// metadata with its `path` rewritten, or a fresh registry blob.
    fn rekeyed_project_node_metadata(
        old_metadata: Option<&str>,
        new_canonical_path: &str,
    ) -> String {
        let Some(mut value) =
            old_metadata.and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
        else {
            return serde_json::json!({
                "source": "registry",
                "path": new_canonical_path,
            })
            .to_string();
        };
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "path".to_string(),
                serde_json::Value::String(new_canonical_path.to_string()),
            );
        }
        value.to_string()
    }

    /// Move a project's `project:{hash}` root node to the new hash: rewrite its
    /// metadata path, re-insert under the new id, and re-point its edges.
    fn move_project_node_root(
        tx: &rusqlite::Transaction,
        old_node_id: &str,
        new_node_id: &str,
        new_hash: &str,
        node: Option<(String, String, Option<String>)>,
        new_canonical_path: &str,
    ) -> Result<()> {
        let Some((title, body, metadata)) = node else {
            return Ok(());
        };
        let new_metadata =
            Self::rekeyed_project_node_metadata(metadata.as_deref(), new_canonical_path);
        tx.execute(
            "DELETE FROM intelligence_nodes WHERE id = ?1",
            params![old_node_id],
        )?;
        tx.execute(
            "INSERT INTO intelligence_nodes (id, kind, title, body, metadata, project_hash, session_id, created_at, updated_at)
             VALUES (?1, 'project', ?2, ?3, ?4, ?5, NULL, strftime('%s','now'), strftime('%s','now'))",
            params![new_node_id, title, body, new_metadata, new_hash],
        )?;
        tx.execute(
            "UPDATE intelligence_edges SET from_node_id = ?2 WHERE from_node_id = ?1",
            params![old_node_id, new_node_id],
        )?;
        tx.execute(
            "UPDATE intelligence_edges SET to_node_id = ?2 WHERE to_node_id = ?1",
            params![old_node_id, new_node_id],
        )?;
        Ok(())
    }
}

/// Shared counting logic for [`Database::remap_preview`] (read-only) and
/// [`Database::remap_project`] (counted just before the re-key runs, inside
/// the same transaction) — both call sites must see identical numbers, so
/// this is the only place the counting SQL lives. Every project-scoped table
/// found by grepping for a `workdir`/`working_dir`/`project_hash` column
/// (see `src/db/mod.rs`'s schema) is covered here.
fn count_remap_targets(
    tx: &rusqlite::Transaction,
    old_path: &str,
    old_hash: &str,
) -> Result<RemapCounts> {
    let interactive_sessions: i64 = tx.query_row(
        "SELECT COUNT(*) FROM interactive_sessions WHERE working_dir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let terminal_sessions: i64 = tx.query_row(
        "SELECT COUNT(*) FROM terminal_sessions WHERE working_dir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let graphs: i64 = tx.query_row(
        "SELECT COUNT(*) FROM graphs WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let graph_specs: i64 = tx.query_row(
        "SELECT COUNT(*) FROM graph_specs WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let sync_messages: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sync_messages WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let sync_locks: i64 = tx.query_row(
        "SELECT COUNT(*) FROM sync_locks WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let last_prompts: i64 = tx.query_row(
        "SELECT COUNT(*) FROM last_prompts WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let scheduled_sends: i64 = tx.query_row(
        "SELECT COUNT(*) FROM scheduled_sends WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let failed_scheduled_sends: i64 = tx.query_row(
        "SELECT COUNT(*) FROM failed_scheduled_sends WHERE workdir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let agents: i64 = tx.query_row(
        "SELECT COUNT(*) FROM agents WHERE working_dir = ?1",
        params![old_path],
        |row| row.get(0),
    )?;
    let intelligence_nodes: i64 = tx.query_row(
        "SELECT COUNT(*) FROM intelligence_nodes WHERE project_hash = ?1",
        params![old_hash],
        |row| row.get(0),
    )?;

    Ok(RemapCounts {
        interactive_sessions,
        terminal_sessions,
        graphs,
        graph_specs,
        sync_messages,
        sync_locks,
        last_prompts,
        scheduled_sends,
        failed_scheduled_sends,
        agents,
        intelligence_nodes,
    })
}
