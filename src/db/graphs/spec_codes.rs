//! Spec-type code helpers (SPECCODE) lifted out of `specs.rs` so that
//! module stays under the repo's 800-code-line budget. These methods live on
//! [`Database`] just as the originals did — only their textual home moved.
use super::*;

impl Database {
    /// SPECCODE: consume the next number for `(workdir, type)` and return the
    /// full code (`FIX-7`). Backed by the `spec_code_counters` table — seeded
    /// at 2 when first consumed for a pair, bumped on every later consume, and
    /// never a `MAX(code)+1` scan, so a deleted spec's number is never reused
    /// and two creates can never grab the same one (the whole sequence runs
    /// under the single connection lock).
    pub fn next_spec_code(&self, workdir: &str, spec_type: SpecType) -> Result<String> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let consumed: i64 = match conn
            .query_row(
                "SELECT next FROM spec_code_counters WHERE workdir = ?1 AND spec_type = ?2",
                params![workdir, spec_type.as_str()],
                |row| row.get(0),
            )
            .optional()?
        {
            Some(next) => {
                conn.execute(
                    "UPDATE spec_code_counters SET next = next + 1 WHERE workdir = ?1 AND spec_type = ?2",
                    params![workdir, spec_type.as_str()],
                )?;
                next
            }
            None => {
                conn.execute(
                    "INSERT INTO spec_code_counters (workdir, spec_type, next) VALUES (?1, ?2, 2)",
                    params![workdir, spec_type.as_str()],
                )?;
                1
            }
        };
        Ok(format!("{}-{consumed}", spec_type.code_prefix()))
    }

    /// SPECCODE: the counter key a spec's code was (or will be) numbered
    /// against: the bound graph's `workdir`, else the spec's own `workdir`
    /// tag, else `""`. Persisted per workdir and type in `spec_code_counters`;
    /// shared by the create path (handler resolves it before insert) and the
    /// `spec_update` path that assigns a type to a legacy spec.
    pub fn spec_code_counter_key(&self, spec: &GraphSpec) -> Result<String> {
        if let Some(graph_id) = &spec.graph_id {
            if let Some(graph) = self.get_graph(graph_id)? {
                return Ok(graph.workdir);
            }
        }
        Ok(spec.workdir.clone().unwrap_or_default())
    }

    /// SPECCODE: write a spec's type and scope. `code` is the already-consumed
    /// code for a legacy spec gaining its type for the first time (or `None`/
    /// the existing code when the type is only changing — a code is immutable
    /// and never renumbered by the Designer decision). Callers consume the
    /// counter themselves via [`Self::next_spec_code`] so this write never
    /// assumes it must number.
    pub fn set_graph_spec_type(
        &self,
        spec_id: &str,
        spec_type: SpecType,
        spec_scope: Option<&str>,
        code: Option<&str>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs
             SET spec_type = ?1,
                 spec_scope = ?2,
                 spec_code = COALESCE(?3, spec_code),
                 updated_at = ?5
             WHERE id = ?4",
            params![
                spec_type.as_str(),
                spec_scope,
                code,
                spec_id,
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(rows > 0)
    }
}
