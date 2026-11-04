use super::*;

// ── Sandbox tool parameter types ──────────────────────────────────────

/// `sandbox` `list` — every sandbox run, optionally narrowed to one graph
/// and/or one status.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SandboxListParams {
    /// Only sandboxes created by this graph id.
    #[serde(default)]
    pub graph_id: Option<String>,
    /// Only sandboxes whose row carries this status (`active`, `kept`,
    /// `cleaned`, `cleanup_failed`, `merged`, `discarded`).
    #[serde(default)]
    pub status: Option<String>,
}

/// `sandbox` `get` / `land` — one sandbox by id or unambiguous id prefix.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SandboxIdParams {
    /// Full sandbox id or an unambiguous id prefix.
    pub id: String,
}

/// `sandbox` `discard` — one sandbox, with explicit consent for unique work.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SandboxDiscardParams {
    /// Full sandbox id or an unambiguous id prefix.
    pub id: String,
    /// Explicit consent to delete commits that exist only in this sandbox.
    /// Without it, unique work is never deleted.
    #[serde(default)]
    pub force: Option<bool>,
}
