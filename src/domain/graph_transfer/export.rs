use super::*;

/// Build a [`GraphExportDocument`] from a graph's already-fetched graph.
///
/// `graph_nodes`/`graph_edges` are the graph's *entire* top-level graph — the
/// same rows `list_graph_nodes_for_graph`/`list_graph_edges_for_graph` return,
/// including ensemble member/join nodes and their wiring edges. This
/// function is what tells the two apart: every node/edge owned by an
/// ensemble (a member node, the join node, and the edges wiring them) is
/// excluded from `nodes`/`edges` and represented instead as one
/// [`GraphExportEnsemble`] entry, so an ensemble survives the round trip as
/// an ensemble, not as expanded member nodes.
pub fn build_export_document(
    lp: &Graph,
    graph_nodes: &[GraphNode],
    graph_edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
) -> Result<GraphExportDocument, String> {
    let owned_ids: std::collections::HashSet<&str> = ensembles
        .iter()
        .flat_map(|details| {
            details
                .members
                .iter()
                .map(|member| member.node_id.as_str())
                .chain(std::iter::once(details.ensemble.join_node_id.as_str()))
        })
        .collect();

    let mut plain_nodes: Vec<&GraphNode> = graph_nodes
        .iter()
        .filter(|node| !owned_ids.contains(node.id.as_str()))
        .collect();
    plain_nodes.sort_by_key(|node| node.position);

    reject_duplicate_export_names(lp, &plain_nodes, ensembles)?;
    let name_map = ExportNameMap::new(lp, &plain_nodes, ensembles);

    let export_nodes = plain_nodes
        .iter()
        .map(|node| GraphExportNode {
            name: node.name.clone(),
            kind: node.kind,
            position: node.position,
            config: export_node_config(node.kind, &node.config),
        })
        .collect();

    let mut export_edges = export_plain_edges(graph_edges, &owned_ids, &name_map)?;
    export_edges.sort_by_key(edge_sort_key);

    let mut sorted_ensembles: Vec<&EnsembleDetails> = ensembles.iter().collect();
    sorted_ensembles.sort_by_key(|details| {
        graph_nodes
            .iter()
            .find(|node| node.id == details.ensemble.join_node_id)
            .map(|node| node.position)
            .unwrap_or(i64::MAX)
    });

    let mut export_ensembles = Vec::with_capacity(sorted_ensembles.len());
    for details in sorted_ensembles {
        export_ensembles.push(export_one_ensemble(
            details,
            graph_edges,
            &owned_ids,
            &name_map,
        )?);
    }

    let infra_node = lp
        .infra_node_id
        .as_deref()
        .map(|id| name_map.node_name(id))
        .transpose()?;

    Ok(GraphExportDocument {
        format_version: GRAPH_EXPORT_FORMAT_VERSION,
        name: lp.name.clone(),
        description: lp.description.clone(),
        nodes: export_nodes,
        edges: export_edges,
        ensembles: export_ensembles,
        infra_node,
    })
}

/// Name-resolution knowledge shared by the export phases: plain-node id →
/// name, ensemble quorum id → ensemble name, plus the graph name every
/// "not part of its own graph" error carries.
pub(super) struct ExportNameMap<'a> {
    lp_name: &'a str,
    id_to_name: HashMap<&'a str, &'a str>,
    join_to_ensemble: HashMap<&'a str, &'a str>,
}

impl<'a> ExportNameMap<'a> {
    fn new(lp: &'a Graph, plain_nodes: &[&'a GraphNode], ensembles: &'a [EnsembleDetails]) -> Self {
        let id_to_name: HashMap<&'a str, &'a str> = plain_nodes
            .iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect();
        let join_to_ensemble: HashMap<&'a str, &'a str> = ensembles
            .iter()
            .map(|details| {
                (
                    details.ensemble.join_node_id.as_str(),
                    details.ensemble.name.as_str(),
                )
            })
            .collect();
        Self {
            lp_name: lp.name.as_str(),
            id_to_name,
            join_to_ensemble,
        }
    }

    fn node_name(&self, node_id: &str) -> Result<String, String> {
        self.id_to_name
            .get(node_id)
            .map(|name| (*name).to_string())
            .ok_or_else(|| {
                format!(
                    "Graph '{}' references node '{node_id}' that is not part of its own graph.",
                    self.lp_name
                )
            })
    }

    fn target(&self, node_id: &str) -> Result<GraphExportEnsembleTarget, String> {
        if let Some(name) = self.id_to_name.get(node_id) {
            return Ok(GraphExportEnsembleTarget::Node((*name).to_string()));
        }
        if let Some(name) = self.join_to_ensemble.get(node_id) {
            return Ok(GraphExportEnsembleTarget::Ensemble {
                ensemble: (*name).to_string(),
            });
        }
        Err(format!(
            "Graph '{}' references node '{}' that is not part of its own graph.",
            self.lp_name, node_id
        ))
    }
}

