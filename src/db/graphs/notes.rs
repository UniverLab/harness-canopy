use anyhow::{anyhow, Result};
use rusqlite::params;

use crate::db::Database;

/// CM47: hard cap on one spec note body, in characters.
pub const SPEC_NOTE_MAX_CHARS: usize = 2500;

/// One `spec_notes` row. `updated_at` is RFC3339; rows arrive newest-first
/// from [`Database::spec_notes_get`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecNoteRow {
    pub owner_kind: String,
    pub owner_id: String,
    pub owner_name: String,
    pub body: String,
    pub author_platform: Option<String>,
    pub author_model: Option<String>,
    pub updated_at: String,
}

fn read_spec_note_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SpecNoteRow> {
    Ok(SpecNoteRow {
        owner_kind: row.get(0)?,
        owner_id: row.get(1)?,
        owner_name: row.get(2)?,
        body: row.get(3)?,
        author_platform: row.get(4)?,
        author_model: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

/// Parameters for [`Database::spec_note_write`]: bundled so the insert stays
/// within the argument-count lint without changing the row it writes.
#[derive(Debug, Clone, Copy)]
pub struct NewSpecNote<'a> {
    pub spec_id: &'a str,
    pub owner_kind: &'a str,
    pub owner_id: &'a str,
    pub owner_name: &'a str,
    pub body: &'a str,
    pub author_platform: Option<&'a str>,
    pub author_model: Option<&'a str>,
}

impl Database {
    /// Replace the caller's note for one spec (upsert on the
    /// `(spec_id, owner_kind, owner_id)` key). Refuses bodies over
    /// [`SPEC_NOTE_MAX_CHARS`] with a named error before touching the table.
    pub fn spec_note_write(&self, note: NewSpecNote<'_>) -> Result<()> {
        let len = note.body.chars().count();
        if len > SPEC_NOTE_MAX_CHARS {
            return Err(anyhow!(
                "Note body is {len} characters; the limit is {SPEC_NOTE_MAX_CHARS} characters."
            ));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO spec_notes
                 (spec_id, owner_kind, owner_id, owner_name, body,
                  author_platform, author_model, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(spec_id, owner_kind, owner_id) DO UPDATE SET
                 owner_name = excluded.owner_name,
                 body = excluded.body,
                 author_platform = excluded.author_platform,
                 author_model = excluded.author_model,
                 updated_at = excluded.updated_at",
            params![
                note.spec_id,
                note.owner_kind,
                note.owner_id,
                note.owner_name,
                note.body,
                note.author_platform,
                note.author_model,
                now,
            ],
        )?;
        Ok(())
    }

    /// Every note of one spec, newest first; `owner_kind`/`owner_id` break
    /// ties so the order is deterministic.
    pub fn spec_notes_get(&self, spec_id: &str) -> Result<Vec<SpecNoteRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT owner_kind, owner_id, owner_name, body,
                    author_platform, author_model, updated_at
             FROM spec_notes WHERE spec_id = ?1
             ORDER BY updated_at DESC, owner_kind ASC, owner_id ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], read_spec_note_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// Delete every note of one spec. Returns the rows removed.
    pub fn clear_spec_notes(&self, spec_id: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let deleted = conn.execute(
            "DELETE FROM spec_notes WHERE spec_id = ?1",
            params![spec_id],
        )?;
        Ok(deleted)
    }

    /// Delete every note of every spec bound to one graph, plus every note of
    /// the specs in the graph's active run queue (a queue run's own bound
    /// specs are typically empty — its members are what actually ran). Used
    /// by `graph_reset` with `clear_notes: true` on a blanket reset.
    pub fn clear_spec_notes_for_graph(&self, graph_id: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let queue_ids: Vec<String> = conn
            .query_row(
                "SELECT active_run_queue_id FROM graphs WHERE id = ?1",
                params![graph_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .unwrap_or(None)
            .into_iter()
            .collect();
        drop(conn);
        let mut removed = self.clear_spec_notes_for_graph_bound(graph_id)?;
        for queue_id in queue_ids {
            let member_ids = self.list_queue_member_spec_ids(&queue_id)?;
            for spec_id in member_ids {
                removed += self.clear_spec_notes(&spec_id)?;
            }
        }
        Ok(removed)
    }

    fn clear_spec_notes_for_graph_bound(&self, graph_id: &str) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let deleted = conn.execute(
            "DELETE FROM spec_notes
             WHERE spec_id IN (SELECT id FROM graph_specs WHERE graph_id = ?1)",
            params![graph_id],
        )?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{GraphSpec, GraphSpecStatus};

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn seed_spec(db: &Database, spec_id: &str) {
        db.insert_graph_spec(&GraphSpec {
            id: spec_id.to_string(),
            graph_id: None,
            name: spec_id.to_string(),
            description: None,
            position: 0,
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
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .unwrap();
    }

    fn note<'a>(spec_id: &'a str, owner_id: &'a str, body: &'a str) -> NewSpecNote<'a> {
        NewSpecNote {
            spec_id,
            owner_kind: "node",
            owner_id,
            owner_name: owner_id,
            body,
            author_platform: Some("claude"),
            author_model: Some("opus"),
        }
    }

    #[test]
    fn insert_reads_back() {
        let db = test_db();
        seed_spec(&db, "spec-1");
        db.spec_note_write(note("spec-1", "impl", "did the thing"))
            .unwrap();

        let notes = db.spec_notes_get("spec-1").unwrap();
        assert_eq!(notes.len(), 1);
        let row = &notes[0];
        assert_eq!(row.owner_kind, "node");
        assert_eq!(row.owner_id, "impl");
        assert_eq!(row.owner_name, "impl");
        assert_eq!(row.body, "did the thing");
        assert_eq!(row.author_platform.as_deref(), Some("claude"));
        assert_eq!(row.author_model.as_deref(), Some("opus"));
        assert!(!row.updated_at.is_empty());
    }

    #[test]
    fn update_replaces_same_owner() {
        let db = test_db();
        seed_spec(&db, "spec-1");
        db.spec_note_write(note("spec-1", "impl", "first")).unwrap();
        db.spec_note_write(note("spec-1", "impl", "second"))
            .unwrap();

        let notes = db.spec_notes_get("spec-1").unwrap();
        assert_eq!(notes.len(), 1, "same owner must keep exactly one note");
        assert_eq!(notes[0].body, "second");
    }

    #[test]
    fn length_rejected_before_insert() {
        let db = test_db();
        seed_spec(&db, "spec-1");
        let body = "x".repeat(SPEC_NOTE_MAX_CHARS + 1);
        let err = db
            .spec_note_write(note("spec-1", "impl", &body))
            .unwrap_err();
        assert!(
            err.to_string().contains("2500"),
            "refusal must name the limit, got: {err}"
        );
        assert!(
            db.spec_notes_get("spec-1").unwrap().is_empty(),
            "rejected write must leave the table empty"
        );
    }

    #[test]
    fn get_orders_newest_first() {
        let db = test_db();
        seed_spec(&db, "spec-1");
        db.spec_note_write(note("spec-1", "older", "first written"))
            .unwrap();
        db.spec_note_write(note("spec-1", "newer", "second written"))
            .unwrap();

        let notes = db.spec_notes_get("spec-1").unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].owner_id, "newer");
        assert_eq!(notes[1].owner_id, "older");
    }

    #[test]
    fn clear_removes() {
        let db = test_db();
        seed_spec(&db, "spec-1");
        db.spec_note_write(note("spec-1", "a", "one")).unwrap();
        db.spec_note_write(note("spec-1", "b", "two")).unwrap();

        let removed = db.clear_spec_notes("spec-1").unwrap();
        assert_eq!(removed, 2);
        assert!(db.spec_notes_get("spec-1").unwrap().is_empty());
    }
}
