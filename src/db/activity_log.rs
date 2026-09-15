//! Bitácora: durable activity log stored separately from curated knowledge.
//!
//! The activity panel used to render `sync_messages` only. Those rows are
//! operational chatter, not Knowledge, so nothing could search them. This
//! module persists every activity event into its own `activity_log` table
//! (distinguishable from `intelligence_nodes` from the first commit) and
//! exposes workdir-scoped reads plus a ranked search used by the Knowledge
//! surface.
//!
//! Retention: at most [`MAX_ACTIVITY_ENTRIES_PER_WORKDIR`] entries per
//! workdir, enforced at write time (oldest pruned before insert).

use anyhow::Result;

use crate::db::Database;
use crate::domain::sync::{MessageKind, SyncMessage};

/// Retention rule: how many activity entries are kept per workdir.
///
/// Activity is high-frequency and Knowledge is not a dump for an unbounded
/// stream, so the log is bounded explicitly and pruning runs inside every
/// write (see [`Database::prune_activity_log`]).
pub const MAX_ACTIVITY_ENTRIES_PER_WORKDIR: i64 = 500;

/// One persisted activity event.
///
/// `source` is a plain field (`loop`, `agent`, `hook`, `sync`, `user`) —
/// attribution is structural, never a prefix parsed out of `message`.
#[derive(Debug, Clone)]
pub struct ActivityLogEntry {
    pub id: i64,
    pub workdir: String,
    pub source: String,
    pub source_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub payload: Option<String>,
    pub created_at: i64,
}

fn read_activity_log_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityLogEntry> {
    Ok(ActivityLogEntry {
        id: row.get(0)?,
        workdir: row.get(1)?,
        source: row.get(2)?,
        source_id: row.get(3)?,
        kind: row.get(4)?,
        message: row.get(5)?,
        payload: row.get(6)?,
        created_at: row.get(7)?,
    })
}

impl From<ActivityLogEntry> for SyncMessage {
    fn from(entry: ActivityLogEntry) -> Self {
        SyncMessage {
            id: entry.id,
            workdir: entry.workdir,
            agent_id: entry.source_id.unwrap_or_default(),
            agent_name: entry.source,
            kind: MessageKind::from_str(&entry.kind).unwrap_or(MessageKind::Info),
            message: entry.message,
            payload: entry.payload,
            created_at: entry.created_at,
        }
    }
}

