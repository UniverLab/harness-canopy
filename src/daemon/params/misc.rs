use super::*;

// ── Seed Identity tool parameter types ─────────────────────────────

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EvolveIdentityParams {
    /// New directives list (replaces existing).
    pub new_directives: Option<Vec<String>>,
    /// New traits map (updates existing keys).
    pub new_traits: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CreateSeedParams {
    /// Unique display name (enforced case-insensitive across all seeds).
    pub name: String,
    /// Behavioral directives injected into prompts.
    pub directives: Option<crate::domain::seeds::SeedDirectives>,
    /// Personality/style traits.
    pub traits: Option<crate::domain::seeds::SeedTraits>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoveSeedParams {
    /// Seed ID to remove.
    pub seed_id: String,
}

// ── Dynamic skill store tool parameter types ──────────────────────────

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SkillGetParams {
    /// Skill directory name, as reported by `skill` `list` (e.g. "code-review").
    pub name: String,
}

// ── Ephemeral subagent tool parameter types ───────────────────────────

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubagentSpawnParams {
    /// The instruction for the subagent to execute.
    pub prompt: String,
    /// CLI platform to use. Auto-detects if omitted.
    pub cli: Option<String>,
    /// Working directory for the subagent. Omit to inherit the caller's:
    /// the calling agent's project for an MCP call, or the process cwd for
    /// the `canopy subagent` CLI (which passes it explicitly).
    pub workdir: Option<String>,
    /// Optional provider/model string.
    pub model: Option<String>,
    /// Optional effort level (e.g. "low", "medium", "high"). Values accepted
    /// depend on the target platform; an unsupported value is refused before
    /// the subagent is spawned. Omit for the platform's own default.
    pub effort: Option<String>,
    /// MCP servers to expose (by name). Empty/omitted = blind (no MCP).
    /// Include "canopy" to make the canopy server visible.
    pub mcp_servers: Option<Vec<String>>,
    /// Timeout in minutes. Default: 15.
    pub timeout_minutes: Option<u64>,
    /// TTL in minutes before an uncollected result expires. Default: 60.
    pub ttl_minutes: Option<u64>,
    /// When true, wait for the subagent to finish and return its result
    /// directly, instead of returning an id for later `subagent` `collect`.
    /// Default false (async, today's behaviour).
    #[serde(default)]
    pub blocking: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubagentCollectParams {
    /// The subagent run ID returned by `subagent` `spawn`.
    pub id: String,
}
