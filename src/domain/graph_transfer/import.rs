use super::*;

/// One ensemble unit built by [`build_import_plan`] — the join node, every
/// member node, the `ensembles`/`ensemble_members` rows — mirroring the
/// shape `daemon::handler::build_ensemble_unit` assembles for
/// `graph_add_ensemble`, so the two authoring paths can never drift.
#[derive(Debug, Clone)]
pub struct GraphImportEnsemblePlan {
    pub ensemble: Ensemble,
    pub members: Vec<EnsembleMember>,
    pub member_nodes: Vec<GraphNode>,
    pub join_node: GraphNode,
    /// Entry sources to wire once this ensemble's members materialise — its
    /// `extra_entry_sources` and every chained exit that lands on it, absent
    /// as edges because the members (and therefore the fan-out) do not exist
    /// yet. Persisted as `ensemble_pending_entry_sources` rows and consumed by
    /// the sync path (`replace_ensemble_members`). Empty for a literal-member
    /// ensemble, whose entry wiring is already real edges.
    pub pending_entry_sources: Vec<(String, GraphEdgeCondition)>,
}

/// Every fresh-id graph piece [`build_import_plan`] assembles for one
/// `graph_import` call — ready to persist as-is (decision 5: import is
/// all-or-nothing, so the caller persists every field here in one
/// transaction or none at all).
#[derive(Debug, Clone)]
pub struct GraphImportPlan {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub ensembles: Vec<GraphImportEnsemblePlan>,
    /// Terminal exits reported by structural validation, formatted for
    /// operators with the node name and id.
    pub terminals: Vec<String>,
    /// CM2: resolved infra node id (from the document's `infra_node` name),
    /// or `None` when the document has no infra node.
    pub infra_node_id: Option<String>,
    /// CM49: the document's completion hooks, restored verbatim on import.
    pub hooks: std::collections::BTreeMap<
        crate::domain::graphs::GraphHookEvent,
        Vec<crate::domain::graphs::GraphCompletionHook>,
    >,
}

/// Build a validated [`GraphImportPlan`] for `graph_id` from a parsed
/// [`GraphExportDocument`] — every node/edge/ensemble gets a fresh id, names
/// resolve to those ids, and every rule `graph_add_node`/`graph_add_edge`/
/// `graph_add_ensemble` would enforce at authoring time is enforced here too
/// (decision 5), with one deliberate carve-out: an agent node's missing
/// `platform`/`cli` is *not* rejected — decision 3 means a shared design may
/// legitimately arrive without one, and the caller (`graph_import`) reports
/// exactly which nodes still need it rather than refusing the whole import.
///
/// Returns `Err` (with nothing to persist) on: an unsupported/missing
/// `format_version`, a duplicate node name, an edge or ensemble field naming
/// a node absent from `document.nodes`, an ensemble that violates its
/// kind's member-count bounds, or `min_pass`/timeout fields out of range.
pub fn build_import_plan(
    document: &GraphExportDocument,
    graph_id: &str,
) -> Result<GraphImportPlan, String> {
    ensure_import_format_version(document)?;
    reject_duplicate_import_names(document)?;

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

    // CM49: a crew-bound ensemble's member wiring (entry fan-out, member→join
    // fan-in) is materialized later by the sync path, so the pre-materialization
    // graph has an orphan quorum and the whole-graph entry-point check would
    // reject it. Skip that check when a crew binding is present; the per-ensemble
    // wiring-ends check above still applies.
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

/// Name-resolution bundle replacing the four local closures the import plan
/// body used to build: document node name → fresh id, ensemble name →
/// allocated join id, ensemble name → allocated member ids.
pub(super) struct ImportNameMaps<'a> {
    /// doc node name → fresh id (owned values)
    name_to_id: HashMap<&'a str, String>,
    /// ensemble name → allocated join-node id
    join_by_name: HashMap<&'a str, &'a str>,
    /// ensemble name → allocated member node ids
    member_ids_by_name: HashMap<&'a str, &'a [String]>,
}

