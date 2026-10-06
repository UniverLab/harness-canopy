use super::*;

// ── Sync tool parameter types ──────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncDeclareIntentParams {
    /// Workdir this mission belongs to.
    pub workdir: String,
    /// High-level mission being started.
    pub mission: String,
    /// Impact on the workspace: low | high | breaking.
    pub impact: String,
    /// Optional human-readable details.
    pub description: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncReportStatusParams {
    /// Workdir this status applies to.
    pub workdir: String,
    /// Workspace state: stable | unstable | testing.
    pub status: String,
    /// Optional status details shown to peers.
    pub message: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncBroadcastParams {
    /// Workdir channel to broadcast to.
    pub workdir: String,
    /// Message kind: info | query | answer.
    pub kind: String,
    /// Human-readable message.
    pub message: String,
    /// Optional JSON metadata.
    #[serde(default)]
    #[schemars(schema_with = "arbitrary_json_value_schema")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SyncGetContextParams {
    /// Workdir to query.
    pub workdir: String,
    /// Number of recent messages to return (default: 10).
    pub limit: Option<usize>,
}

// ── Intelligence tool parameter types ─────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceGetContextParams {
    /// Context depth: light or full.
    pub scope: String,
    /// Optional project hash to scope facts/patterns to a specific project.
    pub project_hash: Option<String>,
    /// Traversal depth in project-graph hops (default 1, max 5).
    pub depth: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceRelationParams {
    /// Target node ID for the relation.
    pub to_node_id: String,
    /// Relation label, e.g. "depends_on" or "summarizes".
    pub relation: String,
    /// Optional edge weight.
    pub weight: Option<f64>,
}

/// Schema for a field that accepts any well-formed JSON value. Plain
/// `serde_json::Value` fields otherwise emit an untyped `{}` schema — list
/// every JSON Schema primitive explicitly so a shallow schema reader (and
/// the registered-tool schema regression guard) can see this parameter has
/// a declared type, without narrowing what it actually accepts.
pub(super) fn arbitrary_json_value_schema(
    _generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["object", "array", "string", "number", "boolean", "null"],
    })
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
pub struct BodyReplaceParams {
    /// Literal fragment to find in the node's body. Must occur exactly
    /// once: zero matches or more than one match fails the write.
    pub fragment: String,
    /// Replacement text for the fragment. The rest of the body is untouched.
    pub replacement: String,
}

// `#[schemars(inline)]` makes every use site of this type emit its full
// object schema in place instead of a bare `$ref` into `$defs`. Without it,
// `IntelligenceUpsertParams.node_data` advertises only `{"$ref": "..."}`
// with no sibling `type`, which is indistinguishable from an untyped
// parameter to a client that doesn't resolve `$ref` — that client then
// falls back to sending the object as a JSON-encoded string.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
pub struct IntelligenceNodeParams {
    /// Optional stable node ID. If omitted, a new UUID is generated.
    pub id: Option<String>,
    /// Node kind: fact, pattern, idea, decision, or defect. Structural: project.
    /// Required on create; omit on update to leave the stored kind untouched.
    #[serde(default)]
    pub kind: Option<String>,
    /// Node status: noted, verified, resolved, superseded, or deprecated.
    /// Defaults to 'noted' if omitted on create.
    pub status: Option<String>,
    /// Human-readable title for the node.
    /// Required on create; omit on update to leave the stored title untouched.
    #[serde(default)]
    pub title: Option<String>,
    /// Main body/content of the node.
    /// Required on create (unless body_replace is used on update); omit on
    /// update to leave the stored body untouched. Mutually exclusive with
    /// `body_replace`.
    #[serde(default)]
    pub body: Option<String>,
    /// Literal fragment replacement on the stored body. Update-only: the
    /// fragment must occur exactly once or the write fails, changing
    /// nothing. Mutually exclusive with `body`.
    #[serde(default)]
    pub body_replace: Option<BodyReplaceParams>,
    /// Optional structured metadata. Doubly-optional: omit the field to
    /// leave it untouched, send `null` to clear it, send a value to set it.
    #[serde(default, deserialize_with = "double_option")]
    #[schemars(schema_with = "arbitrary_json_value_schema")]
    pub metadata: Option<Option<serde_json::Value>>,
    /// Optional project hash this node belongs to. Omit to leave untouched
    /// on update (auto-detected from the session workdir on create);
    /// send `null` to clear it.
    #[serde(default, deserialize_with = "double_option")]
    pub project_hash: Option<Option<String>>,
    /// Optional session ID this node belongs to. Omit to leave untouched;
    /// send `null` to clear it.
    #[serde(default, deserialize_with = "double_option")]
    pub session_id: Option<Option<String>>,
    /// Optional outgoing relations to other nodes. Omit to leave the node's
    /// relations alone; send a list (even an empty one) to replace them.
    pub relations: Option<Vec<IntelligenceRelationParams>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceUpsertParams {
    /// Node payload to create or update.
    pub node_data: IntelligenceNodeParams,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceSearchParams {
    /// Free-text search query.
    pub query: String,
    /// Optional kind filter.
    pub kind: Option<String>,
    /// Maximum number of results to return.
    pub limit: Option<usize>,
    /// Optional project hash to scope the search; traverses outbound edges.
    pub project_hash: Option<String>,
    /// Traversal depth in project-graph hops (default 1, max 5).
    pub depth: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceGraphWalkParams {
    /// Starting node ID.
    pub node_id: String,
    /// Maximum traversal depth.
    pub depth: Option<usize>,
    /// Compact mode: omit `body` and `metadata` from returned nodes.
    /// Defaults to true; pass false for full bodies.
    pub compact: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceDeleteNodeParams {
    /// ID of the intelligence node to delete.
    pub node_id: String,
    /// Optional project hash to explicitly scope the deletion, the same way
    /// `intelligence_get_context` allows an explicit override of the
    /// project auto-detected from the session workdir.
    pub project_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceDeleteRelationParams {
    /// ID of the relation (edge) to delete, as returned in the `edges` list
    /// of `intelligence_graph_walk`.
    pub edge_id: i64,
    /// Optional project hash to explicitly scope the deletion, the same way
    /// `intelligence_get_context` allows an explicit override of the
    /// project auto-detected from the session workdir.
    pub project_hash: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GetToolsParams {
    /// Scope of the action. One of: session_start, file_write, test_run, close_session, multi_agent.
    pub scope: String,
    /// Optional file path hint (used with file_write scope to check conflicts).
    pub path: Option<String>,
}

// ── Intelligence V2 tool parameter types ─────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceListProjectsParams {
    /// Optional search query to filter projects by name/body.
    pub query: Option<String>,
    /// Maximum number of results to return (default: 20).
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IntelligenceLinkProjectsParams {
    /// Source project hash.
    pub from_project_hash: String,
    /// Target project hash.
    pub to_project_hash: String,
    /// Relation label (default: "relates_to").
    pub relation: Option<String>,
    /// Optional edge weight (default: 1.0).
    pub weight: Option<f64>,
}
