//! Validate a shared graph or node document without importing it.
//!
//! The single entry point [`validate_request`] serves both `canopy graph
//! validate` and the `graph` MCP tool: exactly one of a file path
//! or an inline document, no database access, no workdir requirement. A
//! graph document runs every structural check `graph_edit` runs — through
//! the same functions — plus the sharing rule for `prompt_preset` (bundled
//! in the document or a builtin); a node document runs the slug, kind,
//! config, and harness rules from the spec. Success reports the kind, name,
//! counts, and the agent nodes that will need a harness; failure reports
//! every problem found, each with a JSON path.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::daemon::handler_nodes::validate_known_config_keys;
use crate::daemon::handler_templates::{check_node_templates, check_template_bindings};
use crate::daemon::handler_validate::validate_node_mutex_config;
use crate::domain::graph_transfer::{
    agent_nodes_missing_platform, assemble_import_plan, node_config_has_harness,
    parse_export_document_value, validate_entry as validate_preset_entry, GraphExportDocument,
    GraphImportPlan, GRAPH_EXPORT_FORMAT_VERSION,
};
use crate::domain::graphs::GraphNodeKind;

/// One validation failure: where in the document, and why.
#[derive(Debug, Clone, Serialize)]
struct Problem {
    path: String,
    message: String,
}

fn problem(path: impl Into<String>, message: impl Into<String>) -> Problem {
    Problem {
        path: path.into(),
        message: message.into(),
    }
}

fn fail(problems: &[Problem]) -> Value {
    serde_json::json!({ "ok": false, "problems": problems })
}

/// Validate one shared document: exactly one of `path` (read from disk) or
/// an inline `document`. Returns the report value — `{"ok": true, ...}` or
/// `{"ok": false, "problems": [...]}`. `Err` is reserved for a malformed
/// request (zero or two inputs) and for a file that cannot be read; a file
/// whose *contents* are invalid is an `ok: false` report, not an error.
pub(crate) fn validate_request(
    path: Option<String>,
    document: Option<Value>,
) -> Result<Value, String> {
    match (path, document) {
        (Some(path), None) => {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| format!("could not read '{path}': {e}"))?;
            Ok(validate_document_str(&raw))
        }
        (None, Some(document)) => Ok(validate_document_value(&document)),
        _ => Err("Provide exactly one of `path` or `document`.".to_string()),
    }
}

/// Validate raw JSON text (a file's contents).
pub(crate) fn validate_document_str(raw: &str) -> Value {
    let value: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(e) => return fail(&[problem("document", format!("Not valid JSON: {e}."))]),
    };
    validate_document_value(&value)
}

/// Validate a parsed document, detecting its kind: an object with `nodes`
/// is a graph document, one with `kind` plus `config` is a node document.
pub(crate) fn validate_document_value(value: &Value) -> Value {
    let Some(map) = value.as_object() else {
        return fail(&[problem("document", not_either_message())]);
    };
    if map.contains_key("nodes") {
        validate_graph_document(value)
    } else if map.contains_key("kind") && map.contains_key("config") {
        validate_node_document(value)
    } else {
        fail(&[problem("document", not_either_message())])
    }
}

fn not_either_message() -> String {
    "Document is neither a graph document (expected a 'nodes' array) nor a node document (expected 'kind' and 'config').".to_string()
}

// ── Graph documents ───────────────────────────────────────────────────

