use super::*;

/// A missing/unsupported `format_version` is refused with its own clear
/// message rather than falling through to a generic serde error (decision 6).
pub(super) fn ensure_import_format_version(document: &GraphExportDocument) -> Result<(), String> {
    if !matches!(document.format_version, 1..=4) {
        return Err(format!(
            "Graph export document has format_version {}, but this build only supports 1 through {}.",
            document.format_version, GRAPH_EXPORT_FORMAT_VERSION
        ));
    }
    Ok(())
}

/// Every `{{output:<name>}}` reference in a node's prompt template must name
/// a node this document defines.
pub(super) fn reject_unknown_output_references(
    document: &GraphExportDocument,
    name_to_id: &HashMap<&str, String>,
) -> Result<(), String> {
    for doc_node in &document.nodes {
        let Some(prompt) = doc_node
            .config
            .get("prompt_template")
            .and_then(|v| v.as_str())
        else {
            continue;
        };
        let mut rest = prompt;
        while let Some(start) = rest.find("{{output:") {
            let after_prefix = &rest[start + 9..];
            let Some(end) = after_prefix.find("}}") else {
                break;
            };
            let referenced_name = &after_prefix[..end];
            if !name_to_id.contains_key(referenced_name) {
                return Err(format!(
                    "Node '{}' references unknown node '{}' in {{{{output:{}}}}}.",
                    doc_node.name, referenced_name, referenced_name
                ));
            }
            rest = &after_prefix[end + 2..];
        }
    }
    Ok(())
}

/// Authoring-time shape rules `graph_add_ensemble` enforces: member-count
/// bounds, `min_pass` range, and non-negative timeout/grace fields (CM24).
pub(super) fn validate_import_ensemble_shape(
    doc_ensemble: &GraphExportEnsemble,
    import_kind: EnsembleKind,
) -> Result<(), String> {
    let (min_members, max_members) = member_bounds(import_kind);
    if doc_ensemble.members.len() < min_members || doc_ensemble.members.len() > max_members {
        return Err(format!(
            "Ensemble '{}' must have {min_members}-{max_members} members, got {}.",
            doc_ensemble.name,
            doc_ensemble.members.len()
        ));
    }
    validate_ensemble_min_pass(
        &doc_ensemble.name,
        doc_ensemble.min_pass,
        doc_ensemble.members.len(),
    )?;
    if doc_ensemble.timeout_minutes < 0 {
        return Err(format!(
            "Ensemble '{}' has a negative timeout_minutes.",
            doc_ensemble.name
        ));
    }
    if let Some(straggler) = doc_ensemble.straggler_timeout_minutes {
        if straggler < 0 {
            return Err(format!(
                "Ensemble '{}' has a negative straggler_timeout_minutes.",
                doc_ensemble.name
            ));
        }
    }
    // CM24: refuse negative grace on import (same wording pattern as
    // straggler); refused for any kind on write, engine only honours it
    // for parallel ensembles.
    if let Some(grace) = doc_ensemble.quorum_grace_minutes {
        if grace < 0 {
            return Err(format!(
                "Ensemble '{}' has a negative quorum_grace_minutes.",
                doc_ensemble.name
            ));
        }
    }
    Ok(())
}

/// Names of every agent node (plain or ensemble member) left without a
/// `platform`/`cli` after import — a v1 document (or a hand-written one)
/// may legitimately arrive without one, so `graph_import`'s response calls
/// out exactly which nodes need one filled in before the graph can run
/// (requirement 4).
pub fn agent_nodes_missing_platform(plan: &GraphImportPlan) -> Vec<String> {
    let plain = plan
        .nodes
        .iter()
        .filter(|node| node.kind == GraphNodeKind::Agent)
        .filter(|node| !node_config_has_harness(&node.config));
    let members = plan
        .ensembles
        .iter()
        .flat_map(|plan| plan.member_nodes.iter())
        .filter(|node| !node_config_has_harness(&node.config));
    plain.chain(members).map(|node| node.name.clone()).collect()
}

/// Mirrors `daemon::handler::config_has_agent_harness` by design: an agent
/// node's config carries a non-empty `platform` or `cli`.
pub(super) fn node_config_has_harness(config: &Value) -> bool {
    let has_non_empty_str = |field: &str| {
        config
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    has_non_empty_str("platform") || has_non_empty_str("cli")
}
