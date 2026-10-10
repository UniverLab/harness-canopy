use super::*;

impl Database {
    /// Resolve every `relations[].to_node_id` to its full stored id.
    ///
    /// Exact ids pass through (fast path inside `resolve_node_id_by_prefix`);
    /// unique prefixes become the full id. `Ok(None)` (no match) becomes
    /// `Err("relation target '<prefix>' matches no intelligence node")`;
    /// an ambiguous prefix propagates the existing `Ambiguous node id
    /// prefix …` error unchanged. Takes `&[IntelligenceRelationInput]` and
    /// returns owned resolved clones so the caller can run fully inside its
    /// write transaction without re-locking (this method locks briefly per
    /// target and MUST be called before the transaction opens).
    pub(super) fn resolve_relation_target_ids(
        &self,
        relations: &[IntelligenceRelationInput],
    ) -> Result<Vec<IntelligenceRelationInput>> {
        let mut out = Vec::with_capacity(relations.len());
        for rel in relations {
            let Some(full) = self.resolve_node_id_by_prefix(&rel.to_node_id)? else {
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
