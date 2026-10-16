//! Entry sources an ensemble declares beyond its primary.
//!
//! Two import paths share this module. A literal-member ensemble wires each
//! extra source as one edge per member, exactly the shape
//! `Database::add_ensemble_entry_source` writes at authoring time. A
//! crew-bound ensemble travels without member nodes — its members are
//! materialised from the crew by the one sync path
//! (`Database::sync_ensemble_with_crew` → `replace_ensemble_members`) — so its
//! sources are recorded as *pending* rows (`ensemble_pending_entry_sources`)
//! that the sync path consumes instead, along with any chained exit that lands
//! on it. Either way the resolution and duplicate checks live here, so the
//! two paths can never drift.

use super::*;

/// Every extra entry source an ensemble declares, resolved to node ids and
/// deduped against the primary and each other. A source duplicating the
/// primary (same node AND condition) or another extra is a refusal, never
/// silently doubled.
fn extra_entry_sources(
    doc_ensemble: &GraphExportEnsemble,
    entry_from_node: &str,
    maps: &ImportNameMaps<'_>,
) -> Result<Vec<(String, GraphEdgeCondition)>, String> {
    let mut seen: std::collections::HashSet<(String, GraphEdgeCondition)> =
        std::collections::HashSet::new();
    seen.insert((
        entry_from_node.to_string(),
        doc_ensemble.entry_condition.clone(),
    ));
    let mut sources = Vec::new();
    for source in &doc_ensemble.extra_entry_sources {
        let from_id = maps
            .resolve_node_name(&source.from_node)
            .map_err(|e| format!("Ensemble '{}' extra entry source {e}.", doc_ensemble.name))?;
        if !seen.insert((from_id.clone(), source.condition.clone())) {
            return Err(format!(
                "Ensemble '{}' lists entry source '{}' with condition '{}' more than once — the primary entry already uses it; remove the duplicate from extra_entry_sources.",
                doc_ensemble.name,
                source.from_node,
                source.condition.as_str(),
            ));
        }
        sources.push((from_id, source.condition.clone()));
    }
    Ok(sources)
}

/// CB71: restore every extra entry source as one edge per member, exactly the
/// shape `Database::add_ensemble_entry_source` writes at authoring time.
pub(super) fn import_extra_entry_source_edges(
    doc_ensemble: &GraphExportEnsemble,
    entry_from_node: &str,
    maps: &ImportNameMaps<'_>,
    member_node_ids: &[String],
    graph_id: &str,
) -> Result<Vec<GraphEdge>, String> {
    let mut edges = Vec::new();
    for (from_id, condition) in extra_entry_sources(doc_ensemble, entry_from_node, maps)? {
        for member_node_id in member_node_ids {
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: None,
                graph_id: Some(graph_id.to_string()),
                from_node: from_id.clone(),
                to_node: member_node_id.clone(),
                condition: condition.clone(),
            });
        }
    }
    Ok(edges)
}

/// A crew-bound ensemble's extra entry sources, resolved the same way but
/// recorded as pending rows rather than edges: the members (and so the
/// fan-out) do not exist until the crew is materialised.
pub(super) fn crew_bound_pending_entry_sources(
    doc_ensemble: &GraphExportEnsemble,
    entry_from_node: &str,
    maps: &ImportNameMaps<'_>,
) -> Result<Vec<(String, GraphEdgeCondition)>, String> {
    extra_entry_sources(doc_ensemble, entry_from_node, maps)
}