impl<'a> ImportNameMaps<'a> {
    fn new(
        name_to_id: HashMap<&'a str, String>,
        document: &'a GraphExportDocument,
        allocations: &'a ImportAllocations,
    ) -> Self {
        let join_by_name: HashMap<&'a str, &'a str> = document
            .ensembles
            .iter()
            .zip(&allocations.entries)
            .map(|(ensemble, allocated)| (ensemble.name.as_str(), allocated.join_id.as_str()))
            .collect();
        let member_ids_by_name: HashMap<&'a str, &'a [String]> = document
            .ensembles
            .iter()
            .zip(&allocations.entries)
            .map(|(ensemble, allocated)| (ensemble.name.as_str(), allocated.member_ids.as_slice()))
            .collect();
        Self {
            name_to_id,
            join_by_name,
            member_ids_by_name,
        }
    }

    pub(super) fn resolve_node_name(&self, name: &str) -> Result<String, String> {
        self.name_to_id
            .get(name)
            .cloned()
            .ok_or_else(|| format!("references unknown node '{name}'"))
    }

    fn resolve_target(
        &self,
        target: &GraphExportEnsembleTarget,
        field: &str,
        ensemble_name: &str,
    ) -> Result<String, String> {
        match target {
            GraphExportEnsembleTarget::Node(name) => self
                .resolve_node_name(name)
                .map_err(|e| format!("Ensemble '{ensemble_name}' {field} {e}.")),
            GraphExportEnsembleTarget::Ensemble { ensemble } => self
                .join_by_name
                .get(ensemble.as_str())
                .map(|id| (*id).to_string())
                .ok_or_else(|| {
                    format!(
                        "Ensemble '{ensemble_name}' {field} references unknown ensemble '{ensemble}'."
                    )
                }),
        }
    }
}

/// Fresh ids allocated for every document ensemble: the ensemble row id,
/// the join node id, and one fresh id per member node.
pub(super) struct ImportAllocations {
    entries: Vec<ImportAllocEntry>,
}

pub(super) struct ImportAllocEntry {
    ensemble_id: String,
    join_id: String,
    member_ids: Vec<String>,
}

/// The three ensemble targets, resolved once per ensemble before any plan
/// piece is built from them.
pub(super) struct ImportResolvedTargets {
    entry_from_node: String,
    on_pass_to: String,
    on_fail_to: Option<String>,
}

/// Duplicate node names make the document's edges/ensembles ambiguous, and
/// duplicate ensemble names make the by-name targets ambiguous — both are
/// refused before anything is built.
pub(super) fn reject_duplicate_import_names(document: &GraphExportDocument) -> Result<(), String> {
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for node in &document.nodes {
        *name_counts.entry(node.name.as_str()).or_insert(0) += 1;
    }
    let mut duplicate_names: Vec<&str> = name_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect();
    if !duplicate_names.is_empty() {
        duplicate_names.sort_unstable();
        return Err(format!(
            "Graph export document has duplicate node name(s): {}.",
            duplicate_names.join(", ")
        ));
    }

    let mut ensemble_name_counts: HashMap<&str, usize> = HashMap::new();
    for ensemble in &document.ensembles {
        *ensemble_name_counts
            .entry(ensemble.name.as_str())
            .or_insert(0) += 1;
    }
    let mut duplicate_ensemble_names: Vec<&str> = ensemble_name_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name)
        .collect();
    if !duplicate_ensemble_names.is_empty() {
        duplicate_ensemble_names.sort_unstable();
        return Err(format!(
            "Graph export document has duplicate ensemble name(s): {}.",
            duplicate_ensemble_names.join(", ")
        ));
    }
    Ok(())
}

