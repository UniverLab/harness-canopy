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
//!
//! Import restores each ensemble's `kind` and every other field the document
//! carries, including every entry source rather than only the ensemble row's
//! primary; a document field it does not know how to restore — including an
//! unknown `kind` value — is a refusal naming the field, never a silent
//! default (CB62, CB71).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::graphs::{
    member_bounds, Ensemble, EnsembleDetails, EnsembleKind, EnsembleMember, Graph,
    GraphCompletionHook, GraphEdge, GraphEdgeCondition, GraphHookEvent, GraphNode, GraphNodeKind,
};
use crate::domain::validation::{
    validate_ensemble_min_pass, validate_ensembles_in_graph, validate_graph, GraphEdgeView,
    GraphNodeView,
};

mod export;
pub use export::*;
mod import;
pub use import::*;
mod assemble;
pub use assemble::*;
mod pending;
mod presets;
pub use presets::*;
mod validation;
pub use validation::*;

/// The latest `format_version` this build writes. `graph_import` accepts `1`
/// (members with no binding), `2` (bindings included), `3` (graph/error
/// vocabulary, `break` still read as `error` on import), `4` (every ensemble
/// entry source), `5`, and `6` (current: ensemble `mutex` plus the bundled
/// `prompt_presets` every custom preset the document's agent nodes
/// reference travels with). An unrecognized or missing version is a
/// refusal, never a best-effort parse (decision 6).
pub const GRAPH_EXPORT_FORMAT_VERSION: i64 = 6;

// Ensemble member count bounds live in `member_bounds` (the single shared
// definition both this domain layer and the daemon's authoring-time
// validators call), so the two layers can never drift.

/// A graph's design, portable across machines/installations. Key order is
/// deliberate (struct field order) — see `docs/graphs.md` for the documented,
/// hand-writable contract this type is the source of truth for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// CM49: the graph's completion hooks, restored verbatim on import so a
    /// `crew`-bound agent hook round-trips. Omitted when the graph has none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hooks: BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
    /// The full text of every custom `prompt_preset` the document's agent
    /// nodes reference (v6), so installing the document on another machine
    /// produces working nodes instead of name-only references. Builtin
    /// presets (`implementer`, `reviewer`, `resilience`) are never bundled;
    /// import installs each entry into the prompts dir when no file with
    /// that name exists locally (see [`presets`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_presets: Vec<GraphExportPromptPreset>,
}

/// One bundled custom prompt preset: the name an agent node's
/// `prompt_preset` config key references (resolved as
/// `<prompts_dir>/<name>.md` at spawn time) and its full content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphExportPromptPreset {
    pub name: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphExportNode {
    pub name: String,
    pub kind: GraphNodeKind,
    pub position: i64,
    pub config: Value,
}

/// References nodes by `name`, never by id (decision 2) — what makes the
/// file reviewable, hand-editable, and diffable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphExportEdge {
    pub from_node: String,
    pub to_node: String,
    pub condition: GraphEdgeCondition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// One extra entry source of an ensemble — a plain node that enters *every*
/// member on its own condition, added at authoring time with
/// `graph_update_ensemble`'s `add_entry_from`. The primary entry stays in the
/// ensemble's `entry_from_node`/`entry_condition` fields (CB71).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphExportEntrySource {
    pub from_node: String,
    pub condition: GraphEdgeCondition,
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
#[serde(deny_unknown_fields)]
pub struct GraphExportEnsemble {
    pub name: String,
    /// Ensemble kind (`"parallel"` | `"cascade"` | `"round_robin"`); export
    /// omits it for `parallel` (the default). Import restores it exactly and
    /// refuses an unrecognized string (CB62).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// CM28: whether this ensemble holds commit rights (B37). Always
    /// written (unlike `kind`, which hides its default) so an export makes
    /// the graph's committer visible without cross-referencing nodes.
    #[serde(default)]
    pub commit_rights: bool,
    pub prompt_template: String,
    pub entry_from_node: GraphExportEnsembleTarget,
    pub entry_condition: GraphEdgeCondition,
    /// Every entry source beyond the primary (`entry_from_node` +
    /// `entry_condition`). Export omits it when the ensemble has only its
    /// primary; import re-creates one edge per source per member, exactly as
    /// `graph_update_ensemble`'s `add_entry_from` wires it (CB71).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_entry_sources: Vec<GraphExportEntrySource>,
    pub on_pass_to: GraphExportEnsembleTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail_to: Option<GraphExportEnsembleTarget>,
    pub min_pass: i64,
    pub timeout_minutes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub straggler_timeout_minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quorum_grace_minutes: Option<i64>,
    /// CM49: the crew this ensemble is bound to, by name. When set, `members`
    /// is empty — the members live in the crew, shared across graphs. Import
    /// keeps the reference when a crew of this name exists locally, and
    /// otherwise leaves the ensemble pending (`missing_crews`) until one is
    /// created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
    /// A crew-bound ensemble travels without members (the crew supplies them)
    /// and an author writing one by hand may omit the key entirely, so an
    /// absent `members` means "no literal members" rather than a parse error.
    /// An ensemble with neither `crew` nor `members` is still refused, naming
    /// the ensemble.
    #[serde(default)]
    pub members: Vec<GraphExportEnsembleMember>,
    /// Named mutexes every member run holds. Export omits it when the
    /// ensemble has none; import of an older document (or one that omits the
    /// key) treats it as absent. Every name must match
    /// `^[a-z][a-z0-9-]{0,31}$`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mutex: Vec<String>,
}

#[cfg(test)]
mod tests;
