use super::*;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphNodeRunsListParams {
    /// Graph ID whose node runs to list.
    pub graph_id: String,
    /// Narrow to one spec's runs.
    pub spec_id: Option<String>,
    /// Narrow to one node's runs.
    pub node_id: Option<String>,
    /// Maximum number of runs to return, most recent first. Defaults to 20,
    /// capped at 200.
    pub limit: Option<u32>,
    /// Number of most-recent runs to skip before returning `limit` more —
    /// page past the default page (e.g. `offset: 20` for the next page after
    /// the default). Defaults to 0.
    pub offset: Option<u32>,
    /// Compact mode: emit id, node_name, status, iteration, spec_name, started_at, completed_at. Default: false.
    pub compact: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphNodeRunGetParams {
    /// The node run ID (the `id` field from graph_node_runs_list's results).
    pub run_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphPreflightReviewer {
    /// CLI platform to run the reviewer with (e.g. "opencode", "claude").
    pub platform: String,
    /// Optional model for that platform. Omitted = platform default.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphPreflightParams {
    /// Graph ID to preflight.
    pub graph_id: String,
    /// Seconds to wait for each probe's response before reporting a
    /// timeout. Defaults to 30, clamped to [5, 120].
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    /// Optional background design-review agent: when set, names the
    /// platform+model pair that audits the graph's design. The review runs
    /// in the background and never blocks or fails the probe results.
    #[serde(default)]
    pub reviewer: Option<GraphPreflightReviewer>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct GraphListParams {
    /// Optional absolute workdir filter.
    pub workdir: Option<String>,
    /// Include archived graphs in the results. Defaults to `false` — the
    /// same "browsing" view as the TUI's main Graphs list, which excludes
    /// archived graphs. An archived graph is still reachable directly by id
    /// via `graph_get` regardless of this flag.
    #[serde(default)]
    pub include_archived: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphArchiveParams {
    /// Graph ID to archive.
    pub graph_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SessionListParams {
    /// Exact session id to look up. Omit to list live sessions.
    pub session_id: Option<String>,
    /// Max rows when listing. Defaults to fit the result budget, capped at 200.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScheduledSendCreateParams {
    /// The prompt text to deliver. Plain text only — delivered to the target
    /// session exactly as typed, the same as a promptbuilder send.
    pub prompt: String,
    /// Target interactive session id. Omit to schedule to your own session.
    pub target_session_id: Option<String>,
    /// ISO 8601 absolute time to fire at. Mutually exclusive with `in_seconds`.
    pub at: Option<String>,
    /// Relative delay in whole seconds. Mutually exclusive with `at`.
    pub in_seconds: Option<i64>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScheduledSendListParams {
    /// Session id to list pending scheduled sends for. Omit for your own session.
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScheduledSendCancelParams {
    /// Scheduled send id, as returned by scheduled_send_create.
    pub id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphRestoreParams {
    /// Graph ID to restore from the archive back to the main list.
    pub graph_id: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphRunParams {
    /// Graph ID.
    pub graph_id: String,
    /// Optional queue ID. When set, the graph runs the queue's pending specs (in
    /// queue order) through the graph instead of its own bound specs.
    /// Queue membership is unaffected — specs stay standalone.
    pub queue_id: Option<String>,
    /// Optional absolute workdir override for this run only. Wins over the
    /// graph's own `workdir`; the graph's `workdir` is left unchanged.
    pub workdir: Option<String>,
    /// Optional free-form text fed to nodes as `{{spec_content}}` when the graph
    /// has no bound specs and no queue. The graph must still have a top-level
    /// graph. Mutually exclusive with `queue_id`.
    pub idea: Option<String>,
    /// When true, run this graph in a sandbox (temporary worktree with
    /// instruction file). The protocol is materialized in the worktree's
    /// instruction file instead of being injected into the prompt.
    pub sandbox: Option<bool>,
    /// When set, start the first spec this run executes at this graph node
    /// instead of at the graph's Start node: a node id, an unambiguous node-id
    /// prefix, a node name, or an ensemble name. Only the first spec is
    /// directed; later specs begin at Start as usual.
    #[serde(default)]
    pub start_node_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphPauseParams {
    /// Graph ID.
    pub graph_id: String,
    /// If true, immediately terminate the running node and mark it as
    /// interrupted. If false (default), wait for the running node to
    /// complete naturally before pausing.
    #[serde(default)]
    pub interrupt: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphResetParams {
    /// Graph ID.
    pub graph_id: String,
    /// Specific spec IDs to reset to pending, even if already completed.
    /// Omit to reset every spec that isn't already completed, leaving
    /// completed specs untouched so graph_run resumes at the first pending one.
    pub specs: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphContinueParams {
    /// Graph ID.
    pub graph_id: String,
    /// Continue mode: retry_current_node, skip_next_spec or run_from_node.
    pub action: String,
    /// Action argument for `run_from_node`: the node (id, unambiguous id prefix,
    /// node name, or ensemble name) the directed run starts at.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Action argument for `run_from_node`: the spec to start at that node.
    /// Defaults to the graph's current spec.
    #[serde(default)]
    pub spec_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphScheduleAutorunParams {
    /// Graph ID.
    pub graph_id: String,
    /// ISO 8601 timestamp at which the graph should resume, e.g.
    /// "2026-07-10T09:00:00Z". Fires once, then the schedule is cleared.
    /// Omit (or pass null) to cancel any pending autorun schedule instead of
    /// setting a new one. Mutually exclusive with `quota_reset_message` —
    /// prefer that field for a quota-limit CLI message instead of computing
    /// the timestamp yourself.
    pub at: Option<String>,
    /// Raw CLI quota-limit message, e.g. "You've hit your session limit ·
    /// resets 1pm (America/Bogota)". When set, the engine parses the stated
    /// local reset time and timezone itself and computes the UTC resume
    /// instant deterministically (plus a small safety margin) instead of
    /// requiring you to do that arithmetic — this is the preferred way to
    /// reschedule after a quota failure. Mutually exclusive with `at`.
    pub quota_reset_message: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LoopScheduleAutorunParams {
    /// Graph ID. Also accepts `loop_id` — the 2.x parameter name, kept
    /// working by `loop_schedule_autorun` (deprecated alias for
    /// `graph_schedule_autorun`, CB68).
    #[serde(alias = "loop_id")]
    pub graph_id: String,
    /// Same as `graph_schedule_autorun`'s `at`.
    pub at: Option<String>,
    /// Same as `graph_schedule_autorun`'s `quota_reset_message`.
    pub quota_reset_message: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphScheduleContinueParams {
    /// Graph ID.
    pub graph_id: String,
    /// ISO 8601 timestamp at which a still-paused graph should auto-continue,
    /// e.g. "2026-07-10T09:00:00Z". Fires once, then the schedule is
    /// cleared. Omit (or pass null) to cancel any pending auto-continue
    /// schedule instead of setting a new one.
    pub at: Option<String>,
    /// `graph_continue` action to apply when it fires: retry_current_node or
    /// skip_next_spec. Defaults to retry_current_node when omitted.
    pub action: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphCompleteNodeParams {
    /// The exact node run ID this report belongs to (given to you in the
    /// [REPORTING] section of your prompt). Required so a report can never
    /// be misattributed to a different, newer attempt at the same node.
    pub run_id: String,
    /// Graph node ID.
    pub node_id: String,
    /// pass or fail.
    pub status: String,
    /// Node output payload.
    pub output: String,
    /// Human-readable summary.
    pub summary: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GraphReportBlockerParams {
    /// The exact node run ID this report belongs to (given to you in the
    /// [REPORTING] section of your prompt). Required so a report can never
    /// be misattributed to a different, newer attempt at the same node.
    pub run_id: String,
    /// Graph node ID.
    pub node_id: String,
    /// Human-readable blocker description.
    pub description: String,
}
