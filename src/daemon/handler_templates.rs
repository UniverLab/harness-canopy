//! Write-time prompt-template binding checks: the same placeholder set the
//! spawn path binds, enforced when a template is written instead of
//! discovered member by member at run time.
//!
//! Seen 2026-10-26: an ensemble template carrying `{{output:Designer}}`
//! (Designer is an ensemble, not a node) was accepted at write time, and at
//! run time every member refused to spawn one after another, leaving the
//! ensemble with no verdict and the graph blocked. Every authoring path —
//! `graph_node`, `graph_node`, `graph_ensemble`,
//! `graph_ensemble`, `graph_edit` — now runs this check before
//! persisting, and `graph_control` reports templates written before the fix
//! as `graph_warnings`.
//!
//! Only literal templates stored in a config are checked (`prompt_template`,
//! `resume_prompt`, ensemble shared prompts and member overrides). A
//! `prompt_preset` names a file whose body can change after the write, so
//! presets keep their spawn-time check and are never resolved here.

use crate::daemon::handler_edges::GraphTarget;
use crate::db::Database;
use crate::domain::graphs::{EnsembleDetails, GraphDetails, GraphNode, GraphNodeKind};
use serde_json::Value;

/// The names one scope — a spec's graph or a graph's top-level graph — can
/// bind in an `{{output:<name>}}` marker: every node name, plus every
/// ensemble name paired with its quorum node's name (the intended source when
/// an author writes `{{output:<ensemble>}}` by mistake; `None` while the
/// quorum row is missing).
pub(crate) struct TemplateScope {
    pub(crate) nodes: Vec<String>,
    pub(crate) ensembles: Vec<(String, Option<String>)>,
}

impl TemplateScope {
    pub(crate) fn load(
        db: &Database,
        spec_id: Option<&str>,
        graph_id: Option<&str>,
    ) -> Result<Self, String> {
        let (nodes, ensembles): (Vec<GraphNode>, Vec<EnsembleDetails>) = match (spec_id, graph_id) {
            (Some(spec_id), _) => (
                db.list_graph_nodes(spec_id).map_err(|e| e.to_string())?,
                db.list_ensembles_for_spec(spec_id)
                    .map_err(|e| e.to_string())?,
            ),
            (None, Some(graph_id)) => (
                db.list_graph_nodes_for_graph(graph_id)
                    .map_err(|e| e.to_string())?,
                db.list_ensembles_for_graph(graph_id)
                    .map_err(|e| e.to_string())?,
            ),
            (None, None) => (Vec::new(), Vec::new()),
        };
        Ok(Self::from_parts(&nodes, &ensembles))
    }

    pub(crate) fn from_parts(nodes: &[GraphNode], ensembles: &[EnsembleDetails]) -> Self {
        let node_names = nodes.iter().map(|n| n.name.clone()).collect();
        let ensemble_names = ensembles
            .iter()
            .map(|d| {
                let quorum = nodes
                    .iter()
                    .find(|n| n.id == d.ensemble.join_node_id)
                    .map(|n| n.name.clone());
                (d.ensemble.name.clone(), quorum)
            })
            .collect();
        Self {
            nodes: node_names,
            ensembles: ensemble_names,
        }
    }
}

/// One placeholder in a template that nothing can bind.
enum UnbindableHit {
    /// `{{output:<name>}}` where `<name>` is an ensemble: outputs are read
    /// from nodes, so this is refused with the quorum hint, never rendered.
    EnsembleOutput {
        target: String,
        quorum: Option<String>,
    },
    /// Any other marker the binding set cannot bind.
    Unknown { placeholder: String },
}