impl Database {
    /// Persist one activity event, pruning the workdir to the retention
    /// limit first so the table never grows without bound.
    pub fn insert_activity_log_entry(
        &self,
        workdir: &str,
        source: &str,
        source_id: Option<&str>,
        kind: &str,
        message: &str,
        payload: Option<&str>,
    ) -> Result<ActivityLogEntry> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let now = chrono::Utc::now().timestamp();
        conn.execute(
            "INSERT INTO activity_log (workdir, source, source_id, kind, message, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![workdir, source, source_id, kind, message, payload, now],
        )?;
        let id = conn.last_insert_rowid();
        drop(conn);
        // Retention is enforced at write time, after the insert, so the
        // steady-state count never exceeds the limit (pruning before the
        // insert would leave limit + 1 rows behind).
        self.prune_activity_log(workdir, MAX_ACTIVITY_ENTRIES_PER_WORKDIR)?;
        Ok(ActivityLogEntry {
            id,
            workdir: workdir.to_owned(),
            source: source.to_owned(),
            source_id: source_id.map(str::to_owned),
            kind: kind.to_owned(),
            message: message.to_owned(),
            payload: payload.map(str::to_owned),
            created_at: now,
        })
    }

    /// Recent entries for one workdir, chronological (oldest first).
    ///
    /// Single indexed query (`idx_activity_log_workdir_created`), bounded by
    /// `limit` so reads stay inside the result budget.
    pub fn list_activity_log_entries(
        &self,
        workdir: &str,
        limit: usize,
    ) -> Result<Vec<ActivityLogEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, workdir, source, source_id, kind, message, payload, created_at
             FROM (
                 SELECT id, workdir, source, source_id, kind, message, payload, created_at
                 FROM activity_log
                 WHERE workdir = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2
             )
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![workdir, limit as i64],
            read_activity_log_entry,
        )?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Recent entries across all workdirs, chronological (oldest first).
    pub fn list_recent_activity_log_entries(&self, limit: usize) -> Result<Vec<ActivityLogEntry>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, workdir, source, source_id, kind, message, payload, created_at
             FROM (
                 SELECT id, workdir, source, source_id, kind, message, payload, created_at
                 FROM activity_log
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?1
             )
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![limit as i64], read_activity_log_entry)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Ranked keyword search over the bitácora.
    ///
    /// Tokenizes the query; ranks by distinct-term match count, then by
    /// where the hit landed (message = 3, payload = 1), then by recency.
    /// `workdir = Some` scopes to one project; `None` searches all.
    pub fn search_activity_log_entries(
        &self,
        query: &str,
        workdir: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ActivityLogEntry>> {
        let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        let mut match_count_parts: Vec<String> = Vec::with_capacity(terms.len());
        let mut score_parts: Vec<String> = Vec::with_capacity(terms.len() * 2);
        let mut or_clauses: Vec<String> = Vec::with_capacity(terms.len() * 2);
        for (i, _) in terms.iter().enumerate() {
            let p = i + 2;
            score_parts.push(format!(
                "(CASE WHEN instr(lower(message), ?{p}) > 0 THEN 3 ELSE 0 END)"
            ));
            score_parts.push(format!(
                "(CASE WHEN instr(lower(coalesce(payload, '')), ?{p}) > 0 THEN 1 ELSE 0 END)"
            ));
            let term_fields: Vec<String> = ["message", "coalesce(payload, '')"]
                .into_iter()
                .map(|field| format!("instr(lower({field}), ?{p}) > 0"))
                .collect();
            match_count_parts.push(format!(
                "(CASE WHEN {} THEN 1 ELSE 0 END)",
                term_fields.join(" OR ")
            ));
            or_clauses.extend(term_fields);
        }
        let match_count_expr = match_count_parts.join(" + ");
        let score_expr = score_parts.join(" + ");
        let or_clause = or_clauses.join(" OR ");
        let limit_placeholder = terms.len() + 2;
        let sql = format!(
            "SELECT id, workdir, source, source_id, kind, message, payload, created_at, \
             ({match_count_expr}) AS match_count, ({score_expr}) AS score \
             FROM activity_log \
             WHERE (?1 IS NULL OR workdir = ?1) AND ({or_clause}) \
             ORDER BY match_count DESC, score DESC, created_at DESC, id DESC \
             LIMIT ?{limit_placeholder}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::with_capacity(terms.len() + 2);
        params.push(Box::new(workdir.map(str::to_string)));
        for term in &terms {
            params.push(Box::new(term.clone()));
        }
        params.push(Box::new(limit as i64));
        let param_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(Box::as_ref).collect();
        let rows = stmt.query_map(param_refs.as_slice(), read_activity_log_entry)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Delete entries beyond `max_entries` for one workdir (oldest first).
    /// Returns the number of rows removed.
    pub fn prune_activity_log(&self, workdir: &str, max_entries: i64) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let deleted = conn.execute(
            "DELETE FROM activity_log WHERE id IN (
                 SELECT id FROM activity_log
                 WHERE workdir = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT -1 OFFSET ?2
             )",
            rusqlite::params![workdir, max_entries],
        )?;
        Ok(deleted)
    }

    /// Convert one bitácora entry into the Knowledge search result shape.
    ///
    /// `kind` is the structural `"activity"` — deliberately NOT in
    /// `KNOWLEDGE_KINDS`, mirroring how `project` stays structural.
    pub fn activity_entry_to_intelligence_record(
        entry: &ActivityLogEntry,
    ) -> crate::db::intelligence::IntelligenceNodeRecord {
        let title = entry.message.chars().take(200).collect::<String>();
        crate::db::intelligence::IntelligenceNodeRecord {
            id: format!("activity:{}", entry.id),
            kind: "activity".to_string(),
            status: "noted".to_string(),
            title,
            body: entry.message.clone(),
            metadata: entry.payload.clone(),
            project_hash: None,
            session_id: entry.source_id.clone(),
            created_at: entry.created_at,
            updated_at: entry.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    #[test]
    fn insert_and_list_activity_log_entries() {
        let db = test_db();
        let workdir = "/tmp/bitacora";
        for i in 1..=3 {
            db.insert_activity_log_entry(
                workdir,
                "sync",
                Some("agent-1"),
                "info",
                &format!("event {i}"),
                None,
            )
            .unwrap();
        }
        let entries = db.list_activity_log_entries(workdir, 10).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].message, "event 1");
        assert_eq!(entries[2].message, "event 3");
    }

    #[test]
    fn list_activity_log_entries_empty() {
        let db = test_db();
        let entries = db
            .list_activity_log_entries("/tmp/nonexistent", 10)
            .unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn prune_activity_log_enforces_limit() {
        let db = test_db();
        let workdir = "/tmp/bitacora-prune";
        for i in 0..MAX_ACTIVITY_ENTRIES_PER_WORKDIR {
            db.insert_activity_log_entry(
                workdir,
                "sync",
                None,
                "info",
                &format!("event {i}"),
                None,
            )
            .unwrap();
        }
        // Next inserts prune at write time: count never exceeds the limit.
        for i in 0..10 {
            db.insert_activity_log_entry(
                workdir,
                "sync",
                None,
                "info",
                &format!("overflow {i}"),
                None,
            )
            .unwrap();
        }
        let entries = db
            .list_activity_log_entries(workdir, (MAX_ACTIVITY_ENTRIES_PER_WORKDIR + 100) as usize)
            .unwrap();
        assert_eq!(entries.len() as i64, MAX_ACTIVITY_ENTRIES_PER_WORKDIR);
        assert!(entries.iter().any(|e| e.message == "overflow 9"));
        assert!(!entries.iter().any(|e| e.message == "event 0"));
    }

    #[test]
    fn search_activity_log_entries() {
        let db = test_db();
        let workdir = "/tmp/bitacora-search";
        db.insert_activity_log_entry(
            workdir,
            "loop",
            Some("loop-1"),
            "info",
            "deploy finished green",
            None,
        )
        .unwrap();
        db.insert_activity_log_entry(
            workdir,
            "agent",
            Some("agent-9"),
            "info",
            "unrelated chatter here",
            None,
        )
        .unwrap();
        let hits = db
            .search_activity_log_entries("deploy green", Some(workdir), 10)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].message, "deploy finished green");
    }

    #[test]
    fn activity_log_entries_are_scoped_by_workdir() {
        let db = test_db();
        db.insert_activity_log_entry("/tmp/proj-a", "sync", None, "info", "alpha event", None)
            .unwrap();
        db.insert_activity_log_entry("/tmp/proj-b", "sync", None, "info", "beta event", None)
            .unwrap();
        let entries = db.list_activity_log_entries("/tmp/proj-a", 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].message, "alpha event");
    }

    #[test]
    fn activity_log_entry_carries_source() {
        let db = test_db();
        let workdir = "/tmp/bitacora-source";
        let entry = db
            .insert_activity_log_entry(
                workdir,
                "loop",
                Some("loop-42"),
                "status",
                "loop finished",
                None,
            )
            .unwrap();
        assert_eq!(entry.source, "loop");
        assert_eq!(entry.source_id.as_deref(), Some("loop-42"));
        let entries = db.list_activity_log_entries(workdir, 10).unwrap();
        assert_eq!(entries[0].source, "loop");
        assert_eq!(entries[0].source_id.as_deref(), Some("loop-42"));
    }
}
