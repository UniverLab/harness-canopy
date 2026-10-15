//! CM46: durable per-graph event log — the schema-bound reads and writes.
//!
//! The write *path* (the single `record` that keeps this table and the
//! activity stream in step) lives in `domain::activity`; this module holds
//! only the SQL: insert one event, list a graph's events oldest-first with
//! paging and filters, and prune by age for graphs that are not running.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use crate::db::Database;
use crate::domain::activity::GraphEventRecord;

/// Hard cap on one [`Database::list_graph_events`] page, so a caller can never
/// pull an unbounded history in a single read (CB28 result budget).
const MAX_LIST_LIMIT: usize = 500;

/// One persisted `graph_events` row. `payload` is left as the raw JSON string
/// so the reader only pays to parse it when asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEventRow {
    pub id: i64,
    pub graph_id: String,
    pub spec_id: Option<String>,
    pub node_id: Option<String>,
    pub run_id: Option<String>,
    pub kind: String,
    pub message: String,
    pub payload: Option<String>,
    pub created_at: String,
}

fn read_graph_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEventRow> {
    Ok(GraphEventRow {
        id: row.get(0)?,
        graph_id: row.get(1)?,
        spec_id: row.get(2)?,
        node_id: row.get(3)?,
        run_id: row.get(4)?,
        kind: row.get(5)?,
        message: row.get(6)?,
        payload: row.get(7)?,
        created_at: row.get(8)?,
    })
}

/// Build the shared `WHERE created_at < ? AND graph_id NOT IN (...)` fragment
/// and its bound params. The `NOT IN` clause is the "for graphs that are not
/// running" requirement; it is skipped when no graph is running because
/// SQLite rejects an empty `IN ()`.
fn age_prune_predicate(
    cutoff: &str,
    running_graph_ids: &[String],
) -> (String, Vec<Box<dyn rusqlite::ToSql>>) {
    let mut bound: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(cutoff.to_string())];
    let mut where_sql = String::from(" WHERE created_at < ?1");
    if !running_graph_ids.is_empty() {
        let mut placeholders = Vec::with_capacity(running_graph_ids.len());
        for id in running_graph_ids {
            bound.push(Box::new(id.clone()));
            placeholders.push(format!("?{}", bound.len()));
        }
        where_sql.push_str(&format!(
            " AND graph_id NOT IN ({})",
            placeholders.join(", ")
        ));
    }
    (where_sql, bound)
}