/// Decision 2's enforced consequence: two nodes (or two ensembles) sharing a
/// name would make the exported edges/ensembles ambiguous about which one
/// they mean, so export refuses outright rather than guessing.
pub(super) fn reject_duplicate_export_names(
    lp: &Graph,
    plain_nodes: &[&GraphNode],
    ensembles: &[EnsembleDetails],
) -> Result<(), String> {
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for node in plain_nodes {
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
            "Graph '{}' has duplicate node name(s): {}. Export requires unique node names, since the exported file references nodes by name — rename before exporting.",
            lp.name,
            duplicate_names.join(", ")
        ));
    }

    let mut ensemble_name_counts: HashMap<&str, usize> = HashMap::new();
    for details in ensembles {
        *ensemble_name_counts
            .entry(details.ensemble.name.as_str())
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
            "Graph '{}' has duplicate ensemble name(s): {}. Export requires unique ensemble names, since the exported file references ensembles by name — rename before exporting.",
            lp.name,
            duplicate_ensemble_names.join(", ")
        ));
    }
    Ok(())
}

/// The graph's plain (non-ensemble-owned) edges, referenced by node name.
pub(super) fn export_plain_edges(
    graph_edges: &[GraphEdge],
    owned_ids: &std::collections::HashSet<&str>,
    name_map: &ExportNameMap<'_>,
) -> Result<Vec<GraphExportEdge>, String> {
    let mut export_edges = Vec::new();
    for edge in graph_edges {
        if owned_ids.contains(edge.from_node.as_str()) || owned_ids.contains(edge.to_node.as_str())
        {
            continue;
        }
        export_edges.push(GraphExportEdge {
            from_node: name_map.node_name(&edge.from_node)?,
            to_node: name_map.node_name(&edge.to_node)?,
            condition: edge.condition.clone(),
        });
    }
    Ok(export_edges)
}

/// One ensemble's full export entry: resolved targets, sorted members, and
/// the derived extra entry sources (CB71).
pub(super) fn export_one_ensemble(
    details: &EnsembleDetails,
    graph_edges: &[GraphEdge],
    owned_ids: &std::collections::HashSet<&str>,
    name_map: &ExportNameMap<'_>,
) -> Result<GraphExportEnsemble, String> {
    let ensemble = &details.ensemble;
    let on_fail_to = ensemble
        .on_fail_to
        .as_ref()
        .map(|target| name_map.target(target))
        .transpose()?;

    let members = export_ensemble_members(details);
    let extra_entry_sources =
        export_ensemble_entry_sources(details, graph_edges, owned_ids, name_map)?;

    Ok(GraphExportEnsemble {
        name: ensemble.name.clone(),
        kind: if ensemble.kind == EnsembleKind::Parallel {
            None
        } else {
            Some(ensemble.kind.as_str().to_string())
        },
        commit_rights: ensemble.commit_rights,
        prompt_template: ensemble.prompt_template.clone(),
        entry_from_node: name_map.target(&ensemble.entry_from_node)?,
        entry_condition: ensemble.entry_condition.clone(),
        extra_entry_sources,
        on_pass_to: name_map.target(&ensemble.on_pass_to)?,
        on_fail_to,
        min_pass: ensemble.min_pass,
        timeout_minutes: ensemble.timeout_minutes,
        straggler_timeout_minutes: ensemble.straggler_timeout_minutes,
        quorum_grace_minutes: ensemble.quorum_grace_minutes,
        members,
    })
}

