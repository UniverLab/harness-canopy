use crate::domain::graphs::{EnsembleDetails, GraphNode};

/// `read_only` and `commit_rights` are mutually exclusive once resolved: on
/// the ensemble itself, on a member's own config, and across the two (a
/// `read_only` ensemble owning a committer member, or a `commit_rights`
/// ensemble owning a `read_only` member). Resolution through the ensemble
/// mirrors the engine's `node_has_read_only` / `node_has_commit_rights`.
pub(super) fn ensure_ensemble_rights_not_conflicting(
    details: &EnsembleDetails,
    nodes: &[GraphNode],
) -> Result<(), String> {
    let ensemble = &details.ensemble;
    let label = format!("Ensemble '{}' ('{}')", ensemble.id, ensemble.name);
    if ensemble.read_only && ensemble.commit_rights {
        return Err(format!(
            "{label} cannot set both 'read_only' and 'commit_rights' to true."
        ));
    }
    for member in &details.members {
        let Some(node) = nodes.iter().find(|n| n.id == member.node_id) else {
            continue;
        };
        let flag =
            |key: &str| node.config.get(key).and_then(serde_json::Value::as_bool) == Some(true);
        if (flag("read_only") || ensemble.read_only)
            && (flag("commit_rights") || ensemble.commit_rights)
        {
            return Err(format!(
                "{label} member node '{}' cannot resolve both 'read_only' and 'commit_rights' to true.",
                node.name
            ));
        }
    }
    Ok(())
}
