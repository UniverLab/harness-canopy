use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::models::{Trigger, WatchEvent};
pub use crate::domain::spec_type::SpecType;

#[allow(unused_imports)]
pub use crate::domain::specs::validate_spec_description_template;

mod crew;
mod ensembles;
mod hooks;
mod statuses;
mod topology;

pub use crew::{
    group_graph_crew_rows, summarize_graph_crew_rows, CrewRowKind, GraphCrewGroup, GraphCrewRow,
    GraphCrewSummary,
};
pub use ensembles::{
    member_bounds, member_cap_key, member_count_message, Ensemble, EnsembleDetails, EnsembleKind,
    EnsembleMember, EnsembleMemberSpec, MemberCaps,
};
pub use hooks::{GraphCompletionHook, GraphCompletionHookRun, GraphHookEvent};
pub use statuses::{
    ArchiveGraphOutcome, GraphResetOutcome, GraphRunStatus, GraphSpecStatus, GraphStatus,
    SpecAdminStatusOutcome,
};
pub use topology::{GraphEdgeCondition, GraphNodeKind};

/// Minimum/maximum number of routes a [`GraphNodeKind::Router`] node may
/// declare.
pub const ROUTER_MIN_ROUTES: usize = 2;
pub const ROUTER_MAX_ROUTES: usize = 8;

/// One route a router node can select: a short label plus a one-line
/// description of when to take it. Declared in the node's `config` (the
/// `routes` array) and referenced by an edge's [`GraphEdgeCondition::Route`]
/// label — never persisted separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouterRoute {
    pub label: String,
    pub description: String,
}

/// Validate a router node's declared routes + fallback shape: `2`-`8`
/// routes, unique non-empty labels each with a non-empty description, and a
/// `fallback` that names one of them. Pure config-shape check — it doesn't
/// need the graph's edges, so it applies equally at node add and update
/// time (see `daemon::handler::validate_node_config`'s `Router` arm).
pub fn validate_router_routes(routes: &[RouterRoute], fallback: &str) -> Result<(), String> {
    if routes.len() < ROUTER_MIN_ROUTES {
        return Err(format!(
            "A router must declare at least {ROUTER_MIN_ROUTES} routes, got {}.",
            routes.len()
        ));
    }
    if routes.len() > ROUTER_MAX_ROUTES {
        return Err(format!(
            "A router must declare at most {ROUTER_MAX_ROUTES} routes, got {}.",
            routes.len()
        ));
    }

    let mut seen_labels = std::collections::HashSet::new();
    for route in routes {
        if route.label.trim().is_empty() {
            return Err("Every router route must have a non-empty label.".to_string());
        }
        if route.description.trim().is_empty() {
            return Err(format!(
                "Router route '{}' must have a non-empty description.",
                route.label
            ));
        }
        if !seen_labels.insert(route.label.as_str()) {
            return Err(format!(
                "Router route label '{}' is declared more than once.",
                route.label
            ));
        }
    }

    if fallback.trim().is_empty() {
        return Err("A router must declare one route as fallback.".to_string());
    }
    if !routes.iter().any(|route| route.label == fallback) {
        return Err(format!(
            "Router fallback '{fallback}' does not name a declared route."
        ));
    }

    Ok(())
}

/// Validate that every `route`-conditioned edge out of `node_id` names a
/// route actually declared by `routes` — called whenever a router's routes
/// list or its edges change, so an edge can never be left pointing at a
/// route that no longer exists.
pub fn validate_router_edges_declared(
    routes: &[RouterRoute],
    node_id: &str,
    edges: &[GraphEdge],
) -> Result<(), String> {
    for edge in edges.iter().filter(|edge| edge.from_node == node_id) {
        if let Some(label) = edge.condition.route_label() {
            if !routes.iter().any(|route| route.label == label) {
                return Err(format!(
                    "Edge '{}' names undeclared route '{label}'.",
                    edge.id
                ));
            }
        }
    }
    Ok(())
}