/// Run every check `graph_edit` runs — through the same functions — plus
/// the sharing rule for `prompt_preset`, collecting every problem found.
fn validate_graph_document(value: &Value) -> Value {
    if let Some(failure) = reject_bad_graph_format_version(value) {
        return failure;
    }
    let document = match parse_export_document_value(value) {
        Ok(document) => document,
        Err(message) => return fail(&[problem("document", message)]),
    };

    let mut problems = Vec::new();
    validate_graph_node_configs(&document, &mut problems);
    validate_bundled_preset_entries(&document, &mut problems);
    validate_shared_presets(&document, &mut problems);

    let plan = match assemble_import_plan(&document, "validate", None) {
        Ok(plan) => plan,
        Err(message) => {
            problems.push(problem("document", message));
            return fail(&problems);
        }
    };
    validate_graph_templates(&document, &plan, &mut problems);

    if problems.is_empty() {
        serde_json::json!({
            "ok": true,
            "kind": "graph",
            "name": document.name,
            "nodes": document.nodes.len(),
            "edges": document.edges.len(),
            "ensembles": document.ensembles.len(),
            "needs_harness": agent_nodes_missing_platform(&plan),
        })
    } else {
        fail(&problems)
    }
}

/// The version pre-check `parse_export_document_value` would refuse anyway,
/// reported at its own path instead of `document`.
fn reject_bad_graph_format_version(value: &Value) -> Option<Value> {
    match value.get("format_version") {
        None => Some(fail(&[problem(
            "format_version",
            format!(
                "Graph export document is missing format_version; refusing to guess. Expected format_version: {GRAPH_EXPORT_FORMAT_VERSION}."
            ),
        )])),
        Some(raw) => match raw.as_i64() {
            Some(version) if (1..=GRAPH_EXPORT_FORMAT_VERSION).contains(&version) => None,
            Some(other) => Some(fail(&[problem(
                "format_version",
                format!(
                    "Graph export document has format_version {other}, but this build only supports 1 through {GRAPH_EXPORT_FORMAT_VERSION}."
                ),
            )])),
            None => Some(fail(&[problem(
                "format_version",
                format!(
                    "Graph export document has a format_version that is not an integer; refusing to guess. Expected format_version: {GRAPH_EXPORT_FORMAT_VERSION}."
                ),
            )])),
        },
    }
}

/// The same per-node config checks `graph_edit` runs before building its
/// plan: known keys for the kind, plus the `mutex` shape.
fn validate_graph_node_configs(document: &GraphExportDocument, problems: &mut Vec<Problem>) {
    for (index, node) in document.nodes.iter().enumerate() {
        let path = format!("nodes[{index}].config");
        if let Some(map) = node.config.as_object() {
            if let Err(message) = validate_known_config_keys(node.kind, map) {
                problems.push(problem(path.clone(), message));
            }
        }
        if let Err(message) = validate_node_mutex_config(&node.name, &node.config) {
            problems.push(problem(path, message));
        }
    }
}

/// The machine-independent half of import's bundled-preset check
/// (`graph_transfer::validate_entry`, the same function): every bundled entry
/// must be installable on any machine — a plain non-blank name inside the
/// prompts dir, not a builtin, not repeated, non-empty content. The other
/// half of import's check — refusing to overwrite a differing local file —
/// needs the installer's prompts dir and belongs to import alone; without
/// this half a document bundling, say, the builtin `reviewer` would validate
/// here and then be refused by `graph_edit` on every machine.
fn validate_bundled_preset_entries(document: &GraphExportDocument, problems: &mut Vec<Problem>) {
    let mut seen = BTreeSet::new();
    for (index, preset) in document.prompt_presets.iter().enumerate() {
        if let Err(message) = validate_preset_entry(preset, &mut seen) {
            problems.push(problem(format!("prompt_presets[{index}]"), message));
        }
    }
}

/// Sharing rule for `prompt_preset`: validation must not depend on what this
/// machine happens to have installed, so a referenced preset must travel
/// with the document (`prompt_presets`, v6) or be a builtin — naming the
/// node and the preset otherwise. Blank names take the fallback template
/// exactly as the spawn path treats them, so they are skipped.
fn validate_shared_presets(document: &GraphExportDocument, problems: &mut Vec<Problem>) {
    let bundled: BTreeSet<&str> = document
        .prompt_presets
        .iter()
        .map(|preset| preset.name.as_str())
        .collect();
    let builtin_names: Vec<&'static str> = crate::domain::prompts::builtin_prompt_preset_specs()
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let builtins: BTreeSet<&str> = builtin_names.iter().copied().collect();
    for (index, node) in document.nodes.iter().enumerate() {
        if node.kind != GraphNodeKind::Agent {
            continue;
        }
        let Some(name) = node
            .config
            .get("prompt_preset")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
        else {
            continue;
        };
        if builtins.contains(name) || bundled.contains(name) {
            continue;
        }
        problems.push(problem(
            format!("nodes[{index}].config.prompt_preset"),
            format!(
                "Node '{}' references prompt preset '{name}', which is neither bundled in this document nor a builtin ({}).",
                node.name,
                builtin_names.join(", ")
            ),
        ));
    }
}

