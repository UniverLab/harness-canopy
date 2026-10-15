use super::*;

// ── Legacy MCP tool parameter types (used by backward-compatible tools) ──

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskAddParams {
    /// Unique identifier. Lowercase, hyphens, underscores.
    pub id: String,
    /// The instruction the CLI will execute headlessly.
    pub prompt: String,
    /// Standard 5-field cron expression: minute hour day month weekday.
    pub schedule: String,
    /// CLI to use. Auto-detects if omitted.
    pub cli: Option<String>,
    /// Optional provider/model string.
    pub model: Option<String>,
    /// Optional effort level (e.g. "low", "medium", "high").
    pub effort: Option<String>,
    /// Auto-expire after N minutes from registration.
    pub duration_minutes: Option<i64>,
    /// Working directory for the CLI.
    pub working_dir: Option<String>,
    /// Timeout in minutes for execution locking. Default: 15.
    pub timeout_minutes: Option<u32>,
    /// Delivery action: "headless" (default) runs a CLI, or
    /// "send_to_session" delivers the prompt into a live interactive session.
    /// `cli` is ignored when this is "send_to_session".
    pub action: Option<String>,
    /// Target session *name* for action "send_to_session" (recommended — a
    /// name survives session recovery). Mutually exclusive with
    /// `target_session_id`.
    pub target_session_name: Option<String>,
    /// Target session *id* for action "send_to_session". Omit both target
    /// fields to target the calling session.
    pub target_session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskWatchParams {
    /// Unique identifier for the watcher.
    pub id: String,
    /// Absolute path to file or directory to watch.
    pub path: String,
    /// Events to watch: "create", "modify", "delete", "move", or "all".
    pub events: Vec<String>,
    /// Instruction for the CLI on trigger.
    pub prompt: String,
    /// CLI to use. Auto-detects if omitted.
    pub cli: Option<String>,
    /// Optional provider/model string.
    pub model: Option<String>,
    /// Optional effort level.
    pub effort: Option<String>,
    /// Debounce window in seconds (default: 2).
    pub debounce_seconds: Option<u64>,
    /// Watch subdirectories (default: false).
    pub recursive: Option<bool>,
    /// Timeout in minutes for execution locking. Default: 15.
    pub timeout_minutes: Option<u32>,
    /// Delivery action: "headless" (default) runs a CLI, or
    /// "send_to_session" delivers the prompt into a live interactive session.
    pub action: Option<String>,
    /// Target session *name* for action "send_to_session".
    pub target_session_name: Option<String>,
    /// Target session *id* for action "send_to_session".
    pub target_session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskUpdateParams {
    /// ID of the agent to update.
    pub id: String,
    /// New agent ID to rename this agent to. Must be unique and valid
    /// (lowercase alphanumerics, hyphens, underscores). Updates the agent
    /// row, its log path, and all run references atomically.
    pub new_id: Option<String>,
    /// New prompt/instruction.
    pub prompt: Option<String>,
    /// New CLI platform name.
    pub cli: Option<String>,
    /// New provider/model string, or null to clear.
    pub model: Option<Option<String>>,
    /// New effort level, or null to clear.
    pub effort: Option<Option<String>>,
    /// New 5-field cron expression (cron agents only), e.g. `"30 * * * *"`
    /// (top of every hour at :30). Standard cron syntax: minute hour day
    /// month weekday, where `*` means "any value". Pass the value as a
    /// normal JSON string — no shell quoting or escaping is needed.
    pub schedule: Option<String>,
    /// New working directory, or null to clear.
    pub working_dir: Option<Option<String>>,
    /// New duration in minutes from now, or null to clear expiration.
    pub duration_minutes: Option<Option<i64>>,
    /// New absolute path to watch (watch agents only).
    pub path: Option<String>,
    /// New event list (watch agents only).
    pub events: Option<Vec<String>>,
    /// New debounce window in seconds (watch agents only).
    pub debounce_seconds: Option<u64>,
    /// Watch subdirectories (watch agents only).
    pub recursive: Option<bool>,
    /// Enable or disable the agent.
    pub enabled: Option<bool>,
    /// Opt this agent into a desktop toast on every *successful* run. When
    /// false (the default), successful scheduled/watch runs stay silent and
    /// only failures notify — so a frequent agent can't spam notifications.
    /// Manual `agent_run` executions always report success regardless.
    pub notify_on_success: Option<bool>,
    /// Set the delivery action: "headless" runs a CLI, "send_to_session"
    /// delivers the prompt into a live interactive session.
    pub action: Option<String>,
    /// Target session *name* for action "send_to_session".
    pub target_session_name: Option<String>,
    /// Target session *id* for action "send_to_session".
    pub target_session_id: Option<String>,
}

// ── Shared parameter types ─────────────────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskLogsParams {
    /// Agent ID.
    pub id: String,
    /// Last N lines to return (default: 50).
    pub lines: Option<usize>,
    /// ISO 8601 timestamp filter — only return logs after this time.
    pub since: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct IdParam {
    /// Agent ID.
    pub id: String,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct TaskModelsParams {
    /// Optional platform/CLI name (e.g. "opencode", "claude") to return only
    /// the models available to that configured platform. Omit for the full
    /// provider list.
    #[serde(default)]
    pub platform: Option<String>,
    /// When true, force a fresh fetch from models.dev instead of serving a
    /// still-fresh local cache — use this to pick up newly published models.
    #[serde(default)]
    pub refresh: Option<bool>,
    /// When true, bypass the per-provider and per-listing caps on the
    /// **unfiltered** (no-platform) listing to show every model. Defaults to
    /// false, which truncates long listings with a notice naming the provider,
    /// how many were shown, and how many exist.
    ///
    /// Platform-scoped listings (with `platform` set) are never capped and
    /// return all models the platform can reach — the `full` flag has no
    /// effect on them. A caller already narrowed by platform gets the
    /// complete answer; the cap only applies to the unfiltered, provider-wide
    /// listing where the catalogue can be pathological.
    #[serde(default)]
    pub full: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct AgentScheduleEnableParams {
    /// Agent ID.
    pub id: String,
    /// ISO 8601 timestamp at which the agent should be enabled, e.g.
    /// "2026-07-10T09:00:00Z". The agent stays disabled until then.
    pub at: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TaskReportParams {
    /// The run ID (UUID) provided in the agent execution prompt.
    pub run_id: String,
    /// Execution status: `in_progress`, `success`, or `error`.
    pub status: String,
    /// Brief summary of what happened (required for success/error).
    pub summary: Option<String>,
}

/// Deserialize a doubly-optional field so "key absent" and "key present but
/// `null`" stay distinct. `#[serde(default)]` on an `Option<Option<T>>`
/// field gives `None` when the key is missing; without this helper a present
/// `null` also collapses to `None`, because serde's `Option` impl maps JSON
/// `null` straight to `None` before the inner `Option` is ever consulted.
/// Routing the present value through here wraps it: `null` becomes
/// `Some(None)` (clear) and a value becomes `Some(Some(v))` (set).
pub(super) fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct AgentProbeParams {
    /// Platform to probe (e.g. "claude"). Omit to probe every platform
    /// configured in canopy, each with its own default model.
    #[serde(default)]
    pub platform: Option<String>,
    /// Model to probe for `platform` — validates the exact platform+model
    /// pair a graph node would use, instead of the platform's default.
    /// Requires `platform`; omit both to sweep every configured platform.
    #[serde(default)]
    pub model: Option<String>,
    /// Seconds to wait for a response before reporting a timeout (distinct
    /// from a response that came back but didn't contain the probe token).
    /// Defaults to 30, clamped to [5, 120] — this is a liveness check, not a
    /// capability benchmark.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct AgentProbeRecentParams {
    /// Seconds to wait for each response. Defaults to 30, clamped to [5, 120].
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Maximum number of distinct recent pairs to probe. Defaults to 15,
    /// clamped to [1, 50].
    #[serde(default)]
    pub max_pairs: Option<u32>,
}