impl Database {
    /// Insert one durable event and return its row id. The caller
    /// ([`crate::domain::activity::record`]) treats a failure as a WARN.
    pub fn insert_graph_event(&self, event: &GraphEventRecord) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO graph_events
                 (graph_id, spec_id, node_id, run_id, kind, message, payload, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                event.graph_id,
                event.spec_id,
                event.node_id,
                event.run_id,
                event.kind,
                event.message,
                event.payload,
                chrono::Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// One graph's events, oldest first (`id ASC`). Optional `since_id`,
    /// `spec_id` and `kinds` filters compose; `limit` is clamped to
    /// `1..=MAX_LIST_LIMIT`.
    pub fn list_graph_events(
        &self,
        graph_id: &str,
        since_id: Option<i64>,
        spec_id: Option<&str>,
        kinds: Option<&[String]>,
        limit: usize,
    ) -> Result<Vec<GraphEventRow>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let capped = limit.clamp(1, MAX_LIST_LIMIT);
        let mut sql = String::from(
            "SELECT id, graph_id, spec_id, node_id, run_id, kind, message, payload, created_at
             FROM graph_events WHERE graph_id = ?1",
        );
        let mut bound: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(graph_id.to_string())];
        if let Some(spec_id) = spec_id {
            bound.push(Box::new(spec_id.to_string()));
            sql.push_str(&format!(" AND spec_id = ?{}", bound.len()));
        }
        if let Some(since_id) = since_id {
            bound.push(Box::new(since_id));
            sql.push_str(&format!(" AND id > ?{}", bound.len()));
        }
        if let Some(kinds) = kinds.filter(|k| !k.is_empty()) {
            let mut placeholders = Vec::with_capacity(kinds.len());
            for kind in kinds {
                bound.push(Box::new(kind.clone()));
                placeholders.push(format!("?{}", bound.len()));
            }
            sql.push_str(&format!(" AND kind IN ({})", placeholders.join(", ")));
        }
        bound.push(Box::new(capped as i64));
        sql.push_str(&format!(" ORDER BY id ASC LIMIT ?{}", bound.len()));

        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(Box::as_ref).collect();
        let rows = stmt.query_map(refs.as_slice(), read_graph_event_row)?;
        Ok(rows.filter_map(|row| row.ok()).collect())
    }

    /// The smallest event id at or after `ts` (RFC3339), for translating a
    /// `--since <timestamp>` into the numeric `since_id` the list query uses.
    pub fn first_graph_event_id_at_or_after(
        &self,
        graph_id: &str,
        ts: &str,
    ) -> Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        conn.query_row(
            "SELECT id FROM graph_events
             WHERE graph_id = ?1 AND created_at >= ?2
             ORDER BY id ASC LIMIT 1",
            params![graph_id, ts],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
    }

    /// How many events [`Self::prune_graph_events`] would delete — the
    /// dry-run projection for `canopy clean`.
    pub fn count_pruneable_graph_events(
        &self,
        cutoff: &str,
        running_graph_ids: &[String],
    ) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let (where_sql, bound) = age_prune_predicate(cutoff, running_graph_ids);
        let sql = format!("SELECT COUNT(*) FROM graph_events{where_sql}");
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(Box::as_ref).collect();
        let count: i64 = conn.query_row(&sql, refs.as_slice(), |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Delete events older than `cutoff`, except those belonging to a running
    /// graph. Returns the number of rows removed.
    pub fn prune_graph_events(&self, cutoff: &str, running_graph_ids: &[String]) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let (where_sql, bound) = age_prune_predicate(cutoff, running_graph_ids);
        let sql = format!("DELETE FROM graph_events{where_sql}");
        let refs: Vec<&dyn rusqlite::ToSql> = bound.iter().map(Box::as_ref).collect();
        let deleted = conn.execute(&sql, refs.as_slice())?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::activity::GraphEventRecord;

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn record(graph_id: &str, kind: &str, message: &str) -> GraphEventRecord {
        GraphEventRecord {
            graph_id: graph_id.to_string(),
            spec_id: None,
            node_id: None,
            run_id: None,
            kind: kind.to_string(),
            message: message.to_string(),
            payload: None,
        }
    }

    #[test]
    fn insert_then_list_returns_oldest_first() {
        let db = test_db();
        let first = db
            .insert_graph_event(&record("g1", "graph_started", "one"))
            .unwrap();
        let second = db
            .insert_graph_event(&record("g1", "spec_started", "two"))
            .unwrap();
        let rows = db.list_graph_events("g1", None, None, None, 200).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, first);
        assert_eq!(rows[1].id, second);
        assert_eq!(rows[0].kind, "graph_started");
    }

    #[test]
    fn list_filters_by_since_spec_and_kind() {
        let db = test_db();
        let a = db
            .insert_graph_event(&record("g1", "graph_started", "one"))
            .unwrap();
        let mut b = record("g1", "spec_started", "two");
        b.spec_id = Some("spec-1".to_string());
        let b = db.insert_graph_event(&b).unwrap();
        // Another graph's row must never leak in.
        db.insert_graph_event(&record("g2", "graph_started", "other"))
            .unwrap();

        let paged = db
            .list_graph_events("g1", Some(a), None, None, 200)
            .unwrap();
        assert_eq!(paged.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b]);

        let by_spec = db
            .list_graph_events("g1", None, Some("spec-1"), None, 200)
            .unwrap();
        assert_eq!(by_spec.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b]);

        let kinds = vec!["spec_started".to_string()];
        let by_kind = db
            .list_graph_events("g1", None, None, Some(&kinds), 200)
            .unwrap();
        assert_eq!(by_kind.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b]);
    }

    #[test]
    fn check_constraint_rejects_unknown_kind() {
        let db = test_db();
        assert!(
            db.insert_graph_event(&record("g1", "not_a_kind", "boom"))
                .is_err(),
            "the CHECK constraint must reject a kind outside the vocabulary"
        );
    }

    #[test]
    fn paging_with_since_id_returns_no_duplicates_and_no_gaps() {
        let db = test_db();
        let ids: Vec<i64> = (1..=5)
            .map(|i| {
                db.insert_graph_event(&record("g1", "graph_started", &format!("e{i}")))
                    .unwrap()
            })
            .collect();

        let page1 = db.list_graph_events("g1", None, None, None, 2).unwrap();
        assert_eq!(
            page1.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[0], ids[1]]
        );
        let page2 = db
            .list_graph_events("g1", Some(ids[1]), None, None, 2)
            .unwrap();
        assert_eq!(
            page2.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![ids[2], ids[3]]
        );
        let page3 = db
            .list_graph_events("g1", Some(ids[3]), None, None, 2)
            .unwrap();
        assert_eq!(page3.iter().map(|r| r.id).collect::<Vec<_>>(), vec![ids[4]]);

        let all: Vec<i64> = page1
            .iter()
            .chain(page2.iter())
            .chain(page3.iter())
            .map(|r| r.id)
            .collect();
        assert_eq!(all, ids, "paging must be contiguous with no repeats");
    }

    #[test]
    fn prune_by_age_keeps_running_graph_and_recent_rows() {
        let db = test_db();
        let old = db
            .insert_graph_event(&record("old-graph", "graph_started", "old"))
            .unwrap();
        let recent = db
            .insert_graph_event(&record("new-graph", "graph_started", "new"))
            .unwrap();
        let running = db
            .insert_graph_event(&record("live-graph", "graph_started", "live"))
            .unwrap();
        {
            let conn = db.conn.lock().unwrap();
            conn.execute(
                "UPDATE graph_events SET created_at = '2000-01-01T00:00:00+00:00' WHERE id IN (?1, ?2)",
                params![old, running],
            )
            .unwrap();
        }
        let cutoff = "2001-01-01T00:00:00+00:00".to_string();
        let running_ids = vec!["live-graph".to_string()];

        assert_eq!(
            db.count_pruneable_graph_events(&cutoff, &running_ids)
                .unwrap(),
            1,
            "only the old, non-running event is a deletion candidate"
        );
        let deleted = db.prune_graph_events(&cutoff, &running_ids).unwrap();
        assert_eq!(deleted, 1);

        let remaining: Vec<i64> = db
            .list_graph_events("old-graph", None, None, None, 200)
            .unwrap()
            .iter()
            .map(|r| r.id)
            .collect();
        assert!(remaining.is_empty(), "the 91-day-old event must be gone");
        assert_eq!(
            db.list_graph_events("new-graph", None, None, None, 200)
                .unwrap()[0]
                .id,
            recent
        );
        assert_eq!(
            db.list_graph_events("live-graph", None, None, None, 200)
                .unwrap()[0]
                .id,
            running,
            "a running graph's old events must survive"
        );
    }
}
