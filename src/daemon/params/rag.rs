use super::*;

// ── RAG tool parameter types ───────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProjectSearchParams {
    /// Search query matched against project name and description.
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProjectUpdateParams {
    /// Project hash (workdir_hash).
    pub project_hash: String,
    /// New description.
    pub description: Option<String>,
    /// New tags list.
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProjectRemapParams {
    /// The project's current workdir_hash (see `project_search` or
    /// `canopy clean`'s orphan report).
    pub project_hash: String,
    /// The project's new absolute path on disk (where the directory was
    /// renamed or moved to).
    pub new_path: String,
    /// Preview which rows would move without changing anything. Default: false.
    pub dry_run: Option<bool>,
    /// Remap even if `new_path` doesn't exist on disk yet. Default: false.
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ProjectRegisterParams {
    /// Absolute path of the project directory to register explicitly.
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct RagSearchParams {
    /// Natural-language search query.
    pub query: String,
    /// Optional caller identity used for per-agent throttling.
    pub agent_id: Option<String>,
    /// Max results (default: 5).
    pub limit: Option<usize>,
}
