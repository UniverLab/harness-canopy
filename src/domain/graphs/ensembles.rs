use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::graphs::GraphEdgeCondition;

/// An ensemble (F1): a group of agent-node members, run in parallel, plus
/// the join gate that waits for every member, consolidates their outputs,
/// and routes onward. Members share one prompt by default but may each carry
/// their own `prompt_override` (see [`EnsembleMember::prompt_override`]) so a
/// panel can review the same input from several angles at once instead of
/// just several models. Persisted as
/// its own row so `graph_get`/`graph_update_ensemble` can address the whole
/// unit — the members and join themselves are ordinary [`GraphNode`] rows
/// (see [`EnsembleMember`]), wired with ordinary [`GraphEdge`] rows, so the
/// engine's existing graph-walking code needs only the ensemble-aware
/// fan-out/fan-in added in `graph_engine`.
///
/// The execution strategy for an ensemble — how members are selected and
/// how the ensemble's own pass/fail is determined from their results.
///
/// - `Parallel`: every member runs concurrently; the join waits for all and
///   counts passes against `min_pass`. The original (and default) type.
/// - `Cascade`: members are tried in `position` order; the first one that
///   produces a usable result (not an infra crash) wins and the rest never
///   run. Quota savings is the reason this type exists.
/// - `RoundRobin`: invocations rotate across members by position, spreading
///   quota consumption. Requires persistent state (`round_robin_index`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnsembleKind {
    #[default]
    Parallel,
    Cascade,
    RoundRobin,
}

impl EnsembleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnsembleKind::Parallel => "parallel",
            EnsembleKind::Cascade => "cascade",
            EnsembleKind::RoundRobin => "round_robin",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "parallel" => Some(EnsembleKind::Parallel),
            "cascade" => Some(EnsembleKind::Cascade),
            "round_robin" => Some(EnsembleKind::RoundRobin),
            _ => None,
        }
    }
}

/// Every ensemble kind requires at least two members.
pub const ENSEMBLE_MIN_MEMBERS: usize = 2;

/// Per-kind member-count bounds. `parallel` runs every member concurrently,
/// so it stays at 2–8; `cascade` and `round_robin` dispatch one member at a
/// time, so a longer list only adds rotation/fallback depth (not concurrency
/// or cost) and they allow 2–16.
pub fn member_bounds(kind: EnsembleKind) -> (usize, usize) {
    match kind {
        EnsembleKind::Parallel => (ENSEMBLE_MIN_MEMBERS, 8),
        EnsembleKind::Cascade | EnsembleKind::RoundRobin => (ENSEMBLE_MIN_MEMBERS, 16),
    }
}

/// Exactly one of `spec_id`/`graph_id` is set — same invariant as
/// [`GraphNode`]/[`GraphEdge`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ensemble {
    pub id: String,
    pub spec_id: Option<String>,
    pub graph_id: Option<String>,
    pub name: String,
    /// The one shared prompt every member renders against — supports the
    /// same placeholders as an agent node's `prompt_template`.
    pub prompt_template: String,
    /// The engine-executed [`GraphNodeKind::Join`] node that waits for every
    /// member and consolidates their outputs.
    pub join_node_id: String,
    /// The node this ensemble is wired from — every member gets an incoming
    /// edge from this node with `entry_condition`.
    pub entry_from_node: String,
    pub entry_condition: GraphEdgeCondition,
    /// Members required to pass for the join to report `pass`. Defaults to
    /// every member (set at creation to `members.len()`).
    pub min_pass: i64,
    /// Minutes a member may run before the join kills it (B12) and counts it
    /// as failed. `None` means "use `timeout_minutes`" (the members' own
    /// agent timeout) — see [`Self::effective_straggler_timeout_minutes`].
    pub straggler_timeout_minutes: Option<i64>,
    /// CM24: when set and `kind == Parallel`, once `passed >= min_pass` wait
    /// this many minutes for remaining members to finish before terminating
    /// them with reason "quorum met". `None` keeps the old wait-for-every-member
    /// behaviour. `0` means terminate immediately on quorum. Cascade and
    /// round_robin ignore it.
    pub quorum_grace_minutes: Option<i64>,
    /// Shared agent timeout (minutes) applied to every member's node config.
    pub timeout_minutes: i64,
    // Ensemble — CM30: ensemble-level default infra-retry override, applied
    // to every member unless the member sets its own (see EnsembleMember).
    // `None` means "defer further down the chain" — NOT "use the engine
    // default": a member could still override, or the platform could still
    // apply. Mirrors `straggler_timeout_minutes`'s "None = defer" shape,
    // unlike `timeout_minutes` which is never optional.
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
    /// Join-node outgoing routing: where a `pass`/`fail` join result routes
    /// to next. `on_pass_to` is required at creation; `on_fail_to` is
    /// optional (a dead end on fail, same as any other node with no
    /// matching outgoing edge).
    pub on_pass_to: String,
    pub on_fail_to: Option<String>,
    pub kind: EnsembleKind,
    pub round_robin_index: Option<i64>,
    /// CM28: whether this ensemble is the graph's designated committer
    /// (B37) — whichever member the strategy (parallel/cascade/round_robin)
    /// runs for a given dispatch is the committer for that dispatch. A
    /// per-member flag is deliberately never stored: the right belongs to
    /// the role (the ensemble), not to whichever CLI happens to fill it.
    pub commit_rights: bool,
    pub created_at: DateTime<Utc>,
    /// CM49: the crew this ensemble is bound to, by id — the join key every
    /// binding query uses. `None` means literal members (or a pending import
    /// that named a crew not yet created locally, see `crew_name`).
    pub crew_id: Option<String>,
    /// CM49: the binding's crew name. Persisted so a pending import (a bound
    /// ensemble whose crew does not exist locally yet) keeps the name it must
    /// dispatch against once the crew is created; kept current on rename for
    /// display.
    pub crew_name: Option<String>,
    /// CM49: the crew version whose members are currently materialized in
    /// `ensemble_members`. The lazy sync rewrites the rows and advances this
    /// when it is behind the crew's `version`.
    pub crew_version_applied: Option<i64>,
}

