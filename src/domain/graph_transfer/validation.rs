use std::collections::BTreeSet;

use super::*;

/// A missing/unsupported `format_version` is refused with its own clear
/// message rather than falling through to a generic serde error (decision 6).
pub(super) fn ensure_import_format_version(document: &GraphExportDocument) -> Result<(), String> {
    if !matches!(document.format_version, 1..=6) {
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

/// Every `prompt_preset` an agent node names must be resolvable on this
/// machine: a `<prompts_dir>/<name>.md` file (what
/// `domain::prompts::resolve_prompt_preset` reads at spawn time), one of the
/// builtins seeded on daemon start, or a preset the document itself bundles
/// (v6). A document from an older version carries only the preset NAME, so a
/// document written elsewhere would otherwise import cleanly and then run that
/// node on an empty prompt — a hard refusal at import time is the safety net
/// (this spec). Names are compared verbatim — the spawn path joins them
/// verbatim too; only a whitespace-only name is skipped, exactly as the spawn
/// path skips it (it then takes the default template).
///
/// Only plain agent nodes are inspected: an ensemble member carries no preset
/// field — its prompt is the ensemble's own template — so there is nothing to
/// check there.
///
/// The document's bundled `prompt_presets` (v6) count as known references:
/// they are installed into the prompts dir by import before any node spawns.
/// Each bundled entry is validated first (safe name, no overwrite of a
/// differing local file), before the empty-reference early return — a
/// document may bundle zero presets, but every preset it does bundle must be
/// installable.
pub(super) fn reject_unknown_prompt_presets(
    document: &GraphExportDocument,
    prompts_dir: &std::path::Path,
) -> Result<(), String> {
    validate(&document.prompt_presets, prompts_dir)?;
    let bundled: BTreeSet<&str> = document
        .prompt_presets
        .iter()
        .map(|preset| preset.name.as_str())
        .collect();
    let mut referenced: BTreeSet<&str> = document
        .nodes
        .iter()
        .filter(|node| node.kind == GraphNodeKind::Agent)
        .filter_map(|node| node.config.get("prompt_preset").and_then(Value::as_str))
        .filter(|name| !name.trim().is_empty())
        .collect();
    if referenced.is_empty() {
        return Ok(());
    }
    let builtin_names: Vec<&'static str> = crate::domain::prompts::builtin_prompt_preset_specs()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let builtins: BTreeSet<&str> = builtin_names.iter().copied().collect();
    referenced.retain(|name| {
        !builtins.contains(name)
            && !bundled.contains(name)
            && !prompts_dir.join(format!("{name}.md")).is_file()
    });
    if referenced.is_empty() {
        return Ok(());
    }
    // Named from the same list the check uses, in declaration order, so adding
    // a builtin can never leave the hint behind.
    Err(format!(
        "Unknown prompt preset(s): {}. Add file(s) under {} or use a builtin ({}).",
        referenced.into_iter().collect::<Vec<_>>().join(", "),
        prompts_dir.display(),
        builtin_names.join(", ")
    ))
}

/// Authoring-time shape rules `graph_add_ensemble` enforces: member-count
/// bounds, `min_pass` range, and non-negative timeout/grace fields (CM24).
pub(super) fn validate_import_ensemble_shape(
    doc_ensemble: &GraphExportEnsemble,
    import_kind: EnsembleKind,
    caps: crate::domain::graphs::MemberCaps,
) -> Result<(), String> {
    // CM49: a crew-bound ensemble carries the crew NAME and no literal
    // members — its size is the crew's, enforced at bind/dispatch, so the
    // member-count check is skipped. A doc that sets both is a refusal.
    let crew = doc_ensemble
        .crew
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if crew.is_some() && !doc_ensemble.members.is_empty() {
        return Err(format!(
            "Ensemble '{}' sets both 'crew' and 'members'; a bound ensemble carries the crew name only.",
            doc_ensemble.name
        ));
    }
    if crew.is_some() {
        if doc_ensemble.min_pass < 1 {
            return Err(format!(
                "Ensemble '{}' has an invalid min_pass ({}).",
                doc_ensemble.name, doc_ensemble.min_pass
            ));
        }
    } else {
        let (min_members, max_members) = member_bounds(import_kind, caps);
        let count = doc_ensemble.members.len();
        if count < min_members || count > max_members {
            // Only an over-cap import names the setting that raises the cap.
            let hint = if count > max_members {
                format!(
                    " Raise {} to allow more.",
                    crate::domain::graphs::member_cap_key(import_kind)
                )
            } else {
                String::new()
            };
            return Err(format!(
                "Ensemble '{}' (kind {}) must have {min_members}-{max_members} members, got {count}.{hint}",
                doc_ensemble.name,
                import_kind.as_str(),
            ));
        }
        validate_ensemble_min_pass(
            &doc_ensemble.name,
            doc_ensemble.min_pass,
            doc_ensemble.members.len(),
        )?;
    }
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
    crate::domain::validation::validate_mutex_list(
        &format!("Ensemble '{}'", doc_ensemble.name),
        Some(doc_ensemble.mutex.as_slice()),
    )
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
/// node's config carries a non-empty `platform`, `cli`, or `crew` (CM49).
pub(super) fn node_config_has_harness(config: &Value) -> bool {
    let has_non_empty_str = |field: &str| {
        config
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    has_non_empty_str("platform") || has_non_empty_str("cli") || has_non_empty_str("crew")
}