/// Every placeholder in `template` that nothing can bind: ensemble-named
/// `{{output:...}}` markers first (they get their own message), then whatever
/// the shared spawn-path check
/// ([`crate::graph_engine::unbindable_placeholders_in`]) reports. The two
/// lists never overlap — an ensemble-named output is deduplicated out of the
/// shared check's result.
fn unbindable_hits(
    template: &str,
    supported: &[&str],
    node_names: &[String],
    ensembles: &[(String, Option<String>)],
) -> Vec<UnbindableHit> {
    let mut hits = Vec::new();
    for name in crate::domain::prompts::placeholder_names(template) {
        if let Some(target) = name.strip_prefix("output:") {
            if let Some((_, quorum)) = ensembles.iter().find(|(n, _)| n == target) {
                hits.push(UnbindableHit::EnsembleOutput {
                    target: target.to_string(),
                    quorum: quorum.clone(),
                });
            }
        }
    }
    for placeholder in
        crate::graph_engine::unbindable_placeholders_in(template, supported, Some(node_names))
    {
        let already_reported = hits.iter().any(|hit| match hit {
            UnbindableHit::EnsembleOutput { target, .. } => {
                placeholder == format!("output:{target}")
            }
            UnbindableHit::Unknown { .. } => false,
        });
        if !already_reported {
            hits.push(UnbindableHit::Unknown { placeholder });
        }
    }
    hits
}

fn accepted_forms_text(supported: &[&str], node_names: &[String]) -> String {
    let mut forms: Vec<String> = supported.iter().map(|s| format!("{{{{{s}}}}}")).collect();
    if node_names.is_empty() {
        forms.push(
            "no {{output:<node>}} reference (this template cannot read node outputs)".to_string(),
        );
    } else {
        forms.push(format!(
            "{{{{output:<node>}}}} for a node in this graph ({})",
            node_names.join(", ")
        ));
    }
    forms.join(", ")
}

fn ensemble_output_message(owner: &str, target: &str, quorum: Option<&str>) -> String {
    match quorum {
        Some(quorum) => format!(
            "{owner} reads {{{{output:{target}}}}}, but outputs are read from nodes, not ensembles — \
             '{target}' is an ensemble. Did you mean its quorum node '{quorum}'?"
        ),
        None => format!(
            "{owner} reads {{{{output:{target}}}}}, but outputs are read from nodes, not ensembles — \
             '{target}' is an ensemble, which has no readable output of its own. Reference a node instead."
        ),
    }
}

/// Refuse `template` when it carries a placeholder nothing can bind (FR1):
/// an `{{output:<name>}}` naming an ensemble gets the FR2 message (outputs
/// are read from nodes, with the quorum hint); anything else the shared
/// spawn-path check reports is refused naming the owner, the placeholder and
/// the accepted forms. A literal `\{{...}}` is never a hit.
pub(crate) fn check_template_bindings(
    owner: &str,
    template: &str,
    supported: &[&str],
    node_names: &[String],
    ensembles: &[(String, Option<String>)],
) -> Result<(), String> {
    let hits = unbindable_hits(template, supported, node_names, ensembles);
    let ensemble_hit = hits.iter().find_map(|hit| match hit {
        UnbindableHit::EnsembleOutput { target, quorum } => Some((target, quorum)),
        UnbindableHit::Unknown { .. } => None,
    });
    if let Some((target, quorum)) = ensemble_hit {
        return Err(ensemble_output_message(owner, target, quorum.as_deref()));
    }
    let unknown: Vec<&str> = hits
        .iter()
        .filter_map(|hit| match hit {
            UnbindableHit::Unknown { placeholder } => Some(placeholder.as_str()),
            UnbindableHit::EnsembleOutput { .. } => None,
        })
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{owner} carries {} no binding covers ({}). Accepted placeholders: {}. \
         A marker meant as literal text must be escaped as \\{{{{...}}}}.",
        if unknown.len() == 1 {
            "a placeholder"
        } else {
            "placeholders"
        },
        unknown.join(", "),
        accepted_forms_text(supported, node_names),
    ))
}