/// The write-time template binding check from `graph_edit`'s handler (every
/// document node, ensemble shared prompt, and member override) — the same
/// functions, with the expanded plan's node names as the binding scope.
fn validate_graph_templates(
    document: &GraphExportDocument,
    plan: &GraphImportPlan,
    problems: &mut Vec<Problem>,
) {
    let mut node_names: Vec<String> = document.nodes.iter().map(|n| n.name.clone()).collect();
    for ensemble_plan in &plan.ensembles {
        node_names.extend(ensemble_plan.member_nodes.iter().map(|n| n.name.clone()));
        node_names.push(ensemble_plan.join_node.name.clone());
    }
    let ensembles: Vec<(String, Option<String>)> = document
        .ensembles
        .iter()
        .zip(plan.ensembles.iter())
        .map(|(doc, ensemble_plan)| (doc.name.clone(), Some(ensemble_plan.join_node.name.clone())))
        .collect();

    for (index, doc_node) in document.nodes.iter().enumerate() {
        if let Err(message) = check_node_templates(
            doc_node.kind,
            &doc_node.config,
            &doc_node.name,
            &node_names,
            &ensembles,
        ) {
            problems.push(problem(format!("nodes[{index}].config"), message));
        }
    }
    for (index, doc_ensemble) in document.ensembles.iter().enumerate() {
        let owner = format!("Ensemble '{}' shared prompt template", doc_ensemble.name);
        if let Err(message) = check_template_bindings(
            &owner,
            &doc_ensemble.prompt_template,
            crate::graph_engine::AGENT_PROMPT_BINDINGS,
            &node_names,
            &ensembles,
        ) {
            problems.push(problem(
                format!("ensembles[{index}].prompt_template"),
                message,
            ));
        }
        validate_member_overrides(doc_ensemble, index, &node_names, &ensembles, problems);
    }
}

fn validate_member_overrides(
    doc_ensemble: &crate::domain::graph_transfer::GraphExportEnsemble,
    ensemble_index: usize,
    node_names: &[String],
    ensembles: &[(String, Option<String>)],
    problems: &mut Vec<Problem>,
) {
    for (member_index, member) in doc_ensemble.members.iter().enumerate() {
        let Some(template) = member.prompt_override.as_deref() else {
            continue;
        };
        let owner = format!(
            "Ensemble '{}' member {} prompt override",
            doc_ensemble.name,
            member_index + 1
        );
        if let Err(message) = check_template_bindings(
            &owner,
            template,
            crate::graph_engine::AGENT_PROMPT_BINDINGS,
            node_names,
            ensembles,
        ) {
            problems.push(problem(
                format!("ensembles[{ensemble_index}].members[{member_index}].prompt_override"),
                message,
            ));
        }
    }
}

// ── Node documents ────────────────────────────────────────────────────

/// A single shareable node: `format_version`, `name`, `kind`, `config`, an
/// optional `description`, and an optional bundled `preset`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodeDocument {
    format_version: i64,
    name: String,
    kind: String,
    config: Value,
    description: Option<String>,
    preset: Option<NodePreset>,
}

