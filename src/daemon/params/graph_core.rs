use super::*;

use crate::domain::graphs::SpecType;

// ── Graph tool parameter types ────────────────────────────────────────

/// Optional automatic trigger for a graph, mirroring agent triggers. A graph can
/// fire on a cron schedule or a file-system watch instead of only `graph_control run`.
/// Also `Serialize` so the TUI's graph form can send it over MCP verbatim.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphTriggerParams {
    /// Trigger kind: "cron", "watch", or "manual". "manual" (the default)
    /// clears any existing trigger, so the graph only runs via graph_control run.
    pub kind: String,
    /// 5-field cron expression (required when kind = "cron").
    pub schedule: Option<String>,
    /// Absolute path to a file or directory to watch (required when kind = "watch").
    pub path: Option<String>,
    /// Events to watch: "create", "modify", "delete", "move", or "all" (watch only).
    pub events: Option<Vec<String>>,
    /// Debounce window in seconds (watch only, default: 2).
    pub debounce_seconds: Option<u64>,
    /// Watch subdirectories recursively (watch only, default: false).
    pub recursive: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphCreateParams {
    /// Human-readable graph name.
    pub name: String,
    /// Optional graph description.
    pub description: Option<String>,
    /// Absolute working directory for the graph.
    pub workdir: String,
    /// Optional automatic trigger (cron/watch). Omit for a manual graph.
    pub trigger: Option<GraphTriggerParams>,
    /// Optional pre-wired target for infrastructure failures (`Error` edges).
    /// When set, every new agent/check/gate node auto-creates a `Error` edge
    /// to this node.
    pub infra_node_id: Option<String>,
    /// CM29: when true, graph_control run/autorun downgrade the dirty-worktree
    /// refusal to a warning instead of refusing to launch. Default false.
    #[serde(default)]
    pub allow_dirty_start: bool,
    /// SPECCODE: opt-in enforcement. When true, adding a spec to this graph
    /// (graph_edit attach_spec, queue add_spec) and graph_control run refuse a spec without
    /// a type, named. Default false — never enforced until asked.
    #[serde(default)]
    pub require_spec_type: bool,
}