/// Allocate a fresh id per document node and build the name → id map the
/// later phases resolve against; refuses an authorable `join` kind.
pub(super) fn build_import_nodes<'a>(
    document: &'a GraphExportDocument,
    graph_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(Vec<GraphNode>, HashMap<&'a str, String>), String> {
    let mut name_to_id: HashMap<&str, String> = HashMap::new();
    let mut nodes = Vec::with_capacity(document.nodes.len());
    for doc_node in &document.nodes {
        if doc_node.kind == GraphNodeKind::Join {
            return Err(format!(
                "Node '{}' has kind 'join', which is engine-managed and can only be created via an ensemble — it can never be authored directly.",
                doc_node.name
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        name_to_id.insert(doc_node.name.as_str(), id.clone());
        nodes.push(GraphNode {
            id,
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            name: doc_node.name.clone(),
            kind: doc_node.kind,
            config: doc_node.config.clone(),
            position: doc_node.position,
            created_at: now,
        });
    }
    Ok((nodes, name_to_id))
}

/// One fresh-id edge per document edge, endpoints resolved by name.
pub(super) fn build_import_edges(
    document: &GraphExportDocument,
    graph_id: &str,
    maps: &ImportNameMaps<'_>,
) -> Result<Vec<GraphEdge>, String> {
    let mut edges = Vec::with_capacity(document.edges.len());
    for doc_edge in &document.edges {
        let from_id = maps.resolve_node_name(&doc_edge.from_node).map_err(|e| {
            format!(
                "Edge '{}' -> '{}' {e}.",
                doc_edge.from_node, doc_edge.to_node
            )
        })?;
        let to_id = maps.resolve_node_name(&doc_edge.to_node).map_err(|e| {
            format!(
                "Edge '{}' -> '{}' {e}.",
                doc_edge.from_node, doc_edge.to_node
            )
        })?;
        edges.push(GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            from_node: from_id,
            to_node: to_id,
            condition: doc_edge.condition.clone(),
        });
    }
    Ok(edges)
}

/// Allocate the fresh ensemble/join/member ids, in document order.
pub(super) fn allocated_ensemble_ids(document: &GraphExportDocument) -> ImportAllocations {
    ImportAllocations {
        entries: document
            .ensembles
            .iter()
            .map(|ensemble| ImportAllocEntry {
                ensemble_id: uuid::Uuid::new_v4().to_string(),
                join_id: uuid::Uuid::new_v4().to_string(),
                member_ids: ensemble
                    .members
                    .iter()
                    .map(|_| uuid::Uuid::new_v4().to_string())
                    .collect(),
            })
            .collect(),
    }
}

/// Build one ensemble's complete unit — member nodes/edges, extra entry
/// sources, join node, exit edges, and the `Ensemble` row — advancing
/// `next_position` exactly once per member plus once for the join node.
pub(super) fn build_import_ensemble_plan(
    doc_ensemble: &GraphExportEnsemble,
    allocated: &ImportAllocEntry,
    maps: &ImportNameMaps<'_>,
    graph_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    next_position: &mut i64,
) -> Result<(GraphImportEnsemblePlan, Vec<GraphEdge>), String> {
    let import_kind = parse_import_ensemble_kind(doc_ensemble)?;
    let caps = crate::domain::graphs::MemberCaps::from_graphs(
        &crate::domain::canopy_config::CanopyConfig::load(&crate::domain::prompts::canopy_dir())
            .graphs,
    );
    validate_import_ensemble_shape(doc_ensemble, import_kind, caps)?;

    let targets = ImportResolvedTargets {
        entry_from_node: maps.resolve_target(
            &doc_ensemble.entry_from_node,
            "entry_from_node",
            &doc_ensemble.name,
        )?,
        on_pass_to: maps.resolve_target(
            &doc_ensemble.on_pass_to,
            "on_pass_to",
            &doc_ensemble.name,
        )?,
        on_fail_to: doc_ensemble
            .on_fail_to
            .as_ref()
            .map(|target| maps.resolve_target(target, "on_fail_to", &doc_ensemble.name))
            .transpose()?,
    };

    let mut edges: Vec<GraphEdge> = Vec::new();
    let (member_nodes, members, member_edges) = build_import_member_wiring(
        doc_ensemble,
        allocated,
        &targets.entry_from_node,
        graph_id,
        now,
        next_position,
    );
    edges.extend(member_edges);

    edges.extend(super::pending::import_extra_entry_source_edges(
        doc_ensemble,
        &targets.entry_from_node,
        maps,
        &allocated.member_ids,
        graph_id,
    )?);

    let join_node = import_join_node(doc_ensemble, allocated, graph_id, now, *next_position);
    *next_position += 1;

    edges.extend(import_exit_edges(
        doc_ensemble,
        allocated,
        &targets,
        maps,
        graph_id,
    ));

    // CM49: a crew-bound ensemble has no member edges to derive entry wiring
    // from, so its extra entry sources are recorded as pending rows the sync
    // path consumes once the crew supplies members. A literal-member ensemble
    // wires them as real edges above (`import_extra_entry_source_edges`).
    let pending_entry_sources = if doc_ensemble.crew.is_some() {
        super::pending::crew_bound_pending_entry_sources(
            doc_ensemble,
            &targets.entry_from_node,
            maps,
        )?
    } else {
        Vec::new()
    };

    let ensemble =
        import_ensemble_record(doc_ensemble, import_kind, allocated, targets, graph_id, now);

    Ok((
        GraphImportEnsemblePlan {
            ensemble,
            members,
            member_nodes,
            join_node,
            pending_entry_sources,
        },
        edges,
    ))
}

/// The document's ensemble `kind` (absent = the `parallel` default); an
/// unrecognized string is a refusal naming it (CB62).
pub(super) fn parse_import_ensemble_kind(
    doc_ensemble: &GraphExportEnsemble,
) -> Result<EnsembleKind, String> {
    match doc_ensemble.kind.as_deref() {
        None => Ok(EnsembleKind::Parallel),
        Some(value) => EnsembleKind::from_str(value).ok_or_else(|| {
            format!(
                "Ensemble '{}' has unknown kind '{value}'. Expected one of: parallel, cascade, round_robin.",
                doc_ensemble.name
            )
        }),
    }
}

/// Build every member node (agent config with effective prompt/timeout),
/// its entry edge from the primary source, its edge to the quorum, and its
/// `EnsembleMember` row — bumping `next_position` once per member.
pub(super) fn build_import_member_wiring(
    doc_ensemble: &GraphExportEnsemble,
    allocated: &ImportAllocEntry,
    entry_from_node: &str,
    graph_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    next_position: &mut i64,
) -> (Vec<GraphNode>, Vec<EnsembleMember>, Vec<GraphEdge>) {
    let mut member_nodes = Vec::with_capacity(doc_ensemble.members.len());
    let mut members = Vec::with_capacity(doc_ensemble.members.len());
    let mut edges = Vec::new();

    for (index, member) in doc_ensemble.members.iter().enumerate() {
        let node_id = allocated.member_ids[index].clone();
        let effective_prompt = member
            .prompt_override
            .as_deref()
            .unwrap_or(doc_ensemble.prompt_template.as_str());
        let effective_timeout_minutes = member
            .timeout_minutes
            .unwrap_or(doc_ensemble.timeout_minutes);
        member_nodes.push(GraphNode {
            id: node_id.clone(),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            name: format!("{} [{}]", doc_ensemble.name, index + 1),
            kind: GraphNodeKind::Agent,
            // A member's platform may legitimately be absent (see this
            // function's doc comment) — an empty string here is exactly
            // what `agent_nodes_missing_platform` looks for downstream.
            config: serde_json::json!({
                "platform": member.platform.clone().unwrap_or_default(),
                "model": member.model,
                "prompt_template": effective_prompt,
                "timeout_minutes": effective_timeout_minutes,
            }),
            position: *next_position,
            created_at: now,
        });
        edges.push(GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            from_node: entry_from_node.to_string(),
            to_node: node_id.clone(),
            condition: doc_ensemble.entry_condition.clone(),
        });
        edges.push(GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            from_node: node_id.clone(),
            to_node: allocated.join_id.clone(),
            condition: GraphEdgeCondition::Always,
        });
        members.push(EnsembleMember {
            ensemble_id: allocated.ensemble_id.clone(),
            node_id,
            position: index as i64,
            platform: member.platform.clone().unwrap_or_default(),
            model: member.model.clone(),
            prompt_override: member.prompt_override.clone(),
            timeout_minutes: member.timeout_minutes,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        });
        *next_position += 1;
    }

    (member_nodes, members, edges)
}