/// What a validated node document describes: a single engine node of a
/// known kind, or an ensemble node document (whose config carries the
/// ensemble shape instead of one node's config).
#[derive(Debug, Clone, Copy)]
enum NodeShape {
    Node(GraphNodeKind),
    Ensemble,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NodePreset {
    name: String,
    content: String,
}

/// Node-document `format_version`: the only version defined so far.
const NODE_DOCUMENT_FORMAT_VERSION: i64 = 1;

/// Config keys that must never travel in a shared node document — harnesses
/// are chosen by the installer, not the author.
const FORBIDDEN_HARNESS_KEYS: [&str; 3] = ["platform", "model", "cli"];

fn validate_node_document(value: &Value) -> Value {
    let document: NodeDocument = match serde_json::from_value(value.clone()) {
        Ok(document) => document,
        Err(e) => {
            return fail(&[problem("document", format!("Invalid node document: {e}."))]);
        }
    };
    // `description` is documentation-only: accepted by the shape, ignored by
    // the rules.
    let _ = document.description.as_deref();

    let mut problems = Vec::new();
    if document.format_version != NODE_DOCUMENT_FORMAT_VERSION {
        problems.push(problem(
            "format_version",
            format!(
                "Node document has format_version {}, but node documents only support format_version {NODE_DOCUMENT_FORMAT_VERSION}.",
                document.format_version
            ),
        ));
    }
    if !is_slug(&document.name) {
        problems.push(problem(
            "name",
            format!(
                "Node document name '{}' is not a valid slug: use 1-48 lowercase letters, digits, or '-'.",
                document.name
            ),
        ));
    }
    let kind = if document.kind.trim() == "ensemble" {
        validate_ensemble_node_config(&document, &mut problems);
        Some(NodeShape::Ensemble)
    } else {
        validate_node_kind(&document.kind, &mut problems).map(NodeShape::Node)
    };
    if let Some(NodeShape::Node(kind)) = kind {
        validate_node_config(kind, &document.config, &mut problems);
    }
    validate_node_preset(&document, &mut problems);

    let Some(kind) = kind else {
        // An unparsed kind always pushes a problem above, so this is a
        // failure report, never an empty one.
        return fail(&problems);
    };
    if problems.is_empty() {
        let node_kind = match kind {
            NodeShape::Node(kind) => kind.as_str(),
            NodeShape::Ensemble => "ensemble",
        };
        let needs_harness: Vec<String> = match kind {
            NodeShape::Node(GraphNodeKind::Agent) if !node_config_has_harness(&document.config) => {
                vec![document.name.clone()]
            }
            _ => Vec::new(),
        };
        serde_json::json!({
            "ok": true,
            "kind": "node",
            "name": document.name,
            "node_kind": node_kind,
            "nodes": 1,
            "edges": 0,
            "ensembles": 0,
            "needs_harness": needs_harness,
        })
    } else {
        fail(&problems)
    }
}

/// A shareable name: `[a-z0-9-]{1,48}`, checked by hand — no regex needed
/// for one character class.
fn is_slug(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn validate_node_kind(raw: &str, problems: &mut Vec<Problem>) -> Option<GraphNodeKind> {
    match GraphNodeKind::from_str(raw) {
        None => {
            problems.push(problem(
                "kind",
                format!(
                    "Node document has unknown kind '{raw}'. Expected one of: agent, check, gate, router, ensemble."
                ),
            ));
            None
        }
        Some(GraphNodeKind::Join) => {
            problems.push(problem(
                "kind",
                "Node document has kind 'join', which is engine-managed and can only be created via an ensemble — it can never be authored directly.",
            ));
            None
        }
        Some(kind) => Some(kind),
    }
}

/// An ensemble node document's `config` must be the ensemble shape — no
/// harness anywhere (members travel without platform/model), a non-empty
/// shared prompt, and member/min_pass bounds the engine will accept.
fn validate_ensemble_node_config(document: &NodeDocument, problems: &mut Vec<Problem>) {
    if !document.config.is_object() {
        problems.push(problem(
            "config",
            "Node document config must be a JSON object.",
        ));
        return;
    }
    collect_harness_key_problems(&document.config, "config", problems);
    let parsed =
        crate::domain::nodes::EnsembleNodeConfig::from_value(&document.name, &document.config)
            .and_then(|config| config.validate(&document.name));
    if let Err(message) = parsed {
        problems.push(problem("config", message));
    }
}

/// The config must be an object with keys known for the kind, and must not
/// carry harness bindings anywhere inside it — not even nested.
fn validate_node_config(kind: GraphNodeKind, config: &Value, problems: &mut Vec<Problem>) {
    let Some(map) = config.as_object() else {
        problems.push(problem(
            "config",
            "Node document config must be a JSON object.".to_string(),
        ));
        return;
    };
    if let Err(message) = validate_known_config_keys(kind, map) {
        problems.push(problem("config", message));
    }
    collect_harness_key_problems(config, "config", problems);
}

fn collect_harness_key_problems(value: &Value, base: &str, problems: &mut Vec<Problem>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = format!("{base}.{key}");
                if FORBIDDEN_HARNESS_KEYS.contains(&key.as_str()) {
                    problems.push(problem(
                        path.clone(),
                        format!(
                            "Shared node documents must not set '{key}': harnesses are chosen by the installer."
                        ),
                    ));
                }
                collect_harness_key_problems(child, &path, problems);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_harness_key_problems(item, &format!("{base}[{index}]"), problems);
            }
        }
        _ => {}
    }
}

