//! Graph export/import: a graph's design — name, description, nodes, edges,
//! and ensembles — serialized to and from one portable JSON document, so a
//! graph can leave one machine as a file and be recreated on another (spec
//! f0be8c66's successor: "a graph is worth sharing, not just narrating").
//!
//! Deliberately excludes ids, workdir, specs, and run/status state (a graph
//! design carrying someone else's backlog or run history would be a
//! surprise on arrival), and always includes `platform`/`model` (v2) so an
//! exported document round-trips its harness bindings.
//!
//! This module is pure — it never touches the database. `daemon::handler`'s
//! `graph_export`/`graph_import` MCP tools (and their `canopy graph
//! export`/`import` CLI counterparts) fetch/persist the surrounding data and
//! call into here for the document shape and structural validation, so
//! import is built on the same node/edge/ensemble shapes `graph_add_node`/
//! `graph_add_edge`/`graph_add_ensemble` produce rather than a second,
//! divergent write path.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::graphs::{
    Ensemble, EnsembleDetails, EnsembleKind, EnsembleMember, Graph, GraphEdge, GraphEdgeCondition,
    GraphNode, GraphNodeKind,
};
use crate::domain::validation::{
    validate_ensemble_min_pass, validate_ensembles_in_graph, validate_graph, GraphEdgeView,
    GraphNodeView,
};

/// The latest `format_version` this build writes. `graph_import` accepts `1`
/// (members with no binding), `2` (bindings included), and `3` (current:
/// graph/error vocabulary, `break` still read as `error` on import). An
/// unrecognized or missing version is a refusal, never a best-effort parse
/// (decision 6).
pub const GRAPH_EXPORT_FORMAT_VERSION: i64 = 3;

/// Ensemble member count bounds — mirrors `daemon::handler`'s
/// `ENSEMBLE_MIN_MEMBERS`/`ENSEMBLE_MAX_MEMBERS` (`graph_add_ensemble`'s own
/// authoring-time bounds). Duplicated rather than shared across the
/// domain/daemon boundary: the two numbers are part of the ensemble
/// contract itself, not an implementation detail either side owns alone.
const ENSEMBLE_MIN_MEMBERS: usize = 2;
const ENSEMBLE_MAX_MEMBERS: usize = 8;

/// A graph's design, portable across machines/installations. Key order is
/// deliberate (struct field order) — see `docs/graphs.md` for the documented,
/// hand-writable contract this type is the source of truth for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphExportDocument {
    pub format_version: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub nodes: Vec<GraphExportNode>,
    pub edges: Vec<GraphExportEdge>,
    #[serde(default)]
    pub ensembles: Vec<GraphExportEnsemble>,
    /// CM2: optional pre-wired target for infrastructure failures, referenced
    /// by node name (not id) — `None` when the graph has no infra node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_node: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphExportNode {
    pub name: String,
    pub kind: GraphNodeKind,
    pub position: i64,
    pub config: Value,
}

