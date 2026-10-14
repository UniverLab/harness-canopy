use super::*;

// ── CM48 roster tool parameter types ───────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RosterSetParams {
    /// Platform to declare availability for (e.g. `opencode-go`).
    pub platform: String,
    /// `None` (every model of the platform), an exact model id, or a prefix
    /// ending in `*` (e.g. `opencode-go/*`).
    pub model_pattern: Option<String>,
    /// `available` or `unavailable`.
    pub state: String,
    /// Why the pair is unavailable. Stored on the entry whatever the state.
    pub reason: Option<String>,
    /// RFC 3339 UTC instant the unavailability expires. Only valid with
    /// `state=unavailable`, and must be in the future.
    pub until: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RosterClearParams {
    /// Platform whose entry to delete.
    pub platform: String,
    /// The exact pattern deleted; omit to delete the platform-wide entry.
    pub model_pattern: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RosterListParams {
    /// Restrict the listing to one platform.
    pub platform: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RosterCheckParams {
    /// Platform to check.
    pub platform: String,
    /// Model to check; omit to check the platform-wide entry.
    pub model: Option<String>,
}