/// The ensemble's engine-managed quorum (join) node.
pub(super) fn import_join_node(
    doc_ensemble: &GraphExportEnsemble,
    allocated: &ImportAllocEntry,
    graph_id: &str,
    now: chrono::DateTime<chrono::Utc>,
    position: i64,
) -> GraphNode {
    GraphNode {
        id: allocated.join_id.clone(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: format!("{} (quorum)", doc_ensemble.name),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({ "ensemble_id": allocated.ensemble_id }),
        position,
        created_at: now,
    }
}

/// The quorum's pass (and optional fail) exit edges — fanning out to a
/// chained ensemble's member ids when the target names an ensemble.
pub(super) fn import_exit_edges(
    doc_ensemble: &GraphExportEnsemble,
    allocated: &ImportAllocEntry,
    targets: &ImportResolvedTargets,
    maps: &ImportNameMaps<'_>,
    graph_id: &str,
) -> Vec<GraphEdge> {
    let mut edges: Vec<GraphEdge> = Vec::new();
    let pass_targets = doc_ensemble
        .on_pass_to
        .as_ensemble_name()
        .and_then(|name| maps.member_ids_by_name.get(name))
        .map(|member_ids| member_ids.to_vec())
        .unwrap_or_else(|| vec![targets.on_pass_to.clone()]);
    for target in pass_targets {
        edges.push(GraphEdge {
            id: uuid::Uuid::new_v4().to_string(),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            from_node: allocated.join_id.clone(),
            to_node: target,
            condition: GraphEdgeCondition::Pass,
        });
    }
    if let Some(on_fail_to) = &targets.on_fail_to {
        let fail_targets = doc_ensemble
            .on_fail_to
            .as_ref()
            .and_then(GraphExportEnsembleTarget::as_ensemble_name)
            .and_then(|name| maps.member_ids_by_name.get(name))
            .map(|member_ids| member_ids.to_vec())
            .unwrap_or_else(|| vec![on_fail_to.clone()]);
        for target in fail_targets {
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: None,
                graph_id: Some(graph_id.to_string()),
                from_node: allocated.join_id.clone(),
                to_node: target,
                condition: GraphEdgeCondition::Fail,
            });
        }
    }
    edges
}