/// When both travel together, the bundled preset's name must equal the
/// config's `prompt_preset` — otherwise the bundle belongs to another node.
fn validate_node_preset(document: &NodeDocument, problems: &mut Vec<Problem>) {
    let (Some(preset), Some(referenced)) = (
        document.preset.as_ref(),
        document.config.get("prompt_preset").and_then(Value::as_str),
    ) else {
        return;
    };
    // Only the name is validated; the content travels opaquely.
    let _ = preset.content.as_str();
    if preset.name != referenced {
        problems.push(problem(
            "preset.name",
            format!(
                "Node document preset name '{}' does not match the config's prompt_preset '{referenced}'.",
                preset.name
            ),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_graph() -> Value {
        json!({
            "format_version": 6,
            "name": "shared",
            "nodes": [
                {"name": "start", "kind": "agent", "position": 1,
                 "config": {"platform": "claude", "prompt_template": "do it"}},
                {"name": "finish", "kind": "check", "position": 2,
                 "config": {"command": "true"}},
            ],
            "edges": [
                {"from_node": "start", "to_node": "finish", "condition": "always"},
            ],
            "ensembles": [],
        })
    }

    fn valid_node() -> Value {
        json!({
            "format_version": 1,
            "name": "auditor",
            "kind": "agent",
            "config": {"prompt_template": "audit this", "timeout_minutes": 10},
            "description": "A shareable audit node.",
        })
    }

    fn first_path(report: &Value) -> &str {
        report["problems"][0]["path"]
            .as_str()
            .expect("problem path")
    }

    #[test]
    fn valid_graph_reports_counts_and_no_harness_gap() {
        let report = validate_document_value(&valid_graph());
        assert_eq!(report["ok"], true);
        assert_eq!(report["kind"], "graph");
        assert_eq!(report["name"], "shared");
        assert_eq!(report["nodes"], 2);
        assert_eq!(report["edges"], 1);
        assert_eq!(report["ensembles"], 0);
        assert_eq!(report["needs_harness"], json!([]));
    }

    #[test]
    fn valid_graph_lists_agent_nodes_missing_a_harness() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]
            .as_object_mut()
            .expect("config object")
            .remove("platform");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], true);
        assert_eq!(report["needs_harness"], json!(["start"]));
    }

    #[test]
    fn valid_node_reports_kind_counts_and_harness_gap() {
        let report = validate_document_value(&valid_node());
        assert_eq!(report["ok"], true);
        assert_eq!(report["kind"], "node");
        assert_eq!(report["name"], "auditor");
        assert_eq!(report["node_kind"], "agent");
        assert_eq!(report["nodes"], 1);
        assert_eq!(report["edges"], 0);
        assert_eq!(report["ensembles"], 0);
        assert_eq!(report["needs_harness"], json!(["auditor"]));
    }

    #[test]
    fn validate_action_declares_path_and_document() {
        let spec = crate::daemon::handler_graph_tools::graph_action("validate")
            .expect("graph has a validate action");
        let declared: Vec<&str> = spec
            .required
            .iter()
            .chain(spec.optional.iter())
            .copied()
            .collect();
        assert!(declared.contains(&"path"), "{declared:?}");
        assert!(declared.contains(&"document"), "{declared:?}");
    }

    #[test]
    fn request_with_neither_input_is_an_error() {
        let err = validate_request(None, None).expect_err("must refuse");
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn request_with_both_inputs_is_an_error() {
        let err =
            validate_request(Some("f.json".to_string()), Some(json!({}))).expect_err("must refuse");
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn request_with_missing_file_is_an_error() {
        let err = validate_request(Some("/nonexistent/validate-doc.json".to_string()), None)
            .expect_err("must refuse");
        assert!(err.contains("could not read"), "got: {err}");
    }

    #[test]
    fn invalid_json_is_a_document_problem_not_a_request_error() {
        let report = validate_document_str("{not json");
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
    }

    #[test]
    fn document_of_neither_kind_reports_document() {
        let report = validate_document_value(&json!({"hello": "world"}));
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
    }

    #[test]
    fn graph_with_bad_format_version_reports_format_version() {
        let mut doc = valid_graph();
        doc["format_version"] = json!(99);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "format_version");
    }

    #[test]
    fn graph_with_missing_format_version_reports_format_version() {
        let mut doc = valid_graph();
        doc.as_object_mut()
            .expect("object")
            .remove("format_version");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "format_version");
    }

    #[test]
    fn graph_with_unknown_field_reports_document() {
        let mut doc = valid_graph();
        doc["mystery"] = json!(true);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
    }

    #[test]
    fn graph_with_unknown_config_key_reports_indexed_config() {
        let mut doc = valid_graph();
        doc["nodes"][1]["config"]["bogus"] = json!(1);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "nodes[1].config");
    }

    #[test]
    fn graph_with_bad_mutex_reports_indexed_config() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["mutex"] = json!("cpu");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "nodes[0].config");
    }

    #[test]
    fn graph_with_unbundled_preset_reports_prompt_preset_path() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["prompt_preset"] = json!("my-auditor");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "nodes[0].config.prompt_preset");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("start"), "names the node: {message}");
        assert!(
            message.contains("my-auditor"),
            "names the preset: {message}"
        );
    }

    #[test]
    fn graph_with_bundled_preset_passes() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["prompt_preset"] = json!("my-auditor");
        doc["prompt_presets"] = json!([{"name": "my-auditor", "content": "audit well"}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], true);
    }

    #[test]
    fn graph_with_builtin_preset_passes() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["prompt_preset"] = json!("reviewer");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], true);
    }

    #[test]
    fn graph_with_non_integer_format_version_reports_format_version() {
        let mut doc = valid_graph();
        doc["format_version"] = json!("6");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "format_version");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("not an integer"), "got: {message}");
    }

    #[test]
    fn graph_bundling_a_builtin_preset_reports_preset_path() {
        let mut doc = valid_graph();
        doc["prompt_presets"] = json!([{"name": "reviewer", "content": "audit well"}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "prompt_presets[0]");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("reviewer"), "names the preset: {message}");
        assert!(message.contains("builtin"), "names the rule: {message}");
    }

    #[test]
    fn graph_bundling_an_empty_preset_reports_preset_path() {
        let mut doc = valid_graph();
        doc["prompt_presets"] = json!([{"name": "my-auditor", "content": ""}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "prompt_presets[0]");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(
            message.contains("my-auditor"),
            "names the preset: {message}"
        );
    }

    #[test]
    fn graph_bundling_an_unsafe_preset_name_reports_preset_path() {
        let mut doc = valid_graph();
        doc["prompt_presets"] = json!([{"name": "../escape", "content": "outside"}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "prompt_presets[0]");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("../escape"), "names the preset: {message}");
    }

    #[test]
    fn graph_bundling_a_duplicate_preset_reports_the_second_entry() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["prompt_preset"] = json!("my-auditor");
        doc["prompt_presets"] = json!([
            {"name": "my-auditor", "content": "one"},
            {"name": "my-auditor", "content": "two"},
        ]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "prompt_presets[1]");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("more than once"), "got: {message}");
    }

    #[test]
    fn graph_with_duplicate_names_reports_document() {
        let mut doc = valid_graph();
        doc["nodes"][1]["name"] = json!("start");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
    }

    #[test]
    fn graph_with_unbindable_node_template_reports_indexed_config() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["prompt_template"] = json!("do {{bogus}} now");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "nodes[0].config");
    }

    #[test]
    fn graph_with_dangling_edge_reports_document() {
        let mut doc = valid_graph();
        doc["edges"][0]["to_node"] = json!("ghost");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(
            message.contains("ghost"),
            "names the missing node: {message}"
        );
    }

    #[test]
    fn graph_with_two_entry_points_reports_document() {
        let mut doc = valid_graph();
        doc["edges"] = json!([]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(
            message.contains("entry point"),
            "names the structure fault: {message}"
        );
    }

    #[test]
    fn graph_with_unknown_output_reference_reports_document() {
        let mut doc = valid_graph();
        doc["nodes"][0]["config"]["prompt_template"] = json!("use {{output:ghost}} now");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("start"), "names the node: {message}");
        assert!(
            message.contains("ghost"),
            "names the unknown target: {message}"
        );
    }

    fn graph_with_ensemble() -> Value {
        json!({
            "format_version": 6,
            "name": "with-ensemble",
            "nodes": [
                {"name": "kickoff", "kind": "agent", "position": 1,
                 "config": {"platform": "claude", "prompt_template": "go"}},
                {"name": "downstream", "kind": "check", "position": 2,
                 "config": {"command": "true"}},
            ],
            "edges": [],
            "ensembles": [{
                "name": "reviewers",
                "prompt_template": "review this",
                "entry_from_node": "kickoff",
                "entry_condition": "always",
                "on_pass_to": "downstream",
                "min_pass": 1,
                "timeout_minutes": 20,
                "members": [
                    {"platform": "claude"},
                    {"platform": "opencode"},
                ],
            }],
        })
    }

    #[test]
    fn graph_with_ensemble_reports_ensemble_count() {
        let report = validate_document_value(&graph_with_ensemble());
        assert_eq!(report["ok"], true);
        assert_eq!(report["ensembles"], 1);
    }

    #[test]
    fn graph_with_unbindable_ensemble_template_reports_template_path() {
        let mut doc = graph_with_ensemble();
        doc["ensembles"][0]["prompt_template"] = json!("review {{bogus}}");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "ensembles[0].prompt_template");
    }

    #[test]
    fn graph_with_unbindable_member_override_reports_override_path() {
        let mut doc = graph_with_ensemble();
        doc["ensembles"][0]["members"][1]["prompt_override"] = json!("check {{bogus}}");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(
            first_path(&report),
            "ensembles[0].members[1].prompt_override"
        );
    }

    #[test]
    fn graph_ensemble_with_too_few_members_reports_document() {
        let mut doc = graph_with_ensemble();
        doc["ensembles"][0]["members"] = json!([{"platform": "claude"}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(
            message.contains("2-8 members"),
            "names the shape fault: {message}"
        );
    }

    #[test]
    fn graph_ensemble_with_bad_min_pass_reports_document() {
        let mut doc = graph_with_ensemble();
        doc["ensembles"][0]["min_pass"] = json!(5);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(
            message.contains("min_pass"),
            "names the shape fault: {message}"
        );
    }

    #[test]
    fn node_with_unknown_field_reports_document() {
        let mut doc = valid_node();
        doc["mystery"] = json!(true);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "document");
    }

    #[test]
    fn node_with_bad_format_version_reports_format_version() {
        let mut doc = valid_node();
        doc["format_version"] = json!(2);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "format_version");
    }

    #[test]
    fn node_with_bad_slug_reports_name() {
        let mut doc = valid_node();
        doc["name"] = json!("Not A Slug!");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "name");
    }

    #[test]
    fn node_with_unknown_kind_reports_kind() {
        let mut doc = valid_node();
        doc["kind"] = json!("wizard");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "kind");
    }

    #[test]
    fn node_with_join_kind_reports_kind() {
        let mut doc = valid_node();
        doc["kind"] = json!("join");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "kind");
    }

    #[test]
    fn node_with_non_object_config_reports_config() {
        let mut doc = valid_node();
        doc["config"] = json!("platform: claude");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "config");
    }

    #[test]
    fn node_with_unknown_config_key_reports_config() {
        let mut doc = valid_node();
        doc["config"]["bogus"] = json!(1);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "config");
    }

    #[test]
    fn node_with_top_level_platform_reports_dotted_path() {
        let mut doc = valid_node();
        doc["config"]["platform"] = json!("claude");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "config.platform");
    }

    #[test]
    fn node_with_nested_harness_key_reports_nested_path() {
        let mut doc = valid_node();
        doc["config"]["skills"] = json!([{"model": "x"}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "config.skills[0].model");
    }

    #[test]
    fn node_with_preset_mismatch_reports_preset_name() {
        let mut doc = valid_node();
        doc["config"]["prompt_preset"] = json!("one-preset");
        doc["preset"] = json!({"name": "other-preset", "content": "text"});
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "preset.name");
    }

    #[test]
    fn node_with_matching_preset_passes() {
        let mut doc = valid_node();
        doc["config"]["prompt_preset"] = json!("my-preset");
        doc["preset"] = json!({"name": "my-preset", "content": "text"});
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], true);
    }

    fn valid_ensemble_node() -> Value {
        json!({
            "format_version": 1,
            "name": "ensemble-proposers",
            "kind": "ensemble",
            "description": "Three proposers.",
            "config": {
                "kind": "parallel",
                "prompt_template": "draft it {{spec_content}}",
                "members": [{}, {}, {}],
            },
        })
    }

    #[test]
    fn valid_ensemble_node_reports_node_kind_ensemble() {
        let report = validate_document_value(&valid_ensemble_node());
        assert_eq!(report["ok"], true, "{report}");
        assert_eq!(report["kind"], "node");
        assert_eq!(report["node_kind"], "ensemble");
        assert_eq!(report["needs_harness"], json!([]));
    }

    #[test]
    fn ensemble_node_member_with_a_platform_reports_the_nested_path() {
        let mut doc = valid_ensemble_node();
        doc["config"]["members"][0] = json!({"platform": "claude"});
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "config.members[0].platform");
    }

    #[test]
    fn ensemble_node_outside_the_member_bounds_reports_config() {
        let mut doc = valid_ensemble_node();
        doc["config"]["members"] = json!([{}]);
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        assert_eq!(first_path(&report), "config");
        let message = report["problems"][0]["message"].as_str().expect("message");
        assert!(message.contains("ensemble-proposers"), "{message}");
    }

    #[test]
    fn node_reports_every_problem_not_only_the_first() {
        let mut doc = valid_node();
        doc["name"] = json!("Bad Name");
        doc["kind"] = json!("wizard");
        let report = validate_document_value(&doc);
        assert_eq!(report["ok"], false);
        let paths: Vec<&str> = report["problems"]
            .as_array()
            .expect("problems array")
            .iter()
            .map(|p| p["path"].as_str().expect("path"))
            .collect();
        assert_eq!(paths, vec!["name", "kind"]);
    }
}
