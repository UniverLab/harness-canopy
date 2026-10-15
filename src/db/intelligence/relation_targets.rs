use rusqlite::OptionalExtension;

use super::*;

impl Database {
    /// Resolve a node-id prefix (exact match, else unique `LIKE` prefix) against an
    /// already-held connection — **no locking**. Lets callers inside a transaction
    /// (`&Transaction` derefs to `&Connection`) run the existence check that backs a
    /// relation target inside the upsert's own transaction instead of before it,
    /// removing both the reentrant-mutex deadlock and the pre-lock TOCTOU window.
    pub(super) fn resolve_node_id_by_prefix_on(
        conn: &rusqlite::Connection,
        prefix: &str,
    ) -> Result<Option<String>> {
        if prefix.is_empty() {
            return Ok(None);
        }
        // Exact match fast path — full ids (incl. UUIDs) land here and skip the
        // LIKE scan, so an exact id that shares a prefix with others is still
        // returned unambiguously instead of raising the ambiguous error.
        let exact: Option<String> = conn
            .query_row(
                "SELECT id FROM intelligence_nodes WHERE id = ?1 LIMIT 1",
                rusqlite::params![prefix],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = exact {
            return Ok(Some(id));
        }
        // Escape LIKE metacharacters so '_', '%' and the escape char match
        // literally — otherwise a stray '_' would silently over-match and turn a
        // single valid target into a spurious "ambiguous" error.
        let escaped_prefix = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let mut stmt =
            conn.prepare("SELECT id FROM intelligence_nodes WHERE id LIKE ?1 || '%' ESCAPE '\\'")?;
        let ids: Vec<String> = stmt
            .query_map(rusqlite::params![escaped_prefix], |row| row.get(0))?
            .filter_map(|r| r.ok())
            .collect();
        match ids.len() {
            0 => Ok(None),
            1 => Ok(Some(ids.into_iter().next().unwrap())),
            _ => Err(anyhow!(
                "Ambiguous node id prefix '{}' matches {} nodes: {}",
                prefix,
                ids.len(),
                ids.join(", ")
            )),
        }
    }

    /// Resolve every `relations[].to_node_id` to its full stored id.
    ///
    /// Exact ids pass through (fast path inside
    /// [`Self::resolve_node_id_by_prefix_on`]); unique prefixes become the full
    /// id. `Ok(None)` (no match) becomes
    /// `Err("relation target '<prefix>' matches no intelligence node")`;
    /// an ambiguous prefix propagates the existing `Ambiguous node id
    /// prefix …` error unchanged. Takes `&[IntelligenceRelationInput]` and
    /// returns owned resolved clones so the caller can run fully inside its
    /// write transaction: it borrows the connection it is given and never
    /// locks `self.conn`, so passing `&tx` reuses the transaction instead of
    /// deadlocking on the non-reentrant mutex.
    pub(super) fn resolve_relation_target_ids(
        conn: &rusqlite::Connection,
        relations: &[IntelligenceRelationInput],
    ) -> Result<Vec<IntelligenceRelationInput>> {
        let mut out = Vec::with_capacity(relations.len());
        for rel in relations {
            let Some(full) = Self::resolve_node_id_by_prefix_on(conn, &rel.to_node_id)? else {
                anyhow::bail!(
                    "relation target '{}' matches no intelligence node",
                    rel.to_node_id
                )
            };
            out.push(IntelligenceRelationInput {
                to_node_id: full,
                relation: rel.relation.clone(),
                weight: rel.weight,
            });
        }
        Ok(out)
    }
}