/// The `Ensemble` row for this unit; `import_kind` was parsed once at the
/// top of the plan builder and is reused here.
pub(super) fn import_ensemble_record(
    doc_ensemble: &GraphExportEnsemble,
    import_kind: EnsembleKind,
    allocated: &ImportAllocEntry,
    targets: ImportResolvedTargets,
    graph_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Ensemble {
    Ensemble {
        id: allocated.ensemble_id.clone(),
        spec_id: None,
        graph_id: Some(graph_id.to_string()),
        name: doc_ensemble.name.clone(),
        prompt_template: doc_ensemble.prompt_template.clone(),
        join_node_id: allocated.join_id.clone(),
        entry_from_node: targets.entry_from_node,
        entry_condition: doc_ensemble.entry_condition.clone(),
        min_pass: doc_ensemble.min_pass,
        straggler_timeout_minutes: doc_ensemble.straggler_timeout_minutes,
        quorum_grace_minutes: doc_ensemble.quorum_grace_minutes,
        timeout_minutes: doc_ensemble.timeout_minutes,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: targets.on_pass_to,
        on_fail_to: targets.on_fail_to,
        kind: import_kind,
        round_robin_index: if import_kind == EnsembleKind::RoundRobin {
            Some(0)
        } else {
            None
        },
        commit_rights: doc_ensemble.commit_rights,
        created_at: now,

        // CM49: the crew reference travels by name. `crew_id` and the applied
        // version are resolved against the local crews at persist time.
        crew_id: None,
        crew_name: doc_ensemble.crew.clone(),
        crew_version_applied: None,
    }
}

/// Graph-level structural validation (CB8): validate the complete expanded
/// graph as a whole. This catches unreachable nodes, multiple entry points,
/// missing outgoing coverage, and dangling edges — properties that only a
/// whole-graph check can see. For ensembles, the expanded graph includes
/// member/join nodes and wiring, so a document whose plain nodes appear
/// disconnected (kickoff/downstream with only ensemble bridging them) is
/// correctly considered connected. Validation errors come back with the
/// document's node names substituted for the fresh UUIDs where resolvable.
pub(super) fn collect_import_terminals(
    all_nodes: &[GraphNode],
    edges: &[GraphEdge],
) -> Result<Vec<String>, String> {
    let router_labels: Vec<Vec<String>> = all_nodes
        .iter()
        .map(|n| {
            if n.kind == GraphNodeKind::Router {
                extract_router_labels(&n.config)
            } else {
                Vec::new()
            }
        })
        .collect();
    let node_views: Vec<GraphNodeView> = all_nodes
        .iter()
        .enumerate()
        .map(|(idx, n)| GraphNodeView {
            id: &n.id,
            kind: n.kind,
            route_labels: &router_labels[idx],
        })
        .collect();
    let edge_views: Vec<GraphEdgeView> = edges
        .iter()
        .map(|e| GraphEdgeView {
            from: &e.from_node,
            to: &e.to_node,
            condition: &e.condition,
        })
        .collect();
    match validate_graph(&node_views, &edge_views) {
        Ok(report) => Ok(report
            .terminals
            .into_iter()
            .map(|terminal| {
                let display = all_nodes
                    .iter()
                    .find(|node| node.id == terminal.node_id)
                    .map(|node| format!("{} ({})", node.name, node.id))
                    .unwrap_or(terminal.node_id);
                format!("{} ends on '{}'", display, terminal.state)
            })
            .collect()),
        Err(e) => {
            // For import, the document names are more actionable than fresh
            // UUIDs, so enrich the message with names where we can resolve them.
            let mut enriched = e;
            // Try to map UUIDs back to names for friendlier messages: build id->name map.
            let id_to_name: std::collections::HashMap<&str, &str> = all_nodes
                .iter()
                .map(|n| (n.id.as_str(), n.name.as_str()))
                .collect();
            for (id, name) in id_to_name {
                if enriched.contains(id) {
                    enriched = enriched.replace(id, &format!("{name} ({id})"));
                }
            }
            Err(enriched)
        }
    }
}

/// Resolve a desired graph name against names already taken in the target
/// workdir: the name itself if free, else `"{name} (2)"`, `"{name} (3)"`,
/// etc. — decision 4's "import always creates a new graph" never overwrites,
/// so a taken name gets a suffix instead of a refusal.
pub fn resolve_unique_graph_name(existing_names: &[String], desired: &str) -> String {
    if !existing_names.iter().any(|name| name == desired) {
        return desired.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{desired} ({suffix})");
        if !existing_names.iter().any(|name| name == &candidate) {
            return candidate;
        }
        suffix += 1;
    }
}