impl Ensemble {
    /// The straggler kill timeout to actually use: the explicit override, or
    /// (by default) the members' own agent timeout.
    pub fn effective_straggler_timeout_minutes(&self) -> i64 {
        self.straggler_timeout_minutes
            .unwrap_or(self.timeout_minutes)
    }
}

/// `(platform, model, prompt_override, timeout_minutes, infra_retry_limit,
/// infra_crash_max_seconds, infra_backoff_seconds)` — the normalized shape
/// of one ensemble member's identity, shared by `graph_add_ensemble`/
/// `graph_update_ensemble`'s validated input, [`EnsembleBlueprint`]'s stored
/// members, and [`EnsembleMember`] itself. `timeout_minutes` is this
/// member's own timeout override, or `None` to use the ensemble's shared
/// `timeout_minutes`. `infra_retry_limit`, `infra_crash_max_seconds` and
/// `infra_backoff_seconds` are this member's own infra-retry overrides, or
/// `None` to use the ensemble's own default (which may itself defer to the
/// platform's config.toml value, then the engine default).
///
/// [`EnsembleBlueprint`]: crate::domain::blueprints::EnsembleBlueprint
pub type EnsembleMemberSpec = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// One member of an [`Ensemble`] — differs from its siblings in
/// `platform`/`model` and, optionally, its own prompt; `node_id` points at
/// the underlying [`GraphNodeKind::Agent`] row that actually executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsembleMember {
    pub ensemble_id: String,
    pub node_id: String,
    /// Position within the ensemble (0-based) — the deterministic order used
    /// for consolidation and for keying resize diffs in
    /// `graph_update_ensemble`.
    pub position: i64,
    pub platform: String,
    pub model: Option<String>,
    /// This member's own prompt, replacing the ensemble's shared
    /// `prompt_template` for this member only — same placeholders, rendered
    /// the same way. `None` (the default) means the member renders the
    /// shared template exactly as every ensemble did before this field
    /// existed. Independent of `platform`/`model`: an override never changes
    /// which CLI/model runs it.
    pub prompt_override: Option<String>,
    /// This member's own agent timeout in minutes, overriding the
    /// ensemble's shared `timeout_minutes` for this member only. `None`
    /// (the default) means "use the ensemble's `timeout_minutes`", exactly
    /// as every member behaved before this field existed.
    pub timeout_minutes: Option<i64>,
    // EnsembleMember — CM30: this member's own infra-retry override,
    // overriding the ensemble's own default for this member only. `None`
    // means "use the ensemble's value" (which may itself be `None`, meaning
    // "use the platform's/engine's"). Same shape as `timeout_minutes` above.
    pub infra_retry_limit: Option<i64>,
    pub infra_crash_max_seconds: Option<i64>,
    pub infra_backoff_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsembleDetails {
    pub ensemble: Ensemble,
    /// Members in `position` order — the order consolidation and
    /// `graph_update_ensemble` resize diffs rely on.
    pub members: Vec<EnsembleMember>,
}