/// Config for a graph hook — an agent-node-style payload
/// (platform/model/prompt), a direct shell command, or an interactive
/// message into a live session (prompt + target_session_id or
/// target_session_name). Exactly one mode must be configured; the engine
/// refuses hooks that specify more than one mode or none.
/// Used for every event via the `hooks` map on `graph_edit update`.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphCompletionHookParams {
    /// CLI platform to run the hook with (e.g. "mimo", "claude").
    /// Required for agent hooks; must be omitted for command and
    /// interactive hooks. Mutually exclusive with `crew`.
    pub platform: Option<String>,
    /// CM49: crew name to run the hook with — the alternative to `platform`.
    /// The hook resolves its pair from the crew at fire time. Mutually
    /// exclusive with `platform`; agent hooks only.
    pub crew: Option<String>,
    /// Optional model override (agent hooks only).
    pub model: Option<String>,
    /// Optional effort level (agent hooks only).
    pub effort: Option<String>,
    /// Hook prompt template (agent and interactive hooks). Supports {{graph_name}} and
    /// {{workdir}} on every event, plus event-specific markers:
    /// {{completed_specs}} on on_completed, {{spec_name}} and {{spec_id}} on
    /// on_spec_completed, {{blocker}} and {{node}} on on_failed and
    /// on_blocked. A marker its event cannot bind is refused and recorded as
    /// a failed hook run.
    pub prompt: Option<String>,
    /// Shell command to run directly (command hooks only). Mutually exclusive
    /// with platform/prompt. Supports the same `{{...}}` placeholders as the
    /// hook's event, substituted before execution as POSIX shell-quoted
    /// single-quoted literals (`'...'`, with internal `'` escaped as `'\''`).
    /// The same values are exported as `CANOPY_HOOK_*` environment variables,
    /// including `CANOPY_HOOK_LOOP_NAME` (alias `CANOPY_HOOK_GRAPH_NAME`),
    /// `CANOPY_HOOK_WORKDIR`, `CANOPY_HOOK_SPEC_NAME`, `CANOPY_HOOK_SPEC_ID`,
    /// `CANOPY_HOOK_COMPLETED_SPECS`, `CANOPY_HOOK_NODE`,
    /// `CANOPY_HOOK_BLOCKER`, and `CANOPY_HOOK_EVENT`. Use those variables to
    /// avoid interpolation entirely. WARNING: do not configure a
    /// command that starts a canopy binary (e.g. `canopy graph run`). A
    /// process that starts canopy triggers daemon-startup recovery, which
    /// SIGTERMs live graph runs including the run that spawned the hook.
    /// `on_spec_completed` fires while the graph is still running, so this is
    /// not hypothetical. Use the native `graph_control run` action instead.
    pub command: Option<String>,
    /// Exact interactive session id to deliver to (interactive hooks only).
    /// Mutually exclusive with platform/model/effort/command. Requires
    /// `prompt`. The id is not stable over time: when the session is gone,
    /// firing fails naming the id. An interactive send enqueued while no TUI
    /// is running stays queued and is delivered when a TUI later starts — it
    /// is never lost and never redirected elsewhere.
    #[serde(default)]
    pub target_session_id: Option<String>,
    /// Session name an interactive hook delivers to (interactive hooks
    /// only) — an alternative to `target_session_id` that survives the
    /// session's id changing later (e.g. a daemon reinstall), because it is
    /// resolved against live sessions by name every time the hook fires.
    /// Mutually exclusive with `target_session_id`: set exactly one.
    /// Resolution is never cached between fires. Zero live sessions with
    /// this name, or more than one, fails the hook loudly instead of
    /// guessing — see `session_list` to check names before configuring
    /// this.
    #[serde(default)]
    pub target_session_name: Option<String>,
    /// Timeout in minutes for the hook's process (default: 30, same default
    /// as an agent node).
    pub timeout_minutes: Option<u64>,
    /// Target graph id to launch (graph hooks only). Mutually exclusive with
    /// platform/command/target_session_id. When set, this hook launches
    /// another graph in-process instead of spawning a CLI process.
    pub target_graph_id: Option<String>,
    /// Optional queue id for the launched graph (graph hooks only).
    pub queue_id: Option<String>,
    /// Optional workdir override for the launched graph (graph hooks only).
    pub workdir_override: Option<String>,
    /// Optional idea text for the launched graph (graph hooks only). Mutually
    /// exclusive with queue_id. Supports the same `{{...}}` placeholders as
    /// the hook's event.
    pub idea: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphUpdateParams {
    /// Graph ID.
    pub graph_id: String,
    /// New human-readable graph name.
    pub name: Option<String>,
    /// New graph description, or null to clear.
    pub description: Option<Option<String>>,
    /// New absolute workdir for the graph.
    pub workdir: Option<String>,
    /// New automatic trigger. Provide kind = "manual" to clear it. Omit to
    /// leave the current trigger unchanged.
    pub trigger: Option<GraphTriggerParams>,
    /// New `on_completed` post-completion hook config, or null to clear it.
    /// Omit to leave the current hook unchanged. This is a compatibility
    /// alias — prefer `hooks` for event-keyed hook management. Legacy update
    /// replaces/registers only `on_completed` and does not clear other events.
    pub on_completed: Option<Option<GraphCompletionHookParams>>,
    /// Event-keyed hooks. Replace the full hooks map with this map. Each key
    /// is an event name (`on_completed`, `on_failed`, `on_blocked`,
    /// `on_spec_completed`), and each value is an ordered array of hook
    /// configs. Hooks are not retroactive — a hook registered after its
    /// event has already happened does not fire. Omit to leave unchanged.
    /// When `on_completed` is also provided, it is merged into this map
    /// under the `on_completed` key.
    pub hooks: Option<std::collections::BTreeMap<String, Vec<GraphCompletionHookParams>>>,
    /// New pre-wired target for infrastructure failures (`Error` edges), or
    /// null to clear. Omit to leave unchanged.
    pub infra_node_id: Option<Option<String>>,
    /// CM29: set to change whether graph_control run/autorun downgrade the
    /// dirty-worktree refusal to a warning. Omit to leave unchanged.
    pub allow_dirty_start: Option<bool>,
    /// SPECCODE: opt-in enforcement. When true, adding a spec to this graph
    /// (graph_edit attach_spec, queue add_spec) and graph_control run refuse a spec without
    /// a type, named. Default false — never enforced until asked.
    pub require_spec_type: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphAddSpecParams {
    /// Existing graph ID.
    pub graph_id: String,
    /// Human-readable spec name.
    pub name: String,
    /// Optional spec description. Must use the tagged `<spec>` format.
    /// Required: `<objective>`, `<functional_requirements>`, `<guidelines>`.
    /// Optional: `<non_functional_requirements>`, `<constraints>`, `<in_scope>`,
    /// `<out_of_scope>`. Markdown is allowed inside each section.
    pub description: Option<String>,
    /// Execution order within the graph.
    pub position: i64,
    /// Whether the spec is allowed to run in parallel in future engine phases.
    pub parallelizable: bool,
    /// SPECCODE: Conventional Commits type of the change (`feat`, `fix`,
    /// `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`,
    /// `revert`). Optional — when omitted and the name is a conventional
    /// title (`fix(tui): ...`), the type and scope are parsed from the name
    /// and stored as the subject alone; the engine then numbers the spec
    /// (`FIX-7`). A graph with `require_spec_type` refuses a spec without
    /// one.
    pub spec_type: Option<SpecType>,
    /// SPECCODE: optional scope of that type (e.g. `tui`), part of the
    /// prebuilt commit message `fix(tui): <name>`.
    pub spec_scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphUpdateSpecParams {
    /// Existing spec ID.
    pub spec_id: String,
    /// New human-readable spec name.
    pub name: Option<String>,
    /// New spec description. Must use the tagged `<spec>` format. Required:
    /// `<objective>`, `<functional_requirements>`, `<guidelines>`; four more
    /// are optional. Markdown is allowed inside each section.
    pub description: Option<String>,
    /// New execution order within the graph.
    pub position: Option<i64>,
    /// Whether the spec is allowed to run in parallel.
    pub parallelizable: Option<bool>,
    /// SPECCODE: change the spec's Conventional Commits type. On a legacy
    /// spec (no code yet) this consumes the next number for the type and
    /// assigns the immutable code; an already-coded spec keeps its code
    /// (never renumbered).
    pub spec_type: Option<SpecType>,
    /// SPECCODE: change the spec's scope (the parenthesised part of the
    /// future commit message). Omit to leave unchanged.
    pub spec_scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecCreateParams {
    /// Human-readable spec name.
    pub name: String,
    /// Spec description. Must use the tagged `<spec>` format. Required:
    /// `<objective>`, `<functional_requirements>`, `<guidelines>`. Optional:
    /// `<non_functional_requirements>`, `<constraints>`, `<in_scope>`,
    /// `<out_of_scope>`. Markdown is allowed inside each section.
    pub description: String,
    /// Optional absolute workdir tag, for backlog filtering only — it does
    /// not drive execution.
    pub workdir: Option<String>,
    /// SPECCODE: Conventional Commits type of the change. When omitted and
    /// the name is a conventional title (`fix(tui): ...`), type and scope
    /// are parsed from the name and the name stored as the subject; when
    /// both are absent the spec is created without a type/code/commit
    /// message. An explicit type wins over the parsed one.
    pub spec_type: Option<SpecType>,
    /// SPECCODE: optional scope of that type; overridable even when the
    /// scope was parsed from a conventional title.
    pub spec_scope: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecListParams {
    /// Filter to specs tagged with this absolute workdir.
    pub workdir: Option<String>,
    /// Filter to specs in this status (pending, running, completed, failed, skipped).
    pub status: Option<String>,
    /// Only return specs not yet assigned to any graph.
    pub unassigned_only: Option<bool>,
    /// SPECCODE: filter to specs carrying this Conventional Commits type.
    pub spec_type: Option<SpecType>,
    /// Include full spec descriptions in the output. Default: false (compact — id, name, status, workdir, graph_id only).
    pub include_descriptions: Option<bool>,
    /// Maximum number of specs to return. Defaults to a value that fits the
    /// result budget, clamped to [1, 200].
    pub limit: Option<u32>,
    /// Number of specs to skip before returning `limit` more — page past the
    /// default page. Defaults to 0.
    pub offset: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecUpdateParams {
    /// Existing spec ID.
    pub spec_id: String,
    /// New human-readable spec name.
    pub name: Option<String>,
    /// New spec description. Must use the tagged `<spec>` format. Required:
    /// `<objective>`, `<functional_requirements>`, `<guidelines>`; four more
    /// are optional. Markdown is allowed inside each section.
    pub description: Option<String>,
    /// New absolute workdir tag, or null to clear it.
    pub workdir: Option<Option<String>>,
    /// SPECCODE: set the spec's Conventional Commits type. On a legacy spec
    /// (no code yet) this assigns the immutable code at that moment; a
    /// later type change keeps the existing code.
    pub spec_type: Option<SpecType>,
    /// SPECCODE: set (or clear with null) the scope part of the commit
    /// message.
    pub spec_scope: Option<Option<String>>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecSetStatusParams {
    /// Existing spec ID.
    pub spec_id: String,
    /// Target status: `completed`, `skipped`, or `pending` (reopen).
    pub status: String,
    /// Reason for the administrative transition.
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecDeleteParams {
    /// Existing spec ID.
    pub spec_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecGetParams {
    /// Existing spec ID (or unambiguous id prefix).
    pub spec_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SpecSectionGetParams {
    /// Existing spec ID.
    pub spec_id: String,
    /// Canonical section tag name (one of: objective, functional_requirements,
    /// non_functional_requirements, constraints, guidelines, in_scope, out_of_scope).
    pub section: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BlueprintCreateParams {
    /// Unique blueprint name.
    pub name: String,
    /// Node kind: agent, check, or gate.
    pub kind: String,
    /// Config template. `graph_node add` uses this as the node's config,
    /// optionally shallow-merged with `config_overrides`.
    pub config: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BlueprintDeleteParams {
    /// Existing custom blueprint name. Builtins can't be deleted.
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphUpdateNodeParams {
    /// Existing node ID.
    pub node_id: String,
    /// New human-readable node name.
    pub name: Option<String>,
    /// New node kind: agent, check, or gate.
    pub kind: Option<String>,
    /// Config payload. By default merges with the stored config (partial update):
    /// keys present are changed, keys absent are left as-is.
    /// Set `config_replace` to `true` to replace the entire config instead.
    pub config: Option<serde_json::Map<String, serde_json::Value>>,
    /// When `true`, `config` replaces the entire stored config instead of merging.
    /// Defaults to `false` (merge).
    pub config_replace: Option<bool>,
    /// New visual position within the spec.
    pub position: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphAddEdgeParams {
    /// Existing spec ID. Provide exactly one of `spec_id`/`graph_id`.
    pub spec_id: Option<String>,
    /// Existing graph ID, to add this edge to the graph's top-level graph
    /// instead of a spec's graph. Provide exactly one of `spec_id`/`graph_id`.
    pub graph_id: Option<String>,
    /// Source node ID.
    pub from_node: String,
    /// Destination node ID.
    pub to_node: String,
    /// Routing condition: pass, fail, always, or route.
    pub condition: String,
    /// Route label this edge serves — required when `condition` is
    /// `"route"`, and must name one of `from_node`'s declared routes (see
    /// `graph_node add`'s router `config.routes`). Ignored otherwise.
    pub route: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphUpdateEdgeParams {
    /// Existing edge ID.
    pub edge_id: String,
    /// New routing condition: pass, fail, always, or route.
    pub condition: String,
    /// Route label this edge serves — required when `condition` is
    /// `"route"`, and must name one of the edge's `from_node`'s declared
    /// routes. Ignored otherwise.
    pub route: Option<String>,
    /// New destination node ID — retargets the edge instead of recreating
    /// it, preserving its `edge_id` and any run history keyed against it.
    /// Must belong to the same spec/graph as the edge. Omit to leave
    /// the edge's target unchanged.
    pub to_node: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphDeleteEdgeParams {
    /// Existing edge ID to delete.
    pub edge_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphDeleteNodeParams {
    /// Existing node ID to delete. Cascades to every edge naming it as
    /// `from_node` or `to_node`. Rejected if the node is the graph's entry
    /// point.
    pub node_id: String,
}

/// One ensemble member: differs from its siblings by platform/model and,
/// optionally, its own prompt — see `graph_ensemble add`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnsembleMemberParams {
    /// CLI platform for this member (e.g. "claude", "openrouter").
    pub platform: String,
    /// Optional model override for this member.
    pub model: Option<String>,
    /// Optional prompt for this member only, replacing the ensemble's shared
    /// `prompt_template` — same placeholders (e.g. `{{spec_content}}`,
    /// `{{previous_feedback}}`), rendered the same way. Lets a panel review
    /// the same input from several angles (context, security, conventions...)
    /// in one ensemble instead of one prompt across different models. Omit to
    /// use the shared prompt, same as every member before this field existed.
    /// Independent of `platform`/`model`.
    pub prompt_override: Option<String>,
    /// This member's own agent timeout in minutes, overriding the
    /// ensemble's shared `timeout_minutes` for this member only. Omit to
    /// use the ensemble's `timeout_minutes`, same as every member before
    /// this field existed. Must not be negative; 0 is legitimate (an
    /// immediate timeout, same convention as the ensemble's own
    /// `timeout_minutes`).
    pub timeout_minutes: Option<i64>,
    /// CM30: this member's own infra-retry-limit override, overriding the
    /// ensemble's own default (which may itself defer to the platform's
    /// config.toml value, then the engine default of 2). Omit to use the
    /// ensemble's value. Must not be negative; 0 means "one attempt, no
    /// retry" — see graph_node add's infra_retry_limit doc.
    pub infra_retry_limit: Option<i64>,
    /// CM30: see infra_retry_limit. Overrides infra_crash_max_seconds
    /// (engine default 60).
    pub infra_crash_max_seconds: Option<i64>,
    /// CM30: see infra_retry_limit. Overrides infra_backoff_seconds
    /// (engine default 30).
    pub infra_backoff_seconds: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphUpdateEnsembleParams {
    /// Existing ensemble ID.
    pub ensemble_id: String,
    /// New ensemble kind (parallel/cascade/round_robin), or omit to leave unchanged.
    pub kind: Option<String>,
    /// New shared prompt, propagated to every current member.
    pub prompt_template: Option<String>,
    /// Replacement member list (2–8 for parallel, 2–16 for cascade/round_robin)
    /// — added/removed/replaced by
    /// position, always the full list (there is no way to patch a single
    /// member in place). Each entry may set its own `prompt_override`; a
    /// member without one uses the (possibly also-updated) shared
    /// `prompt_template`.
    pub members: Option<Vec<EnsembleMemberParams>>,
    /// CM49: bind this ensemble to a crew by name (`Some(name)`), or unbind
    /// it with `null` (`Some(None)`) — absent leaves the binding unchanged.
    /// Mutually exclusive with `members`. Binding replaces the member rows
    /// with the crew's and stamps the crew version; `null` keeps the current
    /// rows as literal members.
    #[serde(default, deserialize_with = "double_option")]
    pub crew: Option<Option<String>>,
    /// New pass threshold.
    pub min_pass: Option<i64>,
    /// New straggler timeout in minutes, or null to fall back to
    /// `timeout_minutes` again.
    pub straggler_timeout_minutes: Option<Option<i64>>,
    /// CM24: new quorum grace in minutes, or null to clear back to
    /// wait-for-all.
    pub quorum_grace_minutes: Option<Option<i64>>,
    /// New shared member agent timeout in minutes.
    pub timeout_minutes: Option<i64>,
    /// CM30: new ensemble-level infra-retry default, or null to clear back
    /// to deferring to the platform/engine default.
    pub infra_retry_limit: Option<Option<i64>>,
    /// CM30: see infra_retry_limit (infra_crash_max_seconds).
    pub infra_crash_max_seconds: Option<Option<i64>>,
    /// CM30: see infra_retry_limit (infra_backoff_seconds).
    pub infra_backoff_seconds: Option<Option<i64>>,
    /// New `pass` exit target node ID — or an ensemble ID to chain this
    /// ensemble's quorum directly into another ensemble (every member of the
    /// target gets a pass edge from this quorum, no intermediate node).
    pub on_pass_to: Option<String>,
    /// New `fail` exit target node ID, or null to clear it (dead end on
    /// fail). Accepts an ensemble ID like `on_pass_to`.
    pub on_fail_to: Option<Option<String>>,
    /// New entry source node ID. Replaces EVERY existing entry edge: after
    /// this call the ensemble is entered only from `from_node`. Never names
    /// member nodes — the fan-out to every member is rebuilt as one unit.
    pub from_node: Option<String>,
    /// Entry routing condition for `from_node`: pass, fail, or always.
    /// Omit to keep the ensemble's current entry condition.
    pub condition: Option<String>,
    /// Another entry source node ID. Adds entry edges from this node to
    /// every member while keeping the existing entries, so the ensemble can
    /// be entered from several places (e.g. a designer, a failing gate, and
    /// a reviewer bouncing back) with no relay node.
    pub add_entry_from: Option<String>,
    /// Entry routing condition for `add_entry_from`. Defaults to always.
    pub add_entry_condition: Option<String>,
    /// An entry source node ID to detach. Removes its entry edges to every
    /// member. Refused when it is the ensemble's last entry source — an
    /// ensemble always keeps at least one entry.
    pub remove_entry_from: Option<String>,
    /// New commit_rights value, or omit to leave unchanged.
    pub commit_rights: Option<bool>,
    /// New named mutex list (`["cpu"]`), `[]` to clear, or omit to leave
    /// unchanged. Each name must match `^[a-z][a-z0-9-]{0,31}$`.
    pub mutex: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphDeleteEnsembleParams {
    /// Existing ensemble ID. Removes the ensemble as one unit — its member
    /// nodes, its quorum node, and every edge naming any of them. Refused
    /// while the owning graph is running, like the other topology tools.
    pub ensemble_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphCopyNodeParams {
    /// Node to copy. Only its config is copied — never any runtime state
    /// (runs, iterations, statuses). Cannot be an ensemble member/quorum node
    /// (copy those with graph_ensemble copy).
    pub source_node_id: String,
    /// Target spec ID for the copy. Provide at most one of spec_id/graph_id;
    /// omit both to copy into the source node's own graph.
    pub spec_id: Option<String>,
    /// Target graph ID (the graph's top-level graph). Cross-graph copy is
    /// allowed. Provide at most one of spec_id/graph_id; omit both to copy into
    /// the source node's own graph.
    pub graph_id: Option<String>,
    /// New node name. Defaults to the source node's name.
    pub name: Option<String>,
    /// Config keys to shallow-merge over the copied config — e.g. swap an
    /// agent's prompt with {"prompt_template": "..."}, or override
    /// platform/model/timeout_minutes/command/value.
    pub config_overrides: Option<serde_json::Map<String, serde_json::Value>>,
    /// Optional incoming wiring: create an edge from this existing node in the
    /// target graph to the copy. Omit to leave the copy without an entry edge.
    pub entry_from_node: Option<String>,
    /// Condition for the entry edge (pass/fail/always). Defaults to always.
    /// Ignored unless entry_from_node is set.
    pub entry_condition: Option<String>,
    /// Optional outgoing edge: wire the copy to this node on a `pass` result.
    pub on_pass_to: Option<String>,
    /// Optional outgoing edge: wire the copy to this node on a `fail` result.
    pub on_fail_to: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphCopyEnsembleParams {
    /// Ensemble to copy. Members, join config, and shared prompt are copied
    /// (config only — never runtime state). Every id in the copy is new.
    pub source_ensemble_id: String,
    /// Target spec ID for the copy. Provide at most one of spec_id/graph_id;
    /// omit both to copy into the source ensemble's own graph.
    pub spec_id: Option<String>,
    /// Target graph ID (the graph's top-level graph). Cross-graph copy is
    /// allowed. Provide at most one of spec_id/graph_id; omit both to copy into
    /// the source ensemble's own graph.
    pub graph_id: Option<String>,
    /// New ensemble name. Defaults to the source name with a " (copy)" suffix.
    pub name: Option<String>,
    /// New shared prompt for every member — e.g. swap a proposer prompt for a
    /// review prompt. Defaults to the source's prompt.
    pub prompt_template: Option<String>,
    /// Replacement member list (2–8 parallel, 2–16 cascade/round_robin).
    /// Defaults to the source's members.
    pub members: Option<Vec<EnsembleMemberParams>>,
    /// New pass threshold. Defaults to the source's (clamped to the member
    /// count).
    pub min_pass: Option<i64>,
    /// New shared member agent timeout in minutes. Defaults to the source's.
    pub timeout_minutes: Option<i64>,
    /// New straggler timeout in minutes. Defaults to the source's.
    pub straggler_timeout_minutes: Option<i64>,
    /// New quorum grace in minutes. Defaults to the source's.
    pub quorum_grace_minutes: Option<i64>,
    /// Entry wiring override: the node the copy is wired from. Defaults to the
    /// source's entry node — required for a cross-graph copy where that node
    /// doesn't exist in the target.
    pub from_node: Option<String>,
    /// Entry routing condition (pass/fail/always). Defaults to the source's.
    pub condition: Option<String>,
    /// `pass` exit target node ID. Defaults to the source's — required for a
    /// cross-graph copy where the source's target doesn't exist there.
    pub on_pass_to: Option<String>,
    /// `fail` exit target node ID. Defaults to the source's (which may be
    /// none).
    pub on_fail_to: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueueCreateParams {
    /// Human-readable queue name.
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueueAddSpecParams {
    /// Existing queue ID.
    pub queue_id: String,
    /// Existing spec ID to add to the queue.
    pub spec_id: String,
    /// Optional context group. Specs sharing a group in the same queue reuse
    /// one warm harness session: a grouped spec resumes the session captured
    /// by the previous successfully-completed sibling in the group instead of
    /// re-analyzing the repo from cold. Omit for an independent, ungrouped
    /// member.
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueueListParams {
    /// Existing queue ID. Omit to list every queue (summary only, no members).
    pub queue_id: Option<String>,
    /// Include full spec descriptions in queue member listings. Default: false (compact — id, name, status, position, group only).
    pub include_descriptions: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueueRemoveSpecParams {
    /// Existing queue ID.
    pub queue_id: String,
    /// Spec ID to remove from the queue.
    pub spec_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphRemoveSpecParams {
    /// Existing graph ID.
    pub graph_id: String,
    /// Spec ID to unbind from the graph.
    pub spec_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueueReorderParams {
    /// Existing queue ID.
    pub queue_id: String,
    /// Full list of spec IDs currently in the queue, in the desired final
    /// order. Must be a total permutation of the queue's current members —
    /// every spec id exactly once.
    pub spec_ids: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct QueueSetGroupParams {
    /// Existing queue ID.
    pub queue_id: String,
    /// Existing queue member's spec ID.
    pub spec_id: String,
    /// Context group to assign. Omit or pass `null` to clear (ungroup) the
    /// member.
    #[serde(default)]
    pub group: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphGetParams {
    /// Graph ID.
    pub graph_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphExportParams {
    /// Graph ID to export.
    pub graph_id: String,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphImportParams {
    /// The exported graph document (the object `graph` returns).
    #[schemars(schema_with = "arbitrary_json_value_schema")]
    pub document: serde_json::Value,
    /// Absolute workdir for the new graph.
    pub workdir: String,
    /// Graph name to use instead of the document's own `name`.
    pub name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GraphInstallParams {
    /// Catalogue ref: `<slug>` or `<owner>/<repo>/<slug>`, each with an
    /// optional `@<ref>` suffix selecting a version — or `draft:<id>` for
    /// an editor draft.
    #[serde(rename = "ref")]
    pub reference: String,
    /// Absolute workdir for the new graph.
    pub workdir: String,
    /// Graph name to use instead of the document's own `name`.
    pub name: Option<String>,
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
pub struct GraphValidateParams {
    /// Path to a graph or node JSON document to validate. Provide exactly
    /// one of `path` or `document`.
    pub path: Option<String>,
    /// The graph or node document itself. Provide exactly one of `path` or
    /// `document`.
    #[schemars(schema_with = "arbitrary_json_value_schema")]
    pub document: Option<serde_json::Value>,
}