/// Validate that every declared route already has at least one outgoing
/// `route` edge from `node_id` — but only once the router has begun being
/// wired (at least one `route` edge already exists from it). A brand new
/// router with no edges at all is a valid, not-yet-wired state; once wiring
/// starts, every declared route must be covered.
pub fn validate_router_route_coverage(
    routes: &[RouterRoute],
    node_id: &str,
    edges: &[GraphEdge],
) -> Result<(), String> {
    let route_edges: Vec<&GraphEdge> = edges
        .iter()
        .filter(|edge| edge.from_node == node_id && edge.condition.route_label().is_some())
        .collect();
    if route_edges.is_empty() {
        return Ok(());
    }
    for route in routes {
        let served = route_edges
            .iter()
            .any(|edge| edge.condition.route_label() == Some(route.label.as_str()));
        if !served {
            return Err(format!(
                "Router route '{}' has no outgoing edge.",
                route.label
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Graph {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub workdir: String,
    pub status: GraphStatus,
    /// Optional automatic trigger. Reuses the agent [`Trigger`] model so a graph
    /// can fire on a cron schedule or a file-system watch, exactly like an
    /// agent. `None` means the graph is manual-only (`graph_run`).
    #[serde(default)]
    pub trigger: Option<Trigger>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// One-shot resume schedule: when set and reached, the scheduler starts
    /// this graph once (e.g. `run_graph`) and clears the field. Unlike
    /// `trigger`'s cron, this never repeats — it exists so a graph that fails
    /// on a quota can reschedule its own resumption at the exact reset time
    /// instead of relying on a blindly polling cron.
    #[serde(default)]
    pub autorun_at: Option<DateTime<Utc>>,
    /// One-shot deferred resume for a *paused* graph: when set and reached
    /// while the graph is still `Paused`, the scheduler fires the equivalent
    /// of `graph_continue` (see `auto_continue_action`) — preserving the
    /// paused cursor/context — rather than `autorun_at`'s reset-and-relaunch.
    /// Lets a user pause a graph to stop burning quota now and have it pick
    /// back up automatically at a later time, without a human calling
    /// `graph_continue`. Deliberately a separate field from `autorun_at`
    /// rather than a shared one: the two fire through entirely different
    /// paths (`resume_background` alone vs. auto-reset-then-`resume_background`)
    /// and must never be conflated. See `is_auto_continue_due`.
    #[serde(default)]
    pub auto_continue_at: Option<DateTime<Utc>>,
    /// The `graph_continue` action (`"retry_current_node"` or
    /// `"skip_next_spec"`) to apply when `auto_continue_at` fires. `None`
    /// (or any value other than `"skip_next_spec"`) defaults to
    /// `retry_current_node` — see [`crate::scheduler::cron_scheduler`]'s
    /// auto-continue fire branch.
    #[serde(default)]
    pub auto_continue_action: Option<String>,
    /// The queue a run against this graph is currently — or most recently —
    /// drew from, persisted the moment that run starts (`None` for a
    /// bound-spec run). Interrupted runs (a quota failure, a daemon restart)
    /// leave this set so every resume path — scheduled autorun, `graph_reset`
    /// — knows which queue to pick up rather than falling back to the graph's
    /// (often empty) bound specs. It survives genuine completion too (B31),
    /// giving a finished queue-driven graph the only link back to the queue it
    /// ran so `graph list` / `graph info` can render its real `n/n` progress
    /// instead of `0/0`. A stale value never pollutes a later run: every
    /// launch path overwrites this field before the first spec executes, so
    /// a fresh `graph_run` against a different queue (or a bound-spec run,
    /// which writes `None`) replaces it.
    #[serde(default)]
    pub active_run_queue_id: Option<String>,
    /// Event-keyed hooks: an ordered map from event name to the list of
    /// hooks registered for that event. Each hook is an agent-node-style
    /// config (platform/model/effort/prompt/timeout_minutes), a shell
    /// command, or an interactive message (prompt/target_session_id) that is
    /// delivered asynchronously — due immediately, never waiting for a
    /// reply. An empty map preserves pre-N2 behavior exactly (no hooks
    /// fire). See
    /// [`crate::graph_engine::GraphEngine`]'s `run_graph_dispatch` for where
    /// events fire and `render_hook_prompt` for placeholders.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hooks: BTreeMap<GraphHookEvent, Vec<GraphCompletionHook>>,
    /// Archived graphs leave every browsing listing (sidebar, `canopy graph
    /// list`, MCP `graph_list`) but keep their row, specs, and full run
    /// history — a deliberate, reversible, always-counted alternative to
    /// permanent deletion. `false` for every pre-existing graph after
    /// migration. See `Database::{archive_graph, restore_graph}`.
    #[serde(default)]
    pub archived: bool,
    /// Set by `reconcile_orphaned_graphs` (and only by it) the moment it pauses
    /// a graph that was left `Running` by an unclean daemon exit — never by an
    /// operator's `graph_pause`/`graph_report_blocker`, both of which go
    /// through `Database::update_graph_status`, which clears this on every
    /// call. Lets [`Self::is_autorun_due`] tell the two kinds of `Paused`
    /// apart: a pending `autorun_at` must survive a reconciliation pause
    /// (nobody asked for the graph to stop), but must not fire on a pause the
    /// operator actually asked for.
    #[serde(default)]
    pub paused_by_reconciliation: bool,
    /// Optional pre-wired target for infrastructure failures (`Error` edges).
    /// When set, every new agent/check/gate node added to this graph
    /// auto-creates a `Error` edge to this node, so a graph with no
    /// explicit infrastructure edge still has fail coverage by construction.
    /// `None` preserves pre-CM2 behavior — no auto-wiring, no `Error` edges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub infra_node_id: Option<String>,
    /// CM29: when true, `graph_run`/autorun downgrade the dirty-start
    /// refusal to a warning instead of refusing to launch. `false` for
    /// every pre-existing graph (never silently permissive). Set through
    /// `graph_update`.
    #[serde(default)]
    pub allow_dirty_start: bool,
    /// SPECCODE: opt-in enforcement. When true, `graph_add_spec` (and a
    /// queue add / run that would feed one) refuses a spec whose type is
    /// still `None`, named by id, so a graph can demand the conventional
    /// data its deterministic commit node needs. Default `false` — legacy
    /// specs keep working everywhere else.
    #[serde(default)]
    pub require_spec_type: bool,
}

impl Graph {
    /// Short label for the graph's trigger: `"cron"`, `"watch"`, or `"manual"`.
    pub fn trigger_type_label(&self) -> &'static str {
        match &self.trigger {
            Some(Trigger::Cron { .. }) => "cron",
            Some(Trigger::Watch { .. }) => "watch",
            None => "manual",
        }
    }

    /// The cron expression when this graph is cron-triggered.
    pub fn schedule_expr(&self) -> Option<&str> {
        match &self.trigger {
            Some(Trigger::Cron { schedule_expr }) => Some(schedule_expr),
            _ => None,
        }
    }

    /// The watched path when this graph is watch-triggered.
    pub fn watch_path(&self) -> Option<&str> {
        match &self.trigger {
            Some(Trigger::Watch { path, .. }) => Some(path),
            _ => None,
        }
    }

    #[allow(dead_code)]
    pub fn watch_events(&self) -> Option<&[WatchEvent]> {
        match &self.trigger {
            Some(Trigger::Watch { events, .. }) => Some(events),
            _ => None,
        }
    }

    pub fn is_cron(&self) -> bool {
        matches!(&self.trigger, Some(Trigger::Cron { .. }))
    }

    pub fn is_watch(&self) -> bool {
        matches!(&self.trigger, Some(Trigger::Watch { .. }))
    }

    /// Whether a triggered graph is currently eligible to start a fresh run.
    ///
    /// A graph that is already `Running` or `Paused` must not be re-launched by
    /// its trigger — that would spawn a duplicate execution over the same
    /// graph. Draft/Completed/Failed graphs are fireable (a scheduled graph
    /// re-runs its graph on each cron slot / watch event).
    pub fn is_fireable(&self) -> bool {
        !matches!(
            self.status,
            GraphStatus::Running | GraphStatus::Pausing | GraphStatus::Paused
        )
    }

    /// Whether this graph's one-shot `autorun_at` schedule is due at `now`.
    ///
    /// True when `autorun_at` is set, `now` has reached it, and either the
    /// graph is fireable (not `Running`/`Paused`) or it is `Paused` *because
    /// `reconcile_orphaned_graphs` put it there* after an unclean daemon exit.
    /// That second branch is deliberate and narrow: an operator-requested
    /// pause (`graph_pause`, `graph_report_blocker`) must still block the
    /// schedule — [`Self::is_fireable`] is unchanged and still says so for
    /// every other caller — but a pause reconciliation imposed on the graph's
    /// behalf must not silently swallow a schedule nobody asked to cancel.
    /// Firing must clear `autorun_at` so it never fires twice.
    pub fn is_autorun_due(&self, now: DateTime<Utc>) -> bool {
        self.autorun_at.is_some_and(|at| now >= at)
            && (self.is_fireable()
                || (self.status == GraphStatus::Paused && self.paused_by_reconciliation))
    }

    /// Whether `auto_continue_at` has been reached at `now`, independent of
    /// status. Used by the scheduler to decide when a schedule is stale (the
    /// graph left `Paused` some other way before firing) and must be cleared
    /// even though it won't actually resume the graph — see
    /// [`Self::is_auto_continue_due`] for the status-gated check that decides
    /// whether to fire.
    pub fn is_auto_continue_time_reached(&self, now: DateTime<Utc>) -> bool {
        self.auto_continue_at.is_some_and(|at| now >= at)
    }

    /// Whether this graph's one-shot `auto_continue_at` schedule should
    /// actually fire a deferred `graph_continue` at `now`.
    ///
    /// Deliberately Paused-only — unlike [`Self::is_autorun_due`], which is
    /// due on any *fireable* (non-`Running`/`Paused`) status. Deferring a
    /// resume only makes sense while the graph is sitting `Paused`; if it left
    /// that state some other way (manual `graph_continue`, failure) before the
    /// scheduled time, the schedule is stale — the scheduler clears it
    /// without firing rather than waiting here for `Paused` to recur.
    pub fn is_auto_continue_due(&self, now: DateTime<Utc>) -> bool {
        self.auto_continue_at.is_some_and(|at| now >= at) && self.status == GraphStatus::Paused
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSpec {
    pub id: String,
    /// The graph this spec has been assigned to. `None` means the spec is a
    /// standalone backlog item — authored ahead of time, not yet queued into
    /// any graph's run.
    pub graph_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub position: i64,
    pub parallelizable: bool,
    pub status: GraphSpecStatus,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// The graph's workdir `git rev-parse HEAD`, captured once when this spec
    /// starts running (not per node). Lets `check` nodes verify "did this
    /// spec commit anything?" via `{{spec_start_head}}` without relying on
    /// state outside the spec row (e.g. a file marker) that would survive a
    /// daemon restart and produce false positives. `None` when the workdir
    /// isn't a git repo or the spec hasn't started yet.
    #[serde(default)]
    pub spec_start_head: Option<String>,
    /// Number of non-ignored git status entries captured when the spec began.
    /// `None` is unknown for legacy rows and non-git workdirs.
    #[serde(default)]
    pub spec_start_dirty: Option<i64>,
    /// Number of non-ignored git status entries at the moment this spec's
    /// attempt actually ended (Completed, Failed, or Interrupted) — same
    /// `git status --porcelain` count as `spec_start_dirty`, captured once
    /// more at the other boundary. `None` for legacy rows, non-git workdirs,
    /// or a spec that hasn't ended yet.
    #[serde(default)]
    pub spec_end_dirty: Option<i64>,
    /// The first 20 paths from that same `git status --porcelain` output,
    /// in the order git reported them. `None` alongside `spec_end_dirty ==
    /// None`; `Some(vec![])` is impossible (a dirty count of 0 never
    /// persists paths — see `dirty_tree_note`).
    #[serde(default)]
    pub spec_end_dirty_paths: Option<Vec<String>>,
    /// The workdir's git HEAD immediately after a `commit_rights: true`
    /// node's own execution actually moved it during this attempt (C15).
    /// Unlike `spec_start_head` — which only proves *some* commit landed
    /// since the spec began, and is satisfied just as well by a concurrent
    /// commit from outside this run sharing the same worktree — this is set
    /// only when a node the graph explicitly trusts to commit is the one
    /// whose execution moved HEAD, so `check` nodes can verify "did *this
    /// run's own committer* land a commit" via `{{spec_committed_head}}`.
    /// `None` until such a node commits; overwritten (not accumulated) each
    /// time one does, so it always reflects the latest commit this attempt
    /// itself produced. Requires the graph to name a committer
    /// (`commit_rights: true`) — a graph that never does never populates it.
    #[serde(default)]
    pub spec_committed_head: Option<String>,
    /// Optional workdir tag for backlog filtering only (`spec_list`). It does
    /// not drive execution — the run that eventually assigns this spec to a
    /// graph decides the actual workdir.
    #[serde(default)]
    pub workdir: Option<String>,
    /// How the spec was last transitioned to its current status (`admin` for
    /// administrative transitions). `None` means the status change was
    /// engine-driven or the spec was never administratively touched.
    #[serde(default)]
    pub completed_via: Option<String>,
    /// Reason for the most recent administrative status transition.
    #[serde(default)]
    pub completed_via_reason: Option<String>,
    /// Timestamp of the most recent administrative status transition.
    #[serde(default)]
    pub completed_via_at: Option<DateTime<Utc>>,
    /// SPECCODE: the Conventional Commits type this spec is being delivered
    /// as (`feat`, `fix`, ...). `None` on legacy specs and specs authored
    /// without a type — they have no code and no precomputed commit message.
    #[serde(default, rename = "type")]
    pub spec_type: Option<SpecType>,
    /// SPECCODE: optional scope (`tui`, `specs`, ...) of that change type.
    #[serde(default)]
    pub spec_scope: Option<String>,
    /// SPECCODE: engine-generated quick reference (`FIX-7`), immutable once
    /// assigned — assigned on create when the type is known, or by
    /// `spec_update` assigning a type to a legacy spec. Never set by callers
    /// and never part of the commit message.
    #[serde(default)]
    pub spec_code: Option<String>,
}

/// A node in either a spec's graph or a graph's top-level graph.
///
/// Exactly one of `spec_id`/`graph_id` is set — enforced by the DB layer (see
/// [`crate::db::Database::insert_graph_node`]) rather than by this type, since
/// callers build a `GraphNode` before it has been validated against the DB.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub spec_id: Option<String>,
    pub graph_id: Option<String>,
    pub name: String,
    pub kind: GraphNodeKind,
    pub config: Value,
    pub position: i64,
    pub created_at: DateTime<Utc>,
}

/// An edge in either a spec's graph or a graph's top-level graph.
///
/// Exactly one of `spec_id`/`graph_id` is set — same invariant as
/// [`GraphNode`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdge {
    pub id: String,
    pub spec_id: Option<String>,
    pub graph_id: Option<String>,
    pub from_node: String,
    pub to_node: String,
    pub condition: GraphEdgeCondition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeRun {
    pub id: String,
    pub graph_id: String,
    pub spec_id: String,
    pub node_id: String,
    pub status: GraphRunStatus,
    pub input: Option<Value>,
    pub output: Option<Value>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub iteration: i64,
    /// PID of the OS process group currently executing this node run (the
    /// spawned child is always its own process-group leader — see
    /// `process_group(0)` at the spawn sites), so the engine can `killpg` it
    /// on any abnormal end. `None` once the run is finalized, or if no
    /// process was ever spawned for it (e.g. a gate node).
    pub pid: Option<i64>,
    /// `system::boot_id()` at the moment `pid` was recorded. A PID alone
    /// can't tell a live survivor from an unrelated process that reused the
    /// same PID after a reboot recycled the PID space — only meaningful
    /// together with a matching current boot id. See B12.
    pub boot_id: Option<String>,
    /// The harness session id that served this node run, captured per
    /// platform metadata (RS1): generated and set at spawn for platforms
    /// that accept a caller-chosen id (`session_id_set_flag`), or read back
    /// from the platform's session listing after the run
    /// (`session_list_cmd`). `None` for platforms that expose no session
    /// identity, and for every run recorded before this field existed. The
    /// foundation for resume mode (RS2): without it there is nothing to
    /// resume.
    pub session_id: Option<String>,
    /// CB43: platform/model resolved at dispatch time — the CLI name that
    /// `run_agent_process` actually used and the model string actually handed
    /// to the CLI argv (`None` when no model was requested or the platform's
    /// `model_flag` cannot select one). Never derived from the node's current
    /// config at read time; `None` for pre-migration rows and non-agent nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_platform: Option<String>,
    /// CB43: see `executed_platform`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSpecDetails {
    pub spec: GraphSpec,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphDetails {
    pub lp: Graph,
    /// The top-level graph: nodes/edges that target the graph directly
    /// (`graph_id`) rather than any one spec. Defined once per graph instead of
    /// being repeated across every spec.
    pub graph_nodes: Vec<GraphNode>,
    pub graph_edges: Vec<GraphEdge>,
    pub specs: Vec<GraphSpecDetails>,
    /// Every past hook firing (oldest first) — populated only when the graph
    /// has fired at least one hook.
    pub completion_hook_runs: Vec<GraphCompletionHookRun>,
}

#[cfg(test)]
mod tests;