/// The ensemble's members sorted by position, with empty platforms exported
/// as `None`.
pub(super) fn export_ensemble_members(details: &EnsembleDetails) -> Vec<GraphExportEnsembleMember> {
    let mut sorted_members: Vec<&EnsembleMember> = details.members.iter().collect();
    sorted_members.sort_by_key(|m| m.position);
    sorted_members
        .iter()
        .map(|member| GraphExportEnsembleMember {
            platform: if member.platform.is_empty() {
                None
            } else {
                Some(member.platform.clone())
            },
            model: member.model.clone(),
            prompt_override: member.prompt_override.clone(),
            timeout_minutes: member.timeout_minutes,
        })
        .collect()
}

/// CB71: every entry source beyond the primary lives only in the
/// member-fan-out edges, which the export `edges` list skips. Derive
/// them here. Edges from ANY ensemble-owned node (this unit's
/// members/join and other units' joins — chained exits, already
/// carried by the upstream ensemble's on_pass_to/on_fail_to) are not
/// entry sources.
pub(super) fn export_ensemble_entry_sources(
    details: &EnsembleDetails,
    graph_edges: &[GraphEdge],
    owned_ids: &std::collections::HashSet<&str>,
    name_map: &ExportNameMap<'_>,
) -> Result<Vec<GraphExportEntrySource>, String> {
    let ensemble = &details.ensemble;
    let member_ids: std::collections::HashSet<&str> = details
        .members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let mut seen_sources: std::collections::HashSet<(&str, &GraphEdgeCondition)> =
        std::collections::HashSet::new();
    seen_sources.insert((ensemble.entry_from_node.as_str(), &ensemble.entry_condition));
    let mut extra_entry_sources: Vec<GraphExportEntrySource> = Vec::new();
    for edge in graph_edges {
        if !member_ids.contains(edge.to_node.as_str())
            || owned_ids.contains(edge.from_node.as_str())
            || !seen_sources.insert((edge.from_node.as_str(), &edge.condition))
        {
            continue;
        }
        extra_entry_sources.push(GraphExportEntrySource {
            from_node: name_map.node_name(&edge.from_node)?,
            condition: edge.condition.clone(),
        });
    }
    extra_entry_sources.sort_by_key(|source| {
        format!(
            "{}\u{0}{}\u{0}{}",
            source.from_node,
            source.condition.as_str(),
            source.condition.route_label().unwrap_or("")
        )
    });
    Ok(extra_entry_sources)
}

/// An agent node's config passes through untouched, except that a missing
/// `model` key is injected as `null` so the export states "platform
/// default" explicitly, field-for-field with `graph_get`. Every other kind's
/// config passes through verbatim.
pub(super) fn export_node_config(kind: GraphNodeKind, config: &Value) -> Value {
    if kind != GraphNodeKind::Agent {
        return config.clone();
    }
    if let Some(map) = config.as_object() {
        if map.contains_key("model") {
            return config.clone();
        }
        let mut map = map.clone();
        map.insert("model".to_string(), Value::Null);
        return Value::Object(map);
    }
    config.clone()
}

pub(super) fn extract_router_labels(config: &Value) -> Vec<String> {
    config
        .get("routes")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    entry
                        .get("label")
                        .and_then(Value::as_str)
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn edge_sort_key(edge: &GraphExportEdge) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}",
        edge.from_node,
        edge.to_node,
        edge.condition.as_str(),
        edge.condition.route_label().unwrap_or("")
    )
}

/// Parse and validate a `format_version` before attempting the full
/// deserialization, so a missing/unsupported version is refused with its
/// own clear message rather than falling through to a generic serde error
/// (decision 6).
pub fn parse_export_document_value(value: &Value) -> Result<GraphExportDocument, String> {
    match value.get("format_version").and_then(Value::as_i64) {
        Some(1) | Some(2) | Some(3) | Some(4) => {}
        Some(other) => {
            return Err(format!(
                "Graph export document has format_version {other}, but this build only supports 1 through {GRAPH_EXPORT_FORMAT_VERSION}."
            ))
        }
        None => {
            return Err(format!(
                "Graph export document is missing format_version; refusing to guess. Expected format_version: {GRAPH_EXPORT_FORMAT_VERSION}."
            ))
        }
    }
    serde_json::from_value(value.clone())
        .map_err(|e| format!("Invalid graph export document: {e}."))
}

/// [`parse_export_document_value`] from raw JSON text — the CLI's entry
/// point for a file's (or stdin's) contents.
pub fn parse_export_document_str(raw: &str) -> Result<GraphExportDocument, String> {
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| format!("Invalid graph export file: not valid JSON ({e})."))?;
    parse_export_document_value(&value)
}
