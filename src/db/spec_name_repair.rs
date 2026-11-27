//! One-time repair of stored `graph_specs.name` rows that still carry a
//! conventional header prefix (created before `check_subject` refused them).

use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};

use super::Database;
use crate::domain::spec_type::{has_header_prefix, strip_header};

impl Database {
    /// Strip stacked conventional headers from typed `graph_specs.name` rows
    /// and log the number of repaired rows. Idempotent: clean, untyped, or
    /// empty-after-strip rows are left untouched.
    pub(in crate::db) fn migrate_strip_repeated_spec_prefix(conn: &Connection) -> Result<()> {
        let count = Self::repair_repeated_spec_prefixes(conn)?;
        tracing::info!("repaired {count} spec name(s) that repeated a conventional prefix");
        Ok(())
    }

    pub(in crate::db) fn repair_repeated_spec_prefixes(conn: &Connection) -> Result<usize> {
        let mut stmt =
            conn.prepare("SELECT id, name FROM graph_specs WHERE spec_type IS NOT NULL")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut repaired = 0usize;
        let now = Utc::now().timestamp_millis();
        for (id, name) in rows {
            if !has_header_prefix(&name) {
                continue;
            }
            let cleaned = strip_header(&name);
            if cleaned.is_empty() {
                continue;
            }
            conn.execute(
                "UPDATE graph_specs SET name = ?1, updated_at = ?2 WHERE id = ?3",
                params![cleaned, now, id],
            )?;
            repaired += 1;
        }
        Ok(repaired)
    }
}