/// One literal template a node config stores: the config key, its body, and
/// the binding set the spawn path renders it with.
fn node_template_entries(
    kind: GraphNodeKind,
    config: &Value,
) -> Vec<(&'static str, &str, &'static [&'static str])> {
    let mut entries = Vec::new();
    match kind {
        GraphNodeKind::Agent => {
            if let Some(template) = config.get("prompt_template").and_then(Value::as_str) {
                entries.push((
                    "prompt_template",
                    template,
                    crate::graph_engine::AGENT_PROMPT_BINDINGS,
                ));
            }
            if let Some(template) = config.get("resume_prompt").and_then(Value::as_str) {
                entries.push((
                    "resume_prompt",
                    template,
                    crate::graph_engine::RESUME_PROMPT_BINDINGS,
                ));
            }
        }
        GraphNodeKind::Router => {
            if let Some(template) = config.get("prompt_template").and_then(Value::as_str) {
                entries.push((
                    "prompt_template",
                    template,
                    crate::graph_engine::ROUTER_DECISION_BINDINGS,
                ));
            }
        }
        GraphNodeKind::Check | GraphNodeKind::Gate | GraphNodeKind::Join => {}
    }
    entries
}

/// Refuse a node's config when any literal template it stores carries a
/// placeholder nothing can bind. A router decision prompt binds no node
/// outputs at spawn time, so it is checked with empty names and ensembles —
/// any `{{output:...}}` there is refused, exactly as the spawn path refuses
/// it.
pub(crate) fn check_node_templates(
    kind: GraphNodeKind,
    config: &Value,
    owner_name: &str,
    node_names: &[String],
    ensembles: &[(String, Option<String>)],
) -> Result<(), String> {
    let empty_names: &[String] = &[];
    let empty_ensembles: &[(String, Option<String>)] = &[];
    for (field, template, supported) in node_template_entries(kind, config) {
        let (names, ensemble_list) = match kind {
            GraphNodeKind::Router => (empty_names, empty_ensembles),
            _ => (node_names, ensembles),
        };
        let owner = if field == "prompt_template" {
            format!("Node '{owner_name}' prompt template")
        } else {
            format!("Node '{owner_name}' {field}")
        };
        check_template_bindings(&owner, template, supported, names, ensemble_list)?;
    }
    Ok(())
}

/// Write-time binding check for `graph_node`: the new node's templates
/// against its target scope, with its own name already bindable — at spawn
/// the node is part of the graph's node names, so a self-reference renders
/// "(not yet executed)" instead of refusing.
pub(crate) fn check_new_node(
    db: &Database,
    target: &GraphTarget,
    kind: GraphNodeKind,
    config: &Value,
    name: &str,
) -> Result<(), String> {
    let (spec_id, graph_id) = match target {
        GraphTarget::Spec(spec_id) => (Some(spec_id.as_str()), None),
        GraphTarget::Graph(graph_id) => (None, Some(graph_id.as_str())),
    };
    let scope = TemplateScope::load(db, spec_id, graph_id)?;
    let mut node_names = scope.nodes;
    node_names.push(name.to_string());
    check_node_templates(kind, config, name, &node_names, &scope.ensembles)
}

/// Write-time binding check for `graph_node`: the merged config with
/// the effective kind/name, against the node's own scope.
pub(crate) fn check_updated_node(
    db: &Database,
    node: &GraphNode,
    kind: Option<GraphNodeKind>,
    name: Option<&str>,
    config: &Value,
) -> Result<(), String> {
    let scope = TemplateScope::load(db, node.spec_id.as_deref(), node.graph_id.as_deref())?;
    let effective_name = name.unwrap_or(&node.name);
    let mut node_names = scope.nodes;
    if !node_names.iter().any(|n| n == effective_name) {
        node_names.push(effective_name.to_string());
    }
    check_node_templates(
        kind.unwrap_or(node.kind),
        config,
        effective_name,
        &node_names,
        &scope.ensembles,
    )
}

fn template_hit_warning(
    kind: &'static str,
    id: &str,
    name: &str,
    field_label: &str,
    hit: &UnbindableHit,
    member_index: Option<usize>,
) -> Value {
    let (placeholder, detail) = match hit {
        UnbindableHit::EnsembleOutput { target, quorum } => (
            format!("output:{target}"),
            match quorum {
                Some(quorum) => format!(
                    "outputs are read from nodes, not ensembles — did you mean its quorum node '{quorum}'?"
                ),
                None => "outputs are read from nodes, not ensembles.".to_string(),
            },
        ),
        UnbindableHit::Unknown { placeholder } => (placeholder.clone(), String::new()),
    };
    let owner = if kind == "node" {
        format!("Node '{name}' {field_label}")
    } else {
        match member_index {
            Some(i) => format!("Ensemble '{name}' member {} {field_label}", i + 1),
            None => format!("Ensemble '{name}' {field_label}"),
        }
    };
    let mut warning = format!(
        "{owner} carries a placeholder nothing can bind ({{{{{placeholder}}}}}). \
         This template was stored before write-time template validation; fix it so every \
         {{{{...}}}} marker is one the engine substitutes (or escape literal markers as \
         \\{{{{...}}}}), otherwise the node refuses to spawn at run time."
    );
    if !detail.is_empty() {
        warning.push(' ');
        warning.push_str(&detail);
    }
    let mut value = serde_json::json!({
        "kind": kind,
        "id": id,
        "name": name,
        "field": field_label,
        "placeholder": placeholder,
        "warning": warning,
    });
    if let Some(i) = member_index {
        value["member_index"] = serde_json::json!(i);
    }
    value
}

/// Report every template stored before write-time validation that carries a
/// placeholder nothing can bind (FR3): each node key [`check_node_templates`]
/// covers plus every ensemble shared prompt and member override, across the
/// graph's own scope and every spec's scope. Ensemble member and quorum nodes
/// are skipped — their prompts are ensemble-managed copies, covered by the
/// shared/override entries instead. One warning object per offending
/// placeholder, naming node and placeholder.
pub(crate) fn unbindable_template_warnings(
    db: &Database,
    details: &GraphDetails,
) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let graph_ensembles = db
        .list_ensembles_for_graph(&details.lp.id)
        .map_err(|e| e.to_string())?;
    out.extend(scope_template_warnings(
        &details.graph_nodes,
        &graph_ensembles,
    ));
    for spec in &details.specs {
        let spec_ensembles = db
            .list_ensembles_for_spec(&spec.spec.id)
            .map_err(|e| e.to_string())?;
        out.extend(scope_template_warnings(&spec.nodes, &spec_ensembles));
    }
    Ok(out)
}

