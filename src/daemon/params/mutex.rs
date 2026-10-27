use super::*;

// ── Named mutex tool parameter types ─────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct MutexParams {
    /// Action to perform: `list`.
    pub action: String,
}
