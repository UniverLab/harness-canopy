use super::*;

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
    /// Notify this live interactive session (id or name) with a scheduled
    /// prompt when a non-blocking subagent ends. Omitted from a live
    /// interactive session defaults to that session; omitted from anywhere
    /// else notifies nobody. Refused with `blocking: true`.
    #[serde(default)]
    pub notify_session: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SubagentCollectParams {
    /// The subagent run ID returned by `subagent` `spawn`.
    pub id: String,
}