fn scope_template_warnings(nodes: &[GraphNode], ensembles: &[EnsembleDetails]) -> Vec<Value> {
    let scope = TemplateScope::from_parts(nodes, ensembles);
    let mut owned_ids: Vec<&str> = ensembles
        .iter()
        .flat_map(|d| d.members.iter().map(|m| m.node_id.as_str()))
        .collect();
    owned_ids.extend(ensembles.iter().map(|d| d.ensemble.join_node_id.as_str()));

    let mut out = Vec::new();
    for node in nodes {
        if owned_ids.contains(&node.id.as_str()) {
            continue;
        }
        let empty_names: &[String] = &[];
        let empty_ensembles: &[(String, Option<String>)] = &[];
        for (field, template, supported) in node_template_entries(node.kind, &node.config) {
            let (names, ensemble_list) = match node.kind {
                GraphNodeKind::Router => (empty_names, empty_ensembles),
                _ => (scope.nodes.as_slice(), scope.ensembles.as_slice()),
            };
            for hit in unbindable_hits(template, supported, names, ensemble_list) {
                out.push(template_hit_warning(
                    "node", &node.id, &node.name, field, &hit, None,
                ));
            }
        }
    }
    for d in ensembles {
        for hit in unbindable_hits(
            &d.ensemble.prompt_template,
            crate::graph_engine::AGENT_PROMPT_BINDINGS,
            &scope.nodes,
            &scope.ensembles,
        ) {
            out.push(template_hit_warning(
                "ensemble",
                &d.ensemble.id,
                &d.ensemble.name,
                "shared prompt template",
                &hit,
                None,
            ));
        }
        for (i, member) in d.members.iter().enumerate() {
            if let Some(template) = member.prompt_override.as_deref() {
                for hit in unbindable_hits(
                    template,
                    crate::graph_engine::AGENT_PROMPT_BINDINGS,
                    &scope.nodes,
                    &scope.ensembles,
                ) {
                    out.push(template_hit_warning(
                        "ensemble",
                        &d.ensemble.id,
                        &d.ensemble.name,
                        "prompt override",
                        &hit,
                        Some(i),
                    ));
                }
            }
        }
    }
    out
}