/// References nodes by `name`, never by id (decision 2) — what makes the
/// file reviewable, hand-editable, and diffable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphExportEdge {
    pub from_node: String,
    pub to_node: String,
    pub condition: GraphEdgeCondition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphExportEnsembleMember {
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub prompt_override: Option<String>,
    #[serde(default)]
    pub timeout_minutes: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GraphExportEnsembleTarget {
    Node(String),
    Ensemble { ensemble: String },
}

impl GraphExportEnsembleTarget {
    fn as_ensemble_name(&self) -> Option<&str> {
        match self {
            Self::Ensemble { ensemble } => Some(ensemble),
            Self::Node(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphExportEnsemble {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub prompt_template: String,
    pub entry_from_node: GraphExportEnsembleTarget,
    pub entry_condition: GraphEdgeCondition,
    pub on_pass_to: GraphExportEnsembleTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail_to: Option<GraphExportEnsembleTarget>,
    pub min_pass: i64,
    pub timeout_minutes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straggler_timeout_minutes: Option<i64>,
    pub members: Vec<GraphExportEnsembleMember>,
}

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

    // Decision 2's enforced consequence: two nodes sharing a name would
    // make the exported edges/ensembles ambiguous about which one they
    // mean, so export refuses outright rather than guessing.
    let mut name_counts: HashMap<&str, usize> = HashMap::new();
    for node in &plain_nodes {
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

    let id_to_name: HashMap<&str, &str> = plain_nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();
    let resolve_name = |node_id: &str| -> Result<String, String> {
        id_to_name
            .get(node_id)
            .map(|name| (*name).to_string())
            .ok_or_else(|| {
                format!(
                    "Graph '{}' references node '{node_id}' that is not part of its own graph.",
                    lp.name
                )
            })
    };
    let join_id_to_ensemble_name: HashMap<&str, &str> = ensembles
        .iter()
        .map(|details| {
            (
                details.ensemble.join_node_id.as_str(),
                details.ensemble.name.as_str(),
            )
        })
        .collect();
    let resolve_target = |node_id: &str| -> Result<GraphExportEnsembleTarget, String> {
        if let Some(name) = id_to_name.get(node_id) {
            return Ok(GraphExportEnsembleTarget::Node((*name).to_string()));
        }
        if let Some(name) = join_id_to_ensemble_name.get(node_id) {
            return Ok(GraphExportEnsembleTarget::Ensemble {
                ensemble: (*name).to_string(),
            });
        }
        Err(format!(
            "Graph '{}' references node '{}' that is not part of its own graph.",
            lp.name, node_id
        ))
    };

    let export_nodes = plain_nodes
        .iter()
        .map(|node| GraphExportNode {
            name: node.name.clone(),
            kind: node.kind,
            position: node.position,
            config: export_node_config(node.kind, &node.config),
        })
        .collect();

    let mut export_edges = Vec::new();
    for edge in graph_edges {
        if owned_ids.contains(edge.from_node.as_str()) || owned_ids.contains(edge.to_node.as_str())
        {
            continue;
        }
        export_edges.push(GraphExportEdge {
            from_node: resolve_name(&edge.from_node)?,
            to_node: resolve_name(&edge.to_node)?,
            condition: edge.condition.clone(),
        });
    }
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
        let ensemble = &details.ensemble;
        let on_fail_to = ensemble
            .on_fail_to
            .as_ref()
            .map(|target| resolve_target(target))
            .transpose()?;

        let mut sorted_members: Vec<&EnsembleMember> = details.members.iter().collect();
        sorted_members.sort_by_key(|m| m.position);
        let members = sorted_members
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
            .collect();

        export_ensembles.push(GraphExportEnsemble {
            name: ensemble.name.clone(),
            kind: if ensemble.kind == EnsembleKind::Parallel {
                None
            } else {
                Some(ensemble.kind.as_str().to_string())
            },
            prompt_template: ensemble.prompt_template.clone(),
            entry_from_node: resolve_target(&ensemble.entry_from_node)?,
            entry_condition: ensemble.entry_condition.clone(),
            on_pass_to: resolve_target(&ensemble.on_pass_to)?,
            on_fail_to,
            min_pass: ensemble.min_pass,
            timeout_minutes: ensemble.timeout_minutes,
            straggler_timeout_minutes: ensemble.straggler_timeout_minutes,
            members,
        });
    }

    let infra_node = lp.infra_node_id.as_deref().map(resolve_name).transpose()?;

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

/// An agent node's config passes through untouched, except that a missing
/// `model` key is injected as `null` so the export states "platform
/// default" explicitly, field-for-field with `graph_get`. Every other kind's
/// config passes through verbatim.
fn export_node_config(kind: GraphNodeKind, config: &Value) -> Value {
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

fn extract_router_labels(config: &Value) -> Vec<String> {
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

fn edge_sort_key(edge: &GraphExportEdge) -> String {
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
        Some(1) | Some(2) | Some(3) => {}
        Some(other) => {
            return Err(format!(
                "Graph export document has format_version {other}, but this build only supports 1, 2 and {GRAPH_EXPORT_FORMAT_VERSION}."
            ))
        }
        None => {
            return Err(
                "Graph export document is missing format_version; refusing to guess. Expected format_version: 3."
                    .to_string(),
            )
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
/// a node absent from `document.nodes`, an ensemble with 2-8 members
/// violated, or `min_pass`/timeout fields out of range.
pub fn build_import_plan(
    document: &GraphExportDocument,
    graph_id: &str,
) -> Result<GraphImportPlan, String> {
    if !matches!(document.format_version, 1..=3) {
        return Err(format!(
            "Graph export document has format_version {}, but this build only supports 1, 2 and {}.",
            document.format_version, GRAPH_EXPORT_FORMAT_VERSION
        ));
    }

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

    let now = chrono::Utc::now();
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

    for doc_node in &document.nodes {
        if let Some(prompt) = doc_node
            .config
            .get("prompt_template")
            .and_then(|v| v.as_str())
        {
            let mut rest = prompt;
            while let Some(start) = rest.find("{{output:") {
                let after_prefix = &rest[start + 9..];
                if let Some(end) = after_prefix.find("}}") {
                    let referenced_name = &after_prefix[..end];
                    if !name_to_id.contains_key(referenced_name) {
                        return Err(format!(
                            "Node '{}' references unknown node '{}' in {{{{output:{}}}}}.",
                            doc_node.name, referenced_name, referenced_name
                        ));
                    }
                    rest = &after_prefix[end + 2..];
                } else {
                    break;
                }
            }
        }
    }

    let resolve = |name: &str| -> Result<String, String> {
        name_to_id
            .get(name)
            .cloned()
            .ok_or_else(|| format!("references unknown node '{name}'"))
    };

    let mut edges = Vec::with_capacity(document.edges.len());
    for doc_edge in &document.edges {
        let from_id = resolve(&doc_edge.from_node).map_err(|e| {
            format!(
                "Edge '{}' -> '{}' {e}.",
                doc_edge.from_node, doc_edge.to_node
            )
        })?;
        let to_id = resolve(&doc_edge.to_node).map_err(|e| {
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

    // Ensemble-owned nodes continue the position sequence after every plain
    // node — the same "append after existing nodes" convention
    // `graph_add_ensemble`'s `start_position` uses.
    let mut next_position = nodes
        .iter()
        .map(|node| node.position)
        .max()
        .map(|max| max + 1)
        .unwrap_or(1);

    let allocations: Vec<(String, String, Vec<String>)> = document
        .ensembles
        .iter()
        .map(|ensemble| {
            (
                uuid::Uuid::new_v4().to_string(),
                uuid::Uuid::new_v4().to_string(),
                ensemble
                    .members
                    .iter()
                    .map(|_| uuid::Uuid::new_v4().to_string())
                    .collect(),
            )
        })
        .collect();
    let ensemble_name_to_join_id: HashMap<&str, &str> = document
        .ensembles
        .iter()
        .zip(&allocations)
        .map(|(ensemble, (_, join_id, _))| (ensemble.name.as_str(), join_id.as_str()))
        .collect();
    let ensemble_name_to_member_ids: HashMap<&str, &[String]> = document
        .ensembles
        .iter()
        .zip(&allocations)
        .map(|(ensemble, (_, _, member_ids))| (ensemble.name.as_str(), member_ids.as_slice()))
        .collect();
    let resolve_target = |target: &GraphExportEnsembleTarget,
                          field: &str,
                          ensemble_name: &str|
     -> Result<String, String> {
        match target {
            GraphExportEnsembleTarget::Node(name) => resolve(name)
                .map_err(|e| format!("Ensemble '{ensemble_name}' {field} {e}.")),
            GraphExportEnsembleTarget::Ensemble { ensemble } => ensemble_name_to_join_id
                .get(ensemble.as_str())
                .map(|id| (*id).to_string())
                .ok_or_else(|| {
                    format!(
                        "Ensemble '{ensemble_name}' {field} references unknown ensemble '{ensemble}'."
                    )
                }),
        }
    };

    let mut ensembles = Vec::with_capacity(document.ensembles.len());
    for (doc_ensemble, (ensemble_id, join_node_id, member_node_ids)) in
        document.ensembles.iter().zip(&allocations)
    {
        let import_kind = doc_ensemble
            .kind
            .as_deref()
            .and_then(EnsembleKind::from_str)
            .unwrap_or(EnsembleKind::Parallel);
        let min_members = match import_kind {
            EnsembleKind::Parallel => ENSEMBLE_MIN_MEMBERS,
            EnsembleKind::Cascade | EnsembleKind::RoundRobin => 1,
        };
        if doc_ensemble.members.len() < min_members
            || doc_ensemble.members.len() > ENSEMBLE_MAX_MEMBERS
        {
            return Err(format!(
                "Ensemble '{}' must have {min_members}-{ENSEMBLE_MAX_MEMBERS} members, got {}.",
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

        let entry_from_node = resolve_target(
            &doc_ensemble.entry_from_node,
            "entry_from_node",
            &doc_ensemble.name,
        )?;
        let on_pass_to =
            resolve_target(&doc_ensemble.on_pass_to, "on_pass_to", &doc_ensemble.name)?;
        let on_fail_to = doc_ensemble
            .on_fail_to
            .as_ref()
            .map(|target| resolve_target(target, "on_fail_to", &doc_ensemble.name))
            .transpose()?;

        let mut member_nodes = Vec::with_capacity(doc_ensemble.members.len());
        let mut members = Vec::with_capacity(doc_ensemble.members.len());

        for (index, member) in doc_ensemble.members.iter().enumerate() {
            let node_id = member_node_ids[index].clone();
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
                position: next_position,
                created_at: now,
            });
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: None,
                graph_id: Some(graph_id.to_string()),
                from_node: entry_from_node.clone(),
                to_node: node_id.clone(),
                condition: doc_ensemble.entry_condition.clone(),
            });
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: None,
                graph_id: Some(graph_id.to_string()),
                from_node: node_id.clone(),
                to_node: join_node_id.clone(),
                condition: GraphEdgeCondition::Always,
            });
            members.push(EnsembleMember {
                ensemble_id: ensemble_id.clone(),
                node_id,
                position: index as i64,
                platform: member.platform.clone().unwrap_or_default(),
                model: member.model.clone(),
                prompt_override: member.prompt_override.clone(),
                timeout_minutes: member.timeout_minutes,
            });
            next_position += 1;
        }

        let join_node = GraphNode {
            id: join_node_id.clone(),
            spec_id: None,
            graph_id: Some(graph_id.to_string()),
            name: format!("{} (quorum)", doc_ensemble.name),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({ "ensemble_id": ensemble_id }),
            position: next_position,
            created_at: now,
        };
        next_position += 1;

        let pass_targets = doc_ensemble
            .on_pass_to
            .as_ensemble_name()
            .and_then(|name| ensemble_name_to_member_ids.get(name))
            .map(|member_ids| member_ids.to_vec())
            .unwrap_or_else(|| vec![on_pass_to.clone()]);
        for target in pass_targets {
            edges.push(GraphEdge {
                id: uuid::Uuid::new_v4().to_string(),
                spec_id: None,
                graph_id: Some(graph_id.to_string()),
                from_node: join_node_id.clone(),
                to_node: target,
                condition: GraphEdgeCondition::Pass,
            });
        }
        if let Some(on_fail_to) = &on_fail_to {
            let fail_targets = doc_ensemble
                .on_fail_to
                .as_ref()
                .and_then(GraphExportEnsembleTarget::as_ensemble_name)
                .and_then(|name| ensemble_name_to_member_ids.get(name))
                .map(|member_ids| member_ids.to_vec())
                .unwrap_or_else(|| vec![on_fail_to.clone()]);
            for target in fail_targets {
                edges.push(GraphEdge {
                    id: uuid::Uuid::new_v4().to_string(),
                    spec_id: None,
                    graph_id: Some(graph_id.to_string()),
                    from_node: join_node_id.clone(),
                    to_node: target,
                    condition: GraphEdgeCondition::Fail,
                });
            }
        }

        let import_kind = doc_ensemble
            .kind
            .as_deref()
            .and_then(EnsembleKind::from_str)
            .unwrap_or(EnsembleKind::Parallel);

        ensembles.push(GraphImportEnsemblePlan {
            ensemble: Ensemble {
                id: ensemble_id.clone(),
                spec_id: None,
                graph_id: Some(graph_id.to_string()),
                name: doc_ensemble.name.clone(),
                prompt_template: doc_ensemble.prompt_template.clone(),
                join_node_id: join_node_id.clone(),
                entry_from_node,
                entry_condition: doc_ensemble.entry_condition.clone(),
                min_pass: doc_ensemble.min_pass,
                straggler_timeout_minutes: doc_ensemble.straggler_timeout_minutes,
                timeout_minutes: doc_ensemble.timeout_minutes,
                on_pass_to,
                on_fail_to,
                kind: import_kind,
                round_robin_index: if import_kind == EnsembleKind::RoundRobin {
                    Some(0)
                } else {
                    None
                },
                created_at: now,
            },
            members,
            member_nodes,
            join_node,
        });
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

    // Graph-level structural validation (CB8): validate the complete expanded
    // graph as a whole. This catches unreachable nodes, multiple entry points,
    // missing outgoing coverage, and dangling edges — properties that only a
    // whole-graph check can see. For ensembles, the expanded graph includes
    // member/join nodes and wiring, so a document whose plain nodes appear
    // disconnected (kickoff/downstream with only ensemble bridging them) is
    // correctly considered connected.
    let terminals = {
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
            Ok(report) => report
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
                .collect(),
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
                return Err(enriched);
            }
        }
    };

    let infra_node_id = document.infra_node.as_deref().map(resolve).transpose()?;

    Ok(GraphImportPlan {
        nodes,
        edges,
        ensembles,
        terminals,
        infra_node_id,
    })
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
fn node_config_has_harness(config: &Value) -> bool {
    let has_non_empty_str = |field: &str| {
        config
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    has_non_empty_str("platform") || has_non_empty_str("cli")
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_graph(name: &str) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            id: "graph-1".to_string(),
            name: name.to_string(),
            description: Some("A test graph".to_string()),
            workdir: "/tmp/project".to_string(),
            status: crate::domain::graphs::GraphStatus::Draft,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    fn make_node(
        id: &str,
        name: &str,
        kind: GraphNodeKind,
        config: Value,
        position: i64,
    ) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            name: name.to_string(),
            kind,
            config,
            position,
            created_at: Utc::now(),
        }
    }

    fn make_edge(id: &str, from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
        GraphEdge {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition,
        }
    }

    /// A simple 3-node chain (implementer -> gate -> committer), no
    /// ensembles: the base case every other test builds on.
    fn simple_graph() -> (Graph, Vec<GraphNode>, Vec<GraphEdge>) {
        let lp = make_graph("simple-graph");
        let nodes = vec![
            make_node(
                "n1",
                "implementer",
                GraphNodeKind::Agent,
                serde_json::json!({"platform": "claude", "model": "opus", "prompt_template": "implement it"}),
                1,
            ),
            make_node(
                "n2",
                "gate",
                GraphNodeKind::Check,
                serde_json::json!({"command": "cargo test"}),
                2,
            ),
            make_node(
                "n3",
                "committer",
                GraphNodeKind::Agent,
                serde_json::json!({"platform": "claude", "prompt_template": "commit it"}),
                3,
            ),
        ];
        let edges = vec![
            make_edge("e1", "n1", "n2", GraphEdgeCondition::Always),
            make_edge("e2", "n2", "n3", GraphEdgeCondition::Always),
        ];
        (lp, nodes, edges)
    }

    #[test]
    fn export_v2_always_includes_agent_bindings_and_null_model() {
        let (lp, nodes, edges) = simple_graph();
        let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
        assert_eq!(doc.format_version, GRAPH_EXPORT_FORMAT_VERSION);
        let implementer = doc.nodes.iter().find(|n| n.name == "implementer").unwrap();
        assert_eq!(implementer.config["platform"], "claude");
        assert_eq!(implementer.config["model"], "opus");
        assert_eq!(implementer.config["prompt_template"], "implement it");
        // The committer stores no model (platform default): export states
        // it explicitly as null rather than omitting the key.
        let committer = doc.nodes.iter().find(|n| n.name == "committer").unwrap();
        assert_eq!(committer.config["platform"], "claude");
        assert!(committer.config.get("model").is_some());
        assert!(committer.config["model"].is_null());
        // Non-binding keys pass through untouched.
        assert_eq!(committer.config["prompt_template"], "commit it");
    }

    #[test]
    fn export_v2_members_carry_bindings_in_position_order() {
        let lp = make_graph("member-order-graph");
        let kickoff = make_node(
            "kickoff",
            "kickoff",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            1,
        );
        let downstream = make_node(
            "downstream",
            "downstream",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_template": "wrap up"}),
            10,
        );
        let member1 = make_node(
            "m1",
            "Team [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "copilot", "prompt_template": "draft it", "timeout_minutes": 30}),
            2,
        );
        let member2 = make_node(
            "m2",
            "Team [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "opencode", "model": "opencode-go/qwen3.7-plus", "prompt_template": "draft it", "timeout_minutes": 30}),
            3,
        );
        let member3 = make_node(
            "m3",
            "Team [3]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "opencode", "model": "opencode/muse-spark", "prompt_template": "custom", "timeout_minutes": 30}),
            4,
        );
        let join = make_node(
            "join1",
            "Team (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens1"}),
            5,
        );
        let nodes = vec![kickoff, downstream, member1, member2, member3, join];
        let edges = vec![
            make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
            make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
            make_edge("e3", "kickoff", "m3", GraphEdgeCondition::Always),
            make_edge("e4", "m1", "join1", GraphEdgeCondition::Always),
            make_edge("e5", "m2", "join1", GraphEdgeCondition::Always),
            make_edge("e6", "m3", "join1", GraphEdgeCondition::Always),
            make_edge("e7", "join1", "downstream", GraphEdgeCondition::Pass),
        ];
        // Members stored out of position order on purpose: export must
        // still emit them in position order.
        let mut ensemble_details =
            make_ensemble_details("ens1", "join1", "kickoff", "downstream", &["m1", "m2"]);
        ensemble_details.members.push(EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m3".to_string(),
            position: 2,
            platform: "opencode".to_string(),
            model: Some("opencode/muse-spark".to_string()),
            prompt_override: Some("custom angle".to_string()),
            timeout_minutes: Some(3),
        });
        ensemble_details.members[0] = EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "copilot".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        };
        ensemble_details.members[1] = EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m2".to_string(),
            position: 1,
            platform: "opencode".to_string(),
            model: Some("opencode-go/qwen3.7-plus".to_string()),
            prompt_override: None,
            timeout_minutes: None,
        };
        // Shuffle the stored order so the test pins the sort, not the input.
        ensemble_details.members.swap(0, 2);
        ensemble_details.ensemble.name = "Team".to_string();

        let doc = build_export_document(&lp, &nodes, &edges, &[ensemble_details]).unwrap();
        let team = doc.ensembles.iter().find(|e| e.name == "Team").unwrap();
        assert_eq!(team.members.len(), 3);
        assert_eq!(team.members[0].platform.as_deref(), Some("copilot"));
        assert_eq!(team.members[0].model, None);
        assert_eq!(team.members[0].prompt_override, None);
        assert_eq!(team.members[1].platform.as_deref(), Some("opencode"));
        assert_eq!(
            team.members[1].model.as_deref(),
            Some("opencode-go/qwen3.7-plus")
        );
        assert_eq!(team.members[2].platform.as_deref(), Some("opencode"));
        assert_eq!(
            team.members[2].model.as_deref(),
            Some("opencode/muse-spark")
        );
        assert_eq!(
            team.members[2].prompt_override.as_deref(),
            Some("custom angle")
        );
        assert_eq!(team.members[2].timeout_minutes, Some(3));
        assert_eq!(team.members[0].timeout_minutes, None);
        // Optional fields serialize as explicit nulls, never omitted keys.
        let raw = serde_json::to_string(&doc).unwrap();
        assert!(raw.contains("\"model\":null"));
        assert!(raw.contains("\"prompt_override\":null"));
        assert!(raw.contains("\"platform\":\"copilot\""));
    }

    #[test]
    fn export_rejects_duplicate_node_names() {
        let (lp, mut nodes, edges) = simple_graph();
        nodes[1].name = "implementer".to_string();
        let err = build_export_document(&lp, &nodes, &edges, &[]).unwrap_err();
        assert!(err.contains("implementer"));
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn format_version_first_key_in_serialized_json() {
        let (lp, nodes, edges) = simple_graph();
        let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
        let raw = serde_json::to_string(&doc).unwrap();
        // struct field order drives serde_json's key order.
        assert!(raw.starts_with("{\"format_version\":3"));
    }

    #[test]
    fn import_rejects_missing_format_version() {
        let value = serde_json::json!({
            "name": "x", "nodes": [], "edges": [], "ensembles": []
        });
        let err = parse_export_document_value(&value).unwrap_err();
        assert!(err.contains("format_version"));
    }

    #[test]
    fn import_rejects_unsupported_format_version() {
        let value = serde_json::json!({
            "format_version": 99, "name": "x", "nodes": [], "edges": [], "ensembles": []
        });
        let err = parse_export_document_value(&value).unwrap_err();
        assert!(err.contains("99"));
    }

    #[test]
    fn import_accepts_format_versions_1_2_and_3() {
        for version in [1, 2, 3] {
            let value = serde_json::json!({
                "format_version": version, "name": "x", "nodes": [], "edges": [], "ensembles": []
            });
            let doc = parse_export_document_value(&value)
                .unwrap_or_else(|e| panic!("version {version} must parse: {e}"));
            assert_eq!(doc.format_version, version);
        }
    }

    #[test]
    fn import_v2_break_condition_is_error() {
        let value = serde_json::json!({
            "format_version": 2,
            "name": "legacy",
            "nodes": [
                {"name": "a", "kind": "check", "position": 1, "config": {"command": "true"}},
                {"name": "b", "kind": "check", "position": 2, "config": {"command": "true"}}
            ],
            "edges": [{"from_node": "a", "to_node": "b", "condition": "break"}],
            "ensembles": []
        });
        let document = parse_export_document_value(&value).unwrap();
        assert_eq!(document.edges[0].condition, GraphEdgeCondition::Error);
        let exported = serde_json::to_value(GraphExportDocument {
            format_version: GRAPH_EXPORT_FORMAT_VERSION,
            ..document
        })
        .unwrap();
        assert_eq!(exported["format_version"], 3);
        assert_eq!(exported["edges"][0]["condition"], "error");
    }

    #[test]
    fn import_plan_rejects_edge_naming_nonexistent_node() {
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![GraphExportNode {
                name: "only".to_string(),
                kind: GraphNodeKind::Check,
                position: 1,
                config: serde_json::json!({"command": "true"}),
            }],
            edges: vec![GraphExportEdge {
                from_node: "only".to_string(),
                to_node: "ghost".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            ensembles: vec![],
            infra_node: None,
        };
        let err = build_import_plan(&doc, "new-graph").unwrap_err();
        assert!(err.contains("ghost"));
    }

    #[test]
    fn import_plan_rejects_join_kind_node() {
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![GraphExportNode {
                name: "sneaky".to_string(),
                kind: GraphNodeKind::Join,
                position: 1,
                config: serde_json::json!({}),
            }],
            edges: vec![],
            ensembles: vec![],
            infra_node: None,
        };
        let err = build_import_plan(&doc, "new-graph").unwrap_err();
        assert!(err.contains("join"));
    }

    #[test]
    fn import_plan_assigns_fresh_ids_and_graph_id() {
        let (lp, nodes, edges) = simple_graph();
        let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
        let plan = build_import_plan(&doc, "brand-new-graph-id").unwrap();
        assert_eq!(plan.nodes.len(), 3);
        for node in &plan.nodes {
            assert_eq!(node.graph_id.as_deref(), Some("brand-new-graph-id"));
            assert!(node.spec_id.is_none());
            assert!(!nodes.iter().any(|n| n.id == node.id), "id must be fresh");
        }
        assert_eq!(plan.edges.len(), 2);
    }

    #[test]
    fn agent_nodes_missing_platform_reports_unbound_v1_nodes() {
        // A v1-style document carries no bindings: every agent node is
        // reported. Built by hand (not via export, which always binds).
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "implementer".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({"prompt_template": "implement it"}),
                },
                GraphExportNode {
                    name: "gate".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "cargo test"}),
                },
                GraphExportNode {
                    name: "committer".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 3,
                    config: serde_json::json!({"prompt_template": "commit it"}),
                },
            ],
            edges: vec![
                GraphExportEdge {
                    from_node: "implementer".to_string(),
                    to_node: "gate".to_string(),
                    condition: GraphEdgeCondition::Always,
                },
                GraphExportEdge {
                    from_node: "gate".to_string(),
                    to_node: "committer".to_string(),
                    condition: GraphEdgeCondition::Always,
                },
            ],
            ensembles: vec![],
            infra_node: None,
        };
        let plan = build_import_plan(&doc, "graph-2").unwrap();
        let missing = agent_nodes_missing_platform(&plan);
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&"implementer".to_string()));
        assert!(missing.contains(&"committer".to_string()));
    }

    #[test]
    fn agent_nodes_missing_platform_empty_for_v2_export() {
        let (lp, nodes, edges) = simple_graph();
        let doc = build_export_document(&lp, &nodes, &edges, &[]).unwrap();
        let plan = build_import_plan(&doc, "graph-2").unwrap();
        assert!(agent_nodes_missing_platform(&plan).is_empty());
    }

    #[test]
    fn resolve_unique_graph_name_returns_desired_when_free() {
        let existing = vec!["other".to_string()];
        assert_eq!(resolve_unique_graph_name(&existing, "my-graph"), "my-graph");
    }

    #[test]
    fn resolve_unique_graph_name_suffixes_on_collision() {
        let existing = vec!["my-graph".to_string(), "my-graph (2)".to_string()];
        assert_eq!(
            resolve_unique_graph_name(&existing, "my-graph"),
            "my-graph (3)"
        );
    }

    fn make_ensemble_details(
        ensemble_id: &str,
        join_id: &str,
        entry_from: &str,
        on_pass_to: &str,
        member_ids: &[&str],
    ) -> EnsembleDetails {
        let ensemble = Ensemble {
            id: ensemble_id.to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: join_id.to_string(),
            entry_from_node: entry_from.to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: member_ids.len() as i64,
            straggler_timeout_minutes: None,
            timeout_minutes: 30,
            on_pass_to: on_pass_to.to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: Utc::now(),
        };
        let members = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| EnsembleMember {
                ensemble_id: ensemble_id.to_string(),
                node_id: id.to_string(),
                position: i as i64,
                platform: "openrouter".to_string(),
                model: Some(format!("model-{i}")),
                prompt_override: None,
                timeout_minutes: None,
            })
            .collect();
        EnsembleDetails { ensemble, members }
    }

    /// The full ensemble round trip: build a graph with a kickoff node, a
    /// 2-member ensemble, and a downstream node; export it, import it under
    /// a new graph id, and export the result again. Requirement 5 (round
    /// trip, with bindings included on both ends) and the acceptance
    /// criterion ("an ensemble survives the round trip as an ensemble, not
    /// as expanded member nodes") both pin on this.
    #[test]
    fn ensemble_round_trips_as_an_ensemble_not_expanded_nodes() {
        let lp = make_graph("ensemble-graph");
        let kickoff = make_node(
            "kickoff",
            "kickoff",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            1,
        );
        let downstream = make_node(
            "downstream",
            "downstream",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "claude", "prompt_template": "wrap up"}),
            10,
        );
        let member1 = make_node(
            "m1",
            "Proposers [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "openrouter", "model": "model-0", "prompt_template": "draft it", "timeout_minutes": 30}),
            2,
        );
        let member2 = make_node(
            "m2",
            "Proposers [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "openrouter", "model": "model-1", "prompt_template": "draft it", "timeout_minutes": 30}),
            3,
        );
        let join = make_node(
            "join1",
            "Proposers (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens1"}),
            4,
        );
        let nodes = vec![kickoff, downstream, member1, member2, join];
        let edges = vec![
            make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
            make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
            make_edge("e3", "m1", "join1", GraphEdgeCondition::Always),
            make_edge("e4", "m2", "join1", GraphEdgeCondition::Always),
            make_edge("e5", "join1", "downstream", GraphEdgeCondition::Pass),
        ];
        let ensembles = vec![make_ensemble_details(
            "ens1",
            "join1",
            "kickoff",
            "downstream",
            &["m1", "m2"],
        )];

        let first_export = build_export_document(&lp, &nodes, &edges, &ensembles).unwrap();

        // Ensemble survives as one ensemble entry; the plain node list
        // excludes the member/join nodes entirely.
        assert_eq!(first_export.ensembles.len(), 1);
        assert_eq!(first_export.nodes.len(), 2);
        assert!(!first_export
            .nodes
            .iter()
            .any(|n| n.name.contains("Proposers")));
        assert_eq!(first_export.ensembles[0].members.len(), 2);

        let plan = build_import_plan(&first_export, "graph-2").unwrap();
        assert_eq!(plan.ensembles.len(), 1);
        assert_eq!(plan.ensembles[0].member_nodes.len(), 2);

        // Re-export the imported plan under a Graph with the same name as
        // the original (import always renames on collision, but here we
        // simulate "no collision" so the round trip is name-for-name) and
        // assert the document is identical except for the name change we
        // deliberately introduce.
        let mut imported_all_nodes = plan.nodes.clone();
        let mut imported_ensemble_details = Vec::new();
        for ens in &plan.ensembles {
            imported_all_nodes.push(ens.join_node.clone());
            imported_all_nodes.extend(ens.member_nodes.iter().cloned());
            imported_ensemble_details.push(EnsembleDetails {
                ensemble: ens.ensemble.clone(),
                members: ens.members.clone(),
            });
        }
        let mut lp2 = make_graph("ensemble-graph");
        lp2.id = "graph-2".to_string();
        let second_export = build_export_document(
            &lp2,
            &imported_all_nodes,
            &plan.edges,
            &imported_ensemble_details,
        )
        .unwrap();

        assert_eq!(first_export.name, second_export.name);
        assert_eq!(first_export.description, second_export.description);
        assert_eq!(first_export.nodes, second_export.nodes);
        assert_eq!(first_export.edges, second_export.edges);
        assert_eq!(first_export.ensembles, second_export.ensembles);
    }

    #[test]
    fn chained_ensembles_export_as_tables_and_import_to_join_fanout() {
        let lp = make_graph("canopy-v4");
        let nodes = vec![
            make_node(
                "kickoff",
                "kickoff",
                GraphNodeKind::Check,
                serde_json::json!({}),
                1,
            ),
            make_node(
                "final",
                "final",
                GraphNodeKind::Check,
                serde_json::json!({}),
                2,
            ),
            make_node(
                "m1",
                "Reviewer 1 [1]",
                GraphNodeKind::Agent,
                serde_json::json!({}),
                3,
            ),
            make_node(
                "m2",
                "Reviewer 1 [2]",
                GraphNodeKind::Agent,
                serde_json::json!({}),
                4,
            ),
            make_node(
                "n1",
                "Reviewer 2 [1]",
                GraphNodeKind::Agent,
                serde_json::json!({}),
                5,
            ),
            make_node(
                "n2",
                "Reviewer 2 [2]",
                GraphNodeKind::Agent,
                serde_json::json!({}),
                6,
            ),
            make_node(
                "j1",
                "Reviewer 1 (quorum)",
                GraphNodeKind::Join,
                serde_json::json!({}),
                7,
            ),
            make_node(
                "j2",
                "Reviewer 2 (quorum)",
                GraphNodeKind::Join,
                serde_json::json!({}),
                8,
            ),
        ];
        let edges = vec![
            make_edge("e1", "kickoff", "m1", GraphEdgeCondition::Always),
            make_edge("e2", "kickoff", "m2", GraphEdgeCondition::Always),
            make_edge("e3", "m1", "j1", GraphEdgeCondition::Always),
            make_edge("e4", "m2", "j1", GraphEdgeCondition::Always),
            make_edge("e5", "j1", "n1", GraphEdgeCondition::Pass),
            make_edge("e6", "j1", "n2", GraphEdgeCondition::Pass),
            make_edge("e7", "n1", "j2", GraphEdgeCondition::Always),
            make_edge("e8", "n2", "j2", GraphEdgeCondition::Always),
            make_edge("e9", "j2", "final", GraphEdgeCondition::Pass),
        ];
        let mut reviewer1 = make_ensemble_details("ens1", "j1", "kickoff", "j2", &["m1", "m2"]);
        reviewer1.ensemble.name = "Reviewer 1".to_string();
        let mut reviewer2 = make_ensemble_details("ens2", "j2", "kickoff", "final", &["n1", "n2"]);
        reviewer2.ensemble.name = "Reviewer 2".to_string();

        let document = build_export_document(&lp, &nodes, &edges, &[reviewer1, reviewer2]).unwrap();
        assert_eq!(
            document.ensembles[0].on_pass_to,
            GraphExportEnsembleTarget::Ensemble {
                ensemble: "Reviewer 2".to_string()
            }
        );
        assert!(serde_json::to_string(&document)
            .unwrap()
            .contains("\"ensemble\":\"Reviewer 2\""));
        assert!(document.edges.iter().all(|edge| {
            edge.from_node != "Reviewer 1 (quorum)"
                && edge.to_node != "Reviewer 1 (quorum)"
                && edge.from_node != "Reviewer 2 (quorum)"
                && edge.to_node != "Reviewer 2 (quorum)"
        }));

        let plan = build_import_plan(&document, "graph-2").unwrap();
        let imported_r1 = plan
            .ensembles
            .iter()
            .find(|ensemble| ensemble.ensemble.name == "Reviewer 1")
            .unwrap();
        let imported_r2 = plan
            .ensembles
            .iter()
            .find(|ensemble| ensemble.ensemble.name == "Reviewer 2")
            .unwrap();
        assert_eq!(
            imported_r1.ensemble.on_pass_to,
            imported_r2.ensemble.join_node_id
        );
        for member in &imported_r2.members {
            assert!(plan.edges.iter().any(|edge| {
                edge.from_node == imported_r1.ensemble.join_node_id
                    && edge.to_node == member.node_id
                    && edge.condition == GraphEdgeCondition::Pass
            }));
        }
    }

    #[test]
    fn export_refuses_duplicate_ensemble_names() {
        let lp = make_graph("duplicate-ensembles");
        let mut first = make_ensemble_details("ens1", "j1", "kickoff", "final", &["m1", "m2"]);
        let mut second = make_ensemble_details("ens2", "j2", "kickoff", "final", &["n1", "n2"]);
        first.ensemble.name = "Reviewer 1".to_string();
        second.ensemble.name = "Reviewer 1".to_string();
        let error = build_export_document(&lp, &[], &[], &[first, second]).unwrap_err();
        assert!(error.contains("duplicate ensemble name"));
        assert!(error.contains("Reviewer 1"));
    }

    #[test]
    fn import_rejects_unknown_ensemble_target() {
        let document = GraphExportDocument {
            format_version: 3,
            name: "unknown-target".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({}),
                },
                GraphExportNode {
                    name: "final".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({}),
                },
            ],
            edges: vec![],
            ensembles: vec![GraphExportEnsemble {
                name: "Reviewer 1".to_string(),
                kind: None,
                prompt_template: "review".to_string(),
                entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                entry_condition: GraphEdgeCondition::Always,
                on_pass_to: GraphExportEnsembleTarget::Ensemble {
                    ensemble: "Ghost".to_string(),
                },
                on_fail_to: Some(GraphExportEnsembleTarget::Node("final".to_string())),
                min_pass: 2,
                timeout_minutes: 10,
                straggler_timeout_minutes: None,
                members: vec![
                    GraphExportEnsembleMember {
                        platform: Some("x".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                    GraphExportEnsembleMember {
                        platform: Some("x".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                ],
            }],
            infra_node: None,
        };
        let error = build_import_plan(&document, "graph-1").unwrap_err();
        assert!(error.contains("unknown ensemble") && error.contains("Ghost"));
    }

    /// Requirement 5's full statement: export -> import -> export again
    /// produces an identical document except for the name, for a plain
    /// (non-ensemble) graph too.
    #[test]
    fn plain_graph_round_trips_identically_except_name() {
        let (lp, nodes, edges) = simple_graph();
        let first_export = build_export_document(&lp, &nodes, &edges, &[]).unwrap();

        let plan = build_import_plan(&first_export, "graph-2").unwrap();
        let mut lp2 = make_graph("renamed-on-import");
        lp2.id = "graph-2".to_string();
        let second_export = build_export_document(&lp2, &plan.nodes, &plan.edges, &[]).unwrap();

        assert_ne!(first_export.name, second_export.name);
        assert_eq!(second_export.name, "renamed-on-import");
        assert_eq!(first_export.description, second_export.description);
        assert_eq!(first_export.nodes, second_export.nodes);
        assert_eq!(first_export.edges, second_export.edges);
        assert_eq!(first_export.ensembles, second_export.ensembles);
    }

    /// Export → import → export of a graph holding one multi-member ensemble
    /// with distinct platform/model per member plus a solo agent node:
    /// every harness binding must survive identically.
    #[test]
    fn v2_export_import_export_preserves_bindings() {
        let lp = make_graph("bindings-graph");
        let solo = make_node(
            "solo",
            "solo",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "opencode", "model": "opencode/muse-spark", "prompt_template": "go solo", "timeout_minutes": 15}),
            1,
        );
        let downstream = make_node(
            "downstream",
            "downstream",
            GraphNodeKind::Check,
            serde_json::json!({"command": "true"}),
            10,
        );
        let member1 = make_node(
            "m1",
            "Crew [1]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "copilot", "prompt_template": "draft it", "timeout_minutes": 30}),
            2,
        );
        let member2 = make_node(
            "m2",
            "Crew [2]",
            GraphNodeKind::Agent,
            serde_json::json!({"platform": "opencode", "model": "opencode-go/qwen3.7-plus", "prompt_template": "draft it", "timeout_minutes": 30}),
            3,
        );
        let join = make_node(
            "join1",
            "Crew (quorum)",
            GraphNodeKind::Join,
            serde_json::json!({"ensemble_id": "ens1"}),
            4,
        );
        let nodes = vec![solo, downstream, member1, member2, join];
        let edges = vec![
            make_edge("e1", "solo", "m1", GraphEdgeCondition::Always),
            make_edge("e2", "solo", "m2", GraphEdgeCondition::Always),
            make_edge("e3", "m1", "join1", GraphEdgeCondition::Always),
            make_edge("e4", "m2", "join1", GraphEdgeCondition::Always),
            make_edge("e5", "join1", "downstream", GraphEdgeCondition::Pass),
        ];
        let mut details =
            make_ensemble_details("ens1", "join1", "solo", "downstream", &["m1", "m2"]);
        details.ensemble.name = "Crew".to_string();
        details.members[0].platform = "copilot".to_string();
        details.members[0].model = None;
        details.members[0].prompt_override = Some("first angle".to_string());
        details.members[1].platform = "opencode".to_string();
        details.members[1].model = Some("opencode-go/qwen3.7-plus".to_string());

        let first = build_export_document(&lp, &nodes, &edges, &[details]).unwrap();
        let plan = build_import_plan(&first, "graph-2").unwrap();
        assert!(agent_nodes_missing_platform(&plan).is_empty());

        let mut imported_all_nodes = plan.nodes.clone();
        let mut imported_details = Vec::new();
        for ens in &plan.ensembles {
            imported_all_nodes.push(ens.join_node.clone());
            imported_all_nodes.extend(ens.member_nodes.iter().cloned());
            imported_details.push(EnsembleDetails {
                ensemble: ens.ensemble.clone(),
                members: ens.members.clone(),
            });
        }
        let mut lp2 = make_graph("bindings-graph");
        lp2.id = "graph-2".to_string();
        let second =
            build_export_document(&lp2, &imported_all_nodes, &plan.edges, &imported_details)
                .unwrap();

        // Harness bindings identical across the round trip, node and member.
        let first_solo = first.nodes.iter().find(|n| n.name == "solo").unwrap();
        let second_solo = second.nodes.iter().find(|n| n.name == "solo").unwrap();
        assert_eq!(
            first_solo.config["platform"],
            second_solo.config["platform"]
        );
        assert_eq!(first_solo.config["model"], second_solo.config["model"]);
        assert_eq!(first.ensembles, second.ensembles);
        assert_eq!(first.nodes, second.nodes);
    }

    /// Import must not fail when the document names a platform the importing
    /// machine has not configured: the node is created as written, and
    /// reporting an unusable pair belongs to `graph_preflight`.
    #[test]
    fn import_v2_with_unconfigured_platform_succeeds() {
        let doc = GraphExportDocument {
            format_version: 2,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "odd".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({
                        "platform": "never-configured-xyz",
                        "model": "some-model-abc",
                        "prompt_template": "go",
                    }),
                },
                GraphExportNode {
                    name: "next".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![GraphExportEdge {
                from_node: "odd".to_string(),
                to_node: "next".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            ensembles: vec![],
            infra_node: None,
        };
        let plan = build_import_plan(&doc, "new-graph")
            .expect("unconfigured platform must import, not error");
        let odd = plan.nodes.iter().find(|n| n.name == "odd").unwrap();
        assert_eq!(odd.config["platform"], "never-configured-xyz");
        assert_eq!(odd.config["model"], "some-model-abc");
    }

    /// A `format_version: 2` document must stay readable after later fields
    /// are added: unknown fields are ignored rather than rejected, at the
    /// document, node, and member levels.
    #[test]
    fn unknown_fields_are_ignored() {
        let value = serde_json::json!({
            "format_version": 2,
            "name": "future-graph",
            "future_field": 123,
            "nodes": [
                {"name": "a", "kind": "check", "position": 1, "config": {"command": "true"}, "future_node_field": "x"},
                {"name": "b", "kind": "check", "position": 2, "config": {"command": "true"}},
            ],
            "edges": [
                {"from_node": "a", "to_node": "b", "condition": "always"}
            ],
            "ensembles": [
                {
                    "name": "team",
                    "prompt_template": "go",
                    "entry_from_node": "a",
                    "entry_condition": "always",
                    "on_pass_to": "b",
                    "min_pass": 2,
                    "timeout_minutes": 30,
                    "members": [
                        {"platform": "claude", "model": null, "prompt_override": null, "future_member_field": [1, 2]},
                        {"platform": "claude", "model": "opus", "prompt_override": null}
                    ]
                }
            ]
        });
        let document = parse_export_document_value(&value)
            .expect("unknown future fields must not break parsing");
        let plan = build_import_plan(&document, "new-graph")
            .expect("unknown future fields must not break import");
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.ensembles.len(), 1);
        assert_eq!(plan.ensembles[0].members[1].model.as_deref(), Some("opus"));
    }

    /// Pins `docs/graphs.md`'s worked example to the actual format: if this
    /// test ever fails to parse/import, the doc's example has drifted from
    /// what the code accepts and needs updating alongside it.
    #[test]
    fn docs_worked_example_parses_and_imports_cleanly() {
        let raw = r#"{
          "format_version": 1,
          "name": "implement-and-review",
          "description": "Implement a spec, get two model opinions, then commit.",
          "nodes": [
            {
              "name": "implementer",
              "kind": "agent",
              "position": 1,
              "config": {
                "prompt_template": "Implement: {{spec_content}}",
                "timeout_minutes": 30
              }
            },
            {
              "name": "committer",
              "kind": "agent",
              "position": 4,
              "config": {
                "prompt_template": "Review the feedback and commit if satisfied.",
                "commit_rights": true,
                "timeout_minutes": 15
              }
            }
          ],
          "edges": [],
          "ensembles": [
            {
              "name": "reviewers",
              "prompt_template": "Review this diff for correctness: {{previous_feedback}}",
              "entry_from_node": "implementer",
              "entry_condition": "always",
              "on_pass_to": "committer",
              "min_pass": 2,
              "timeout_minutes": 20,
              "members": [
                {},
                {}
              ]
            }
          ]
        }"#;

        let document = parse_export_document_str(raw).expect("doc example must parse");
        let plan =
            build_import_plan(&document, "graph-from-docs").expect("doc example must import");
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.ensembles.len(), 1);
        assert_eq!(plan.ensembles[0].member_nodes.len(), 2);
        let missing = agent_nodes_missing_platform(&plan);
        assert_eq!(
            missing.len(),
            4,
            "implementer, committer, and both reviewer members are missing a platform: {missing:?}"
        );
    }

    #[test]
    fn import_plan_rejects_ensemble_with_too_few_members() {
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "next".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![],
            ensembles: vec![GraphExportEnsemble {
                name: "solo".to_string(),
                kind: None,
                prompt_template: "go".to_string(),
                entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                entry_condition: GraphEdgeCondition::Always,
                on_pass_to: GraphExportEnsembleTarget::Node("next".to_string()),
                on_fail_to: None,
                min_pass: 1,
                timeout_minutes: 30,
                straggler_timeout_minutes: None,
                members: vec![GraphExportEnsembleMember {
                    platform: Some("claude".to_string()),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                }],
            }],
            infra_node: None,
        };
        let err = build_import_plan(&doc, "new-graph").unwrap_err();
        assert!(err.contains("2-8 members"));
    }

    /// CM23 (FR6, requirement 5's import half): a member's own exported
    /// `timeout_minutes` is what gets baked into that member's node config on
    /// import — not the ensemble's shared value — while a sibling with no
    /// override still falls back to the ensemble's `timeout_minutes`.
    #[test]
    fn import_plan_bakes_member_timeout_override_into_node_config() {
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "next".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![],
            ensembles: vec![GraphExportEnsemble {
                name: "team".to_string(),
                kind: None,
                prompt_template: "go".to_string(),
                entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                entry_condition: GraphEdgeCondition::Always,
                on_pass_to: GraphExportEnsembleTarget::Node("next".to_string()),
                on_fail_to: None,
                min_pass: 1,
                timeout_minutes: 20,
                straggler_timeout_minutes: None,
                members: vec![
                    GraphExportEnsembleMember {
                        platform: Some("claude".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: Some(3),
                    },
                    GraphExportEnsembleMember {
                        platform: Some("claude".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                ],
            }],
            infra_node: None,
        };
        let plan = build_import_plan(&doc, "new-graph").unwrap();
        let ensemble_plan = &plan.ensembles[0];
        assert_eq!(ensemble_plan.members[0].timeout_minutes, Some(3));
        assert_eq!(
            ensemble_plan.member_nodes[0].config["timeout_minutes"],
            serde_json::json!(3)
        );
        assert_eq!(ensemble_plan.members[1].timeout_minutes, None);
        assert_eq!(
            ensemble_plan.member_nodes[1].config["timeout_minutes"],
            serde_json::json!(20)
        );
    }

    #[test]
    fn import_plan_rejects_unreachable_node() {
        // A -> B, C self-graph (single entry A, C unreachable)
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "A".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "a"}),
                },
                GraphExportNode {
                    name: "B".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 2,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "b"}),
                },
                GraphExportNode {
                    name: "C".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 3,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "c"}),
                },
            ],
            edges: vec![
                GraphExportEdge {
                    from_node: "A".to_string(),
                    to_node: "B".to_string(),
                    condition: GraphEdgeCondition::Always,
                },
                GraphExportEdge {
                    from_node: "C".to_string(),
                    to_node: "C".to_string(),
                    condition: GraphEdgeCondition::Always,
                },
            ],
            ensembles: vec![],
            infra_node: None,
        };
        let err = build_import_plan(&doc, "new-graph").unwrap_err();
        assert!(err.contains('C'), "err should name C: {err}");
        assert!(
            err.to_lowercase().contains("unreachable"),
            "err should mention unreachable: {err}"
        );
    }

    #[test]
    fn import_plan_rejects_multiple_entry_points() {
        // A and B both with no incoming => multiple entries
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "A".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "a"}),
                },
                GraphExportNode {
                    name: "B".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 2,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "b"}),
                },
                GraphExportNode {
                    name: "C".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 3,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "c"}),
                },
            ],
            edges: vec![GraphExportEdge {
                from_node: "A".to_string(),
                to_node: "C".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            ensembles: vec![],
            infra_node: None,
        };
        let err = build_import_plan(&doc, "new-graph").unwrap_err();
        assert!(err.to_lowercase().contains("entry"), "err: {err}");
        assert!(err.contains('A'), "err should name A: {err}");
        assert!(err.contains('B'), "err should name B: {err}");
    }

    #[test]
    fn import_plan_accepts_agent_missing_fail_edge_as_terminal() {
        // Resilience node with only pass edge -> fail is a terminal exit,
        // not an error (same verdict as graph_preflight).
        let doc = GraphExportDocument {
            format_version: 1,
            name: "x".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "resilience".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "go"}),
                },
                GraphExportNode {
                    name: "next".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 2,
                    config: serde_json::json!({"platform": "claude", "prompt_template": "next"}),
                },
            ],
            edges: vec![GraphExportEdge {
                from_node: "resilience".to_string(),
                to_node: "next".to_string(),
                condition: GraphEdgeCondition::Pass,
            }],
            ensembles: vec![],
            infra_node: None,
        };
        let plan =
            build_import_plan(&doc, "new-graph").expect("missing fail is a terminal, not an error");
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.terminals.len(), 3);
        assert!(plan
            .terminals
            .iter()
            .any(|terminal| terminal.starts_with("resilience (")
                && terminal.ends_with(") ends on 'fail'")));
    }

    #[test]
    fn build_import_plan_rejects_unknown_node_reference() {
        let doc = GraphExportDocument {
            format_version: GRAPH_EXPORT_FORMAT_VERSION,
            name: "test".to_string(),
            description: None,
            nodes: vec![GraphExportNode {
                name: "Worker".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({
                    "platform": "test",
                    "prompt_template": "See {{output:Nonexistent}}"
                }),
                position: 1,
            }],
            edges: vec![],
            ensembles: vec![],
            infra_node: None,
        };
        let err = build_import_plan(&doc, "new-graph").unwrap_err();
        assert!(
            err.contains("references unknown node 'Nonexistent'"),
            "err: {err}"
        );
    }

    #[test]
    fn build_import_plan_accepts_valid_node_reference() {
        let doc = GraphExportDocument {
            format_version: GRAPH_EXPORT_FORMAT_VERSION,
            name: "test".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "Architect".to_string(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({"platform": "test"}),
                    position: 1,
                },
                GraphExportNode {
                    name: "Implementer".to_string(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({
                        "platform": "test",
                        "prompt_template": "Follow {{output:Architect}}"
                    }),
                    position: 2,
                },
                GraphExportNode {
                    name: "Resilience".to_string(),
                    kind: GraphNodeKind::Agent,
                    config: serde_json::json!({"platform": "test"}),
                    position: 3,
                },
            ],
            edges: vec![
                GraphExportEdge {
                    from_node: "Architect".to_string(),
                    to_node: "Implementer".to_string(),
                    condition: GraphEdgeCondition::Pass,
                },
                GraphExportEdge {
                    from_node: "Architect".to_string(),
                    to_node: "Resilience".to_string(),
                    condition: GraphEdgeCondition::Fail,
                },
                GraphExportEdge {
                    from_node: "Implementer".to_string(),
                    to_node: "Resilience".to_string(),
                    condition: GraphEdgeCondition::Pass,
                },
                GraphExportEdge {
                    from_node: "Implementer".to_string(),
                    to_node: "Resilience".to_string(),
                    condition: GraphEdgeCondition::Fail,
                },
            ],
            ensembles: vec![],
            infra_node: None,
        };
        let result = build_import_plan(&doc, "new-graph");
        assert!(
            result.is_ok(),
            "valid {{output:NodeName}} reference must be accepted: {:?}",
            result.err()
        );
    }
}
