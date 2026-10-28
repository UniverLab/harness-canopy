use super::*;

// ── Node authoring and node-catalogue parameter types ─────────────────

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphAddNodeParams {
    /// Existing spec ID. Provide exactly one of `spec_id`/`graph_id`.
    pub spec_id: Option<String>,
    /// Existing graph ID, to add this node to the graph's top-level graph
    /// instead of a spec's graph. Provide exactly one of `spec_id`/`graph_id`.
    pub graph_id: Option<String>,
    /// Human-readable node name.
    pub name: String,
    /// Node kind: agent, check, or gate. Optional when `blueprint` or `node`
    /// is given — the template's own kind applies. Never pass it with `node`:
    /// the node document already declares its kind.
    pub kind: Option<String>,
    /// Kind-specific configuration object. Alternative to `blueprint` and
    /// `node`; provide exactly one of `config`/`blueprint`/`node`.
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    /// Name of an existing blueprint (builtin or custom) to base this node
    /// on, instead of a full `config`. See `blueprint_list`.
    pub blueprint: Option<String>,
    /// Name of an installed or builtin node document to base this node on
    /// (see `node_list`) — the alternative to `kind` + `config`. An agent
    /// node document never carries a harness, so pass `config_overrides`
    /// with `platform` (or `cli`, or `crew`) to choose one.
    pub node: Option<String>,
    /// Shallow overrides merged onto the template's config — blueprint or
    /// node document. Override keys win, every other templated key is
    /// preserved.
    pub config_overrides: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphAddEnsembleParams {
    /// Existing spec ID. Provide exactly one of `spec_id`/`graph_id`.
    pub spec_id: Option<String>,
    /// Existing graph ID, to add this ensemble to the graph's top-level graph
    /// instead of a spec's graph. Provide exactly one of `spec_id`/`graph_id`.
    pub graph_id: Option<String>,
    /// Human-readable ensemble name.
    pub name: String,
    /// Ensemble execution strategy: "parallel" (default), "cascade", or
    /// "round_robin". Parallel runs all members concurrently and counts
    /// passes against min_pass. Cascade tries members in order; the first
    /// usable result wins. Round_robin rotates across members.
    pub kind: Option<String>,
    /// The one shared prompt every member renders — supports the same
    /// placeholders as an agent node's `prompt_template`. Required unless a
    /// `blueprint` or `node` supplies one.
    pub prompt_template: Option<String>,
    /// Members (2–8 for parallel, 2–16 for cascade/round_robin): proposer/
    /// reviewer variants that differ by
    /// platform/model and, optionally, per-member `prompt_override`.
    /// Required when `node` is given (the document carries no harnesses);
    /// otherwise required unless a `blueprint` supplies them.
    pub members: Option<Vec<EnsembleMemberParams>>,
    /// Name of an existing ensemble blueprint (e.g. "ensemble-proposers") to
    /// source `prompt_template`/`members` from when they're omitted above.
    /// An explicit `prompt_template`/`members` still wins if both are given.
    pub blueprint: Option<String>,
    /// Name of an installed or builtin ensemble node document (see
    /// `node_list`, e.g. "ensemble-proposers") to source the shared prompt,
    /// the member count, and the ensemble shape from. Its members carry no
    /// harnesses, so `members` must still be passed with a platform for each
    /// one. Mutually exclusive with `blueprint` and `crew`.
    pub node: Option<String>,
    /// Existing node ID this ensemble is wired from. Every member gets an
    /// incoming edge from this node with `condition`.
    pub from_node: String,
    /// Entry routing condition from `from_node`: pass, fail, or always.
    pub condition: String,
    /// Members required to pass for the quorum to report `pass`. Defaults to
    /// every member, or the node document's own `min_pass`.
    pub min_pass: Option<i64>,
    /// Minutes a member may run before the quorum kills it and counts it as
    /// failed. Defaults to `timeout_minutes` (the members' own agent
    /// timeout).
    pub straggler_timeout_minutes: Option<i64>,
    /// CM24: optional grace after quorum met before terminating stragglers.
    /// None keeps wait-for-all. 0 = immediate. Only parallel ensembles honour it.
    pub quorum_grace_minutes: Option<i64>,
    /// Shared agent timeout (minutes) applied to every member. Defaults to
    /// 30, matching an ordinary agent node.
    pub timeout_minutes: Option<i64>,
    /// CM30: ensemble-level default infra-retry budget, applied to every
    /// member unless that member sets its own; omit to defer to the
    /// platform's config.toml value, then the engine default (2).
    pub infra_retry_limit: Option<i64>,
    /// CM30: see infra_retry_limit. Ensemble-level default for
    /// infra_crash_max_seconds (engine default 60).
    pub infra_crash_max_seconds: Option<i64>,
    /// CM30: see infra_retry_limit. Ensemble-level default for
    /// infra_backoff_seconds (engine default 30).
    pub infra_backoff_seconds: Option<i64>,
    /// Existing node ID the quorum routes to on `pass` (e.g. an arbiter node).
    pub on_pass_to: String,
    /// Existing node ID the quorum routes to on `fail`. Omit for a dead end on
    /// fail, same as any other node with no matching outgoing edge.
    pub on_fail_to: Option<String>,
    /// CM28/B37: whether this ensemble is the graph's designated committer.
    /// Whichever member the strategy runs for a dispatch is the committer
    /// for that dispatch — never set per-member. Defaults to false, same as
    /// an ordinary node with no `commit_rights` key.
    pub commit_rights: Option<bool>,
    /// CM49: a crew name to bind instead of `members` — mutually exclusive
    /// with `members`/a blueprint that supplies members. The crew's ordered
    /// members become this ensemble's members, and later `crew_update` calls
    /// reach it at its next dispatch.
    pub crew: Option<String>,
    /// Named mutexes (e.g. `["cpu"]`) every member run holds while it
    /// executes — one member run per resource at a time across all graphs.
    /// Each name must match `^[a-z][a-z0-9-]{0,31}$`. Omit for none.
    pub mutex: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeInstallParams {
    /// Catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each with an
    /// optional `@<ref>` suffix selecting a version — or `draft:<id>` for
    /// an editor draft.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Overwrite a local preset file whose content differs from the bundled
    /// one instead of refusing the install. Default false.
    pub overwrite_presets: Option<bool>,
    /// Confirmation that the install review was shown to the user. Omit (or
    /// false) to fetch, validate and return `{needs_review: true, review}`
    /// with every command, prompt and attention mark and install nothing;
    /// true to install and echo the same review in the result. An agent must
    /// show the review to its user before passing true.
    pub accept: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeRemoveParams {
    /// Name of an installed node document to remove. A builtin with no
    /// installed override cannot be removed; its override can.
    pub name: String,
}
