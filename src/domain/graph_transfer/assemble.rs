//! Assembling a validated import plan from an export document without
//! touching the database or installing anything — the structural half of
//! `graph_import`, shared with `graph_validate`. Lives in its own module
//! (rather than `import.rs`) to keep every production file within the
//! file-size gate; the node/edge/ensemble builders it orchestrates stay in
//! `import.rs`.

use super::*;

/// Assemble a validated [`GraphImportPlan`] without touching the database or
/// installing anything: the same structural checks `graph_import` runs, with
/// the prompt-preset existence check only when `preset_dir` is `Some`.
/// Import passes its prompts dir (a preset must exist on this machine);
/// `graph_validate` passes `None` and applies the sharing rule itself — a
/// referenced preset must be bundled in the document or a builtin — since
/// validation must not depend on what this machine happens to have
/// installed.
pub fn assemble_import_plan(
    document: &GraphExportDocument,
    graph_id: &str,
    preset_dir: Option<&std::path::Path>,
) -> Result<GraphImportPlan, String> {
    ensure_import_format_version(document)?;
    reject_duplicate_import_names(document)?;
    if let Some(dir) = preset_dir {
        reject_unknown_prompt_presets(document, dir)?;
    }

    let now = chrono::Utc::now();
    let (nodes, name_to_id) = build_import_nodes(document, graph_id, now)?;
    reject_unknown_output_references(document, &name_to_id)?;

    let allocations = allocated_ensemble_ids(document);
    let maps = ImportNameMaps::new(name_to_id, document, &allocations);
    let mut edges = build_import_edges(document, graph_id, &maps)?;

    // Ensemble-owned nodes continue the position sequence after every plain
    // node — the same "append after existing nodes" convention
    // `graph_add_ensemble`'s `start_position` uses.
    let mut next_position = nodes
        .iter()
        .map(|node| node.position)
        .max()
        .map(|max| max + 1)
        .unwrap_or(1);

    let mut ensembles = Vec::with_capacity(document.ensembles.len());
    for (doc_ensemble, allocated) in document.ensembles.iter().zip(&allocations.entries) {
        let (plan, plan_edges) = build_import_ensemble_plan(
            doc_ensemble,
            allocated,
            &maps,
            graph_id,
            now,
            &mut next_position,
        )?;
        edges.extend(plan_edges);
        ensembles.push(plan);
    }

    // A crew-bound ensemble chained into another ensemble whose members are
    // not materialised yet has no member nodes to fan its exit out to (its
    // exit edge is the target's entry edge). Record the source join as a
    // pending entry source of the target so the target's first sync builds the
    // fan-out — the same rows the extra entry sources use, so a chained exit
    // is wired through the one sync path rather than by a second walk.
    for (source_index, doc_ensemble) in document.ensembles.iter().enumerate() {
        let exits = [
            (
                doc_ensemble.on_pass_to.as_ensemble_name(),
                GraphEdgeCondition::Pass,
            ),
            (
                doc_ensemble
                    .on_fail_to
                    .as_ref()
                    .and_then(GraphExportEnsembleTarget::as_ensemble_name),
                GraphEdgeCondition::Fail,
            ),
        ];
        for (target_name, condition) in exits {
            let Some(target_name) = target_name else {
                continue;
            };
            let Some(target_index) = document
                .ensembles
                .iter()
                .position(|candidate| candidate.name == target_name)
            else {
                continue;
            };
            if !allocations.entries[target_index].member_ids.is_empty() {
                continue;
            }
            ensembles[target_index]
                .pending_entry_sources
                .push((allocations.entries[source_index].join_id.clone(), condition));
        }
    }

    // Defense in depth: the same structural check `graph_run` runs on every
    // ensemble reachable from a live graph (see `validate_ensembles_in_graph`)
    // confirms the plan's wiring is internally consistent before anything is
    // persisted — nothing above should ever be able to trip it, but a
    // hand-edited file is exactly the kind of input this guards against.
    let mut all_nodes = nodes.clone();
    let mut ensemble_details = Vec::with_capacity(ensembles.len());
    for plan in &ensembles {
        all_nodes.push(plan.join_node.clone());
        all_nodes.extend(plan.member_nodes.iter().cloned());
        ensemble_details.push(EnsembleDetails {
            ensemble: plan.ensemble.clone(),
            members: plan.members.clone(),
        });
    }
    validate_ensembles_in_graph(&ensemble_details, &all_nodes, &edges)?;

    // CM49: a crew-bound ensemble's member wiring is materialized later by the
    // sync path, so the pre-materialization graph has an orphan quorum and the
    // whole-graph entry-point check would reject it; skip it for a crew binding.
    let has_crew_binding = ensembles
        .iter()
        .any(|plan| plan.ensemble.crew_name.is_some());
    let terminals = if has_crew_binding {
        Vec::new()
    } else {
        collect_import_terminals(&all_nodes, &edges)?
    };

    let infra_node_id = document
        .infra_node
        .as_deref()
        .map(|name| maps.resolve_node_name(name))
        .transpose()?;

    Ok(GraphImportPlan {
        nodes,
        edges,
        ensembles,
        terminals,
        infra_node_id,
        hooks: document.hooks.clone(),
    })
}
