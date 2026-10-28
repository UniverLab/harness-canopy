use super::*;

/// CM50: `crew assignments` — every graph's harness/model assignments, without
/// the prompts. All filters are optional: with none, the answer covers
/// every non-archived graph (or every graph with `include_archived`).
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphCrewParams {
    /// Full id of the one graph to report on. Omit for every graph; an
    /// unknown id is an error rather than an empty answer.
    pub graph_id: Option<String>,
    /// Filter to an exact platform match (e.g. "claude", "opencode-go").
    pub platform: Option<String>,
    /// Filter to a case-insensitive model substring match (e.g. "opus").
    pub model: Option<String>,
    /// Include archived graphs. Defaults to false.
    pub include_archived: Option<bool>,
    /// When "pair", return one summary row per distinct platform/model
    /// pair — graph, ensemble, node and hook counts plus the graph names —
    /// instead of per-graph rows.
    pub group_by: Option<String>,
}
