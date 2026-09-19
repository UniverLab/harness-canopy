use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::models::{Trigger, WatchEvent};

#[allow(unused_imports)]
pub use crate::domain::specs::validate_spec_description_template;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphStatus {
    Draft,
    Running,
    /// Pause requested, waiting for the current node to finish naturally.
    Pausing,
    Paused,
    Completed,
    Failed,
}

impl GraphStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Running => "running",
            Self::Pausing => "pausing",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "pausing" => Self::Pausing,
            "paused" => Self::Paused,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Draft,
        }
    }
}

/// Result of [`crate::db::Database::reset_graph`] — the single state-transition
/// path used by both the `graph_reset` MCP tool and the scheduler's
/// auto-reset-and-resume of a `failed` graph on autorun.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphResetOutcome {
    NotFound,
    /// A node run is still `running` under this graph, regardless of what the
    /// graph's own `status` column says — resetting underneath it would
    /// corrupt its in-flight state (or race a stale completion against the
    /// dispatch the reset launches). Named so the caller's next move is an
    /// informed wait or a deliberate kill, not a blind retry.
    InFlight {
        run_id: String,
        node_id: String,
        started_at: DateTime<Utc>,
    },
    /// One of the explicitly requested `specs` doesn't belong to this graph.
    InvalidSpec(String),
    Reset {
        spec_count: usize,
        /// Number of administratively skipped specs preserved by a blanket
        /// reset. Explicit resets report zero because they intentionally
        /// reopen every named target.
        skipped_count: usize,
    },
}

/// Outcome of [`crate::db::Database::archive_graph`] — mirrors
/// [`GraphResetOutcome`]'s shape (an explicit outcome enum rather than a bare
/// bool/error) so the caller can render a precise message for each refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveGraphOutcome {
    NotFound,
    /// A `running` graph must be paused first — archiving is for work that's
    /// finished with.
    Running,
    AlreadyArchived,
    Archived,
}

#[derive(Debug, Clone)]
pub enum SpecAdminStatusOutcome {
    Success,
    NotFound,
    /// Spec is bound to a graph (not standalone); spec_set_status only
    /// administers standalone specs.
    NotStandalone(String),
    /// Spec has an active run and cannot be administratively transitioned.
    ActiveRun {
        graph_id: String,
        run_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphSpecStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
    /// The run working this spec was cut short by something external to the
    /// spec — a daemon restart, a machine crash, an unrelated process dying —
    /// not by the work itself failing. Set only by reconciliation, in place
    /// of the `git stash` it used to run: the working tree is left exactly
    /// as the interrupted attempt left it, and the spec is picked up again
    /// like any other runnable spec (queue selection treats this exactly
    /// like `Pending`), with its node prompt telling the agent a prior
    /// attempt exists so it continues rather than restarts.
    Interrupted,
}

impl GraphSpecStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Interrupted => "interrupted",
        }
    }

    /// Infallible: an unrecognized value (e.g. a status written by a newer
    /// binary that a caller on an older binary doesn't know) must never be
    /// silently read as `Completed` — that would let stale/incoming work
    /// skip execution entirely. Falling back to `Pending` is the safe
    /// choice: worst case, a spec re-runs a step it didn't need to.
    pub fn from_str(value: &str) -> Self {
        match value {
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "skipped" => Self::Skipped,
            "interrupted" => Self::Interrupted,
            _ => Self::Pending,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphNodeKind {
    Agent,
    Check,
    Gate,
    /// Engine-managed quorum node for an ensemble (F1) — never created via
    /// `graph_add_node` directly, only as part of `graph_add_ensemble`'s
    /// one-call expansion. Waits for every member branch to terminate,
    /// consolidates their outputs, and routes onward. See
    /// [`crate::graph_engine::GraphEngine`]'s ensemble fan-out handling.
    Join,
    /// A branch point that declares 2-8 named routes (see [`RouterRoute`])
    /// instead of the binary pass/fail an agent/check/gate node produces.
    /// Model, persistence and validation only in this iteration — the engine
    /// never executes a router (see
    /// [`crate::graph_engine::GraphEngine::execute_node`]'s `Router` arm).
    Router,
}

impl GraphNodeKind {
    /// Serde/DB-safe kind string. Unchanged for all variants (keeps
    /// existing DB rows and `from_str` parsing intact).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Check => "check",
            Self::Gate => "gate",
            Self::Join => "join",
            Self::Router => "router",
        }
    }

    /// User-facing label for the node kind. Returns `"quorum"` for
    /// `Join` so user-facing surfaces (TUI, CLI, MCP descriptions)
    /// display the intent rather than the internal enum name.
    pub fn display_str(self) -> &'static str {
        match self {
            Self::Join => "quorum",
            other => other.as_str(),
        }
    }

    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "agent" => Some(Self::Agent),
            "check" => Some(Self::Check),
            "gate" => Some(Self::Gate),
            "join" => Some(Self::Join),
            "router" => Some(Self::Router),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphEdgeCondition {
    Pass,
    Fail,
    Always,
    /// Router-only: this edge is taken when the router selects the named
    /// route. The label must match one of the router node's declared
    /// [`RouterRoute`]s — validated at edge add/update time (see
    /// `daemon::handler`'s `graph_add_edge`/`graph_update_edge`), never
    /// accepted free-form.
    Route(String),
    /// The node produced no verdict (infrastructure failure — crash,
    /// timeout, empty response). Emitted by the engine itself after retry
    /// exhaustion, never by a router. Falls back to [`Self::Fail`] when no
    /// `Error` edge exists, so graphs without one keep current behavior.
    #[serde(alias = "break")]
    Error,
}

impl GraphEdgeCondition {
    /// The DB/JSON tag for this condition. For `Route`, this is the fixed
    /// tag `"route"` — the label itself lives in [`Self::route_label`] (and,
    /// in storage, a sibling `route` column — see
    /// [`crate::db::Database::insert_graph_edge`]) so this stays a cheap
    /// `&str` borrow rather than an allocation.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Always => "always",
            Self::Route(_) => "route",
            Self::Error => "error",
        }
    }

    /// The route label this edge names, if this is a `Route` condition.
    pub fn route_label(&self) -> Option<&str> {
        match self {
            Self::Route(label) => Some(label),
            _ => None,
        }
    }

    /// Parses only the three condition kinds that round-trip through a
    /// single string — unchanged from before `Route` existed. A `Route`
    /// condition instead round-trips through [`Self::from_parts`], since it
    /// needs the sibling `route` column's label.
    ///
    /// Deliberately does **not** accept the retired `break` spelling — that
    /// only round-trips through serde's `#[serde(alias = "break")]` on
    /// `Error`, scoped to reading a pre-existing exported document (see
    /// `graph_transfer.rs`), never as a freshly-typed value on
    /// `graph_add_edge`/`graph_update_edge` or any other write path.
    pub fn from_str(value: &str) -> Option<Self> {
        match value {
            "pass" => Some(Self::Pass),
            "fail" => Some(Self::Fail),
            "always" => Some(Self::Always),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// Reconstruct a condition from its DB-persisted `(tag, route_label)`
    /// pair — the inverse of [`Self::as_str`]/[`Self::route_label`]. `tag`
    /// `"route"` requires a non-empty `route_label`; every other tag falls
    /// back to [`Self::from_str`] and ignores `route_label`.
    pub fn from_parts(tag: &str, route_label: Option<String>) -> Option<Self> {
        match tag {
            "route" => route_label
                .filter(|label| !label.trim().is_empty())
                .map(Self::Route),
            other => Self::from_str(other),
        }
    }
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphRunStatus {
    Running,
    Pass,
    Fail,
    /// Operator-interrupted: the node was explicitly stopped by an operator
    /// via `graph_pause(interrupt: true)`, not a failure of the node's work.
    Interrupted,
}

impl GraphRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Interrupted => "interrupted",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "pass" => Self::Pass,
            "fail" => Self::Fail,
            "interrupted" => Self::Interrupted,
            _ => Self::Running,
        }
    }
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
}

/// Config for a graph completion hook — an agent payload
/// (`platform`/`model`/`prompt`), a direct shell command (`command`), an
/// interactive message into a live session (`prompt` + `target_session_id`),
/// or a graph launch (`target_graph_id`). Exactly one mode must be configured;
/// the engine validates this at creation time.
///
/// An interactive hook is fire-and-forget: firing it enqueues one due-now
/// row in `scheduled_sends` for the exact configured session id, delivered
/// asynchronously by the TUI (which stays queued, not lost, while no TUI is
/// running). It never reads or waits for a reply.
///
/// A graph hook is fire-and-forget: firing it launches another graph in-process
/// without waiting for it (CH4). The launched graph's outcome never changes the
/// launching graph's status. Depth is capped at one: a graph launched by a hook
/// cannot itself launch another graph via hooks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCompletionHook {
    /// CLI platform for agent hooks (e.g. "mimo", "claude"). `None` for
    /// command and interactive hooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// Hook prompt template (agent and interactive hooks). `None` for
    /// command hooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Shell command to run directly (command hooks only). Supports the same
    /// `{{...}}` placeholders as the hook's event, substituted before
    /// execution. WARNING: do not configure a command that starts a canopy
    /// binary — it triggers daemon-startup recovery, which SIGTERMs live
    /// graph runs including the run that spawned the hook.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Exact interactive session id an interactive hook delivers to
    /// (interactive hooks only). `None` for agent and command hooks. A
    /// session id is not stable over time: it will eventually point at a
    /// session that no longer exists, and firing then fails loudly naming
    /// the id rather than redirecting anywhere else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_id: Option<String>,
    pub timeout_minutes: Option<u64>,
    /// Target graph id to launch (graph hooks only). Mutually exclusive with
    /// platform/command/target_session_id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_graph_id: Option<String>,
    /// Optional queue id for the launched graph (graph hooks only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    /// Optional workdir override for the launched graph (graph hooks only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workdir_override: Option<String>,
    /// Optional idea text for the launched graph (graph hooks only). Mutually
    /// exclusive with queue_id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idea: Option<String>,
}

impl GraphCompletionHook {
    /// Whether this is a command hook (runs a shell command directly).
    pub fn is_command(&self) -> bool {
        self.command.is_some()
    }

    /// Whether this is an agent hook (launches a CLI process).
    #[allow(dead_code)]
    pub fn is_agent(&self) -> bool {
        self.platform.is_some()
    }

    /// Whether this is an interactive hook (enqueues a scheduled send to a
    /// live session). Selected when `target_session_id` and `prompt` are
    /// present while `platform` and `command` are absent.
    pub fn is_interactive(&self) -> bool {
        self.target_session_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            && self.prompt.as_deref().is_some_and(|s| !s.trim().is_empty())
            && self.platform.as_deref().is_none_or(|s| s.trim().is_empty())
            && self.command.as_deref().is_none_or(|s| s.trim().is_empty())
    }

    /// Whether this is a graph hook (launches another graph in-process).
    pub fn is_graph(&self) -> bool {
        self.target_graph_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
    }
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

/// One firing of a graph hook. Deliberately its own table/type rather than
/// a `GraphNodeRun` — a hook run belongs to no spec and no graph node
/// (`graph_runs.spec_id`/`node_id` are `NOT NULL` FKs into exactly those),
/// and its outcome must never feed back into the run's routing or final
/// status the way a node run's does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphCompletionHookRun {
    pub id: String,
    pub graph_id: String,
    /// Which event produced this run.
    pub event: GraphHookEvent,
    /// Zero-based index within the event's hook list (declaration order).
    pub hook_index: i64,
    pub status: GraphRunStatus,
    pub output: Option<Value>,
    pub summary: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    /// Same B12 kill-on-abnormal-end treatment as [`GraphNodeRun::pid`].
    pub pid: Option<i64>,
    pub boot_id: Option<String>,
    /// CB43: platform/model resolved at dispatch for agent hooks
    /// (`None` for command/interactive hooks and pre-migration rows).
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

/// The four events a graph hook can fire on. Key name for the event-keyed
/// hooks map on [`Graph`]. Event names are the public vocabulary of the
/// hooks feature — chosen once here; CH2, CH3 and CH4 reuse them without
/// renaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(clippy::enum_variant_names)]
pub enum GraphHookEvent {
    /// Fires when a graph transitions to `Completed`.
    OnCompleted,
    /// Fires when a graph transitions to `Failed`.
    OnFailed,
    /// Fires when a graph stops with a blocker set (transition to `Paused`).
    OnBlocked,
    /// Fires once per spec reaching `completed`, whether from bound specs
    /// or from a queue.
    OnSpecCompleted,
}

impl GraphHookEvent {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OnCompleted => "on_completed",
            Self::OnFailed => "on_failed",
            Self::OnBlocked => "on_blocked",
            Self::OnSpecCompleted => "on_spec_completed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "on_completed" => Some(Self::OnCompleted),
            "on_failed" => Some(Self::OnFailed),
            "on_blocked" => Some(Self::OnBlocked),
            "on_spec_completed" => Some(Self::OnSpecCompleted),
            _ => None,
        }
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
    /// Join-node outgoing routing: where a `pass`/`fail` join result routes
    /// to next. `on_pass_to` is required at creation; `on_fail_to` is
    /// optional (a dead end on fail, same as any other node with no
    /// matching outgoing edge).
    pub on_pass_to: String,
    pub on_fail_to: Option<String>,
    pub kind: EnsembleKind,
    pub round_robin_index: Option<i64>,
    pub created_at: DateTime<Utc>,
}

impl Ensemble {
    /// The straggler kill timeout to actually use: the explicit override, or
    /// (by default) the members' own agent timeout.
    pub fn effective_straggler_timeout_minutes(&self) -> i64 {
        self.straggler_timeout_minutes
            .unwrap_or(self.timeout_minutes)
    }
}

/// `(platform, model, prompt_override, timeout_minutes)` — the normalized
/// shape of one ensemble member's identity, shared by `graph_add_ensemble`/
/// `graph_update_ensemble`'s validated input, [`EnsembleBlueprint`]'s stored
/// members, and [`EnsembleMember`] itself. `timeout_minutes` is this
/// member's own timeout override, or `None` to use the ensemble's shared
/// `timeout_minutes`.
///
/// [`EnsembleBlueprint`]: crate::domain::blueprints::EnsembleBlueprint
pub type EnsembleMemberSpec = (String, Option<String>, Option<String>, Option<i64>);

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnsembleDetails {
    pub ensemble: Ensemble,
    /// Members in `position` order — the order consolidation and
    /// `graph_update_ensemble` resize diffs rely on.
    pub members: Vec<EnsembleMember>,
}

#[cfg(test)]
mod tests {
    use super::{
        validate_router_edges_declared, validate_router_route_coverage, validate_router_routes,
        GraphEdge, GraphEdgeCondition, GraphNodeKind, GraphResetOutcome, GraphRunStatus,
        GraphSpecStatus, GraphStatus, RouterRoute, SpecAdminStatusOutcome,
    };

    #[test]
    fn graph_status_as_str_roundtrip() {
        assert_eq!(GraphStatus::Draft.as_str(), "draft");
        assert_eq!(GraphStatus::Running.as_str(), "running");
        assert_eq!(GraphStatus::Pausing.as_str(), "pausing");
        assert_eq!(GraphStatus::Paused.as_str(), "paused");
        assert_eq!(GraphStatus::Completed.as_str(), "completed");
        assert_eq!(GraphStatus::Failed.as_str(), "failed");
    }

    #[test]
    fn graph_status_from_str() {
        assert_eq!(GraphStatus::from_str("running"), GraphStatus::Running);
        assert_eq!(GraphStatus::from_str("pausing"), GraphStatus::Pausing);
        assert_eq!(GraphStatus::from_str("paused"), GraphStatus::Paused);
        assert_eq!(GraphStatus::from_str("completed"), GraphStatus::Completed);
        assert_eq!(GraphStatus::from_str("failed"), GraphStatus::Failed);
        assert_eq!(GraphStatus::from_str("invalid"), GraphStatus::Draft);
    }

    #[test]
    fn graph_spec_status_as_str() {
        assert_eq!(GraphSpecStatus::Pending.as_str(), "pending");
        assert_eq!(GraphSpecStatus::Running.as_str(), "running");
        assert_eq!(GraphSpecStatus::Completed.as_str(), "completed");
        assert_eq!(GraphSpecStatus::Failed.as_str(), "failed");
        assert_eq!(GraphSpecStatus::Skipped.as_str(), "skipped");
        assert_eq!(GraphSpecStatus::Interrupted.as_str(), "interrupted");
    }

    #[test]
    fn graph_spec_status_from_str() {
        assert_eq!(
            GraphSpecStatus::from_str("running"),
            GraphSpecStatus::Running
        );
        assert_eq!(
            GraphSpecStatus::from_str("completed"),
            GraphSpecStatus::Completed
        );
        assert_eq!(GraphSpecStatus::from_str("failed"), GraphSpecStatus::Failed);
        assert_eq!(
            GraphSpecStatus::from_str("skipped"),
            GraphSpecStatus::Skipped
        );
        assert_eq!(
            GraphSpecStatus::from_str("interrupted"),
            GraphSpecStatus::Interrupted
        );
        // An unknown status (e.g. written by a newer binary) must never
        // silently read as `Completed` — that would skip work that hasn't
        // actually run. `Pending` is the only safe fallback.
        assert_eq!(
            GraphSpecStatus::from_str("invalid"),
            GraphSpecStatus::Pending
        );
    }

    #[test]
    fn graph_node_kind_as_str() {
        assert_eq!(GraphNodeKind::Agent.as_str(), "agent");
        assert_eq!(GraphNodeKind::Check.as_str(), "check");
        assert_eq!(GraphNodeKind::Gate.as_str(), "gate");
        assert_eq!(GraphNodeKind::Join.as_str(), "join");
    }

    #[test]
    fn graph_node_kind_from_str() {
        assert_eq!(GraphNodeKind::from_str("agent"), Some(GraphNodeKind::Agent));
        assert_eq!(GraphNodeKind::from_str("check"), Some(GraphNodeKind::Check));
        assert_eq!(GraphNodeKind::from_str("gate"), Some(GraphNodeKind::Gate));
        assert_eq!(GraphNodeKind::from_str("join"), Some(GraphNodeKind::Join));
        assert!(GraphNodeKind::from_str("invalid").is_none());
    }

    #[test]
    fn graph_node_kind_display_str() {
        assert_eq!(GraphNodeKind::Agent.display_str(), "agent");
        assert_eq!(GraphNodeKind::Check.display_str(), "check");
        assert_eq!(GraphNodeKind::Gate.display_str(), "gate");
        assert_eq!(GraphNodeKind::Join.display_str(), "quorum");
    }

    #[test]
    fn ensemble_straggler_timeout_defaults_to_member_timeout() {
        let ensemble = super::Ensemble {
            id: "ens1".to_string(),
            spec_id: Some("spec1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "{{spec_content}}".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "n0".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: super::EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: chrono::Utc::now(),
        };
        assert_eq!(ensemble.effective_straggler_timeout_minutes(), 30);

        let mut overridden = ensemble;
        overridden.straggler_timeout_minutes = Some(5);
        assert_eq!(overridden.effective_straggler_timeout_minutes(), 5);
    }

    /// CM24: `quorum_grace_minutes` defaults to `None` (wait-for-all), and a
    /// set value is carried verbatim — there is no `effective_*` helper.
    #[test]
    fn ensemble_quorum_grace_none_by_default() {
        let ensemble = super::Ensemble {
            id: "ens1".to_string(),
            spec_id: Some("spec1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "{{spec_content}}".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "n0".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: super::EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: chrono::Utc::now(),
        };
        assert_eq!(ensemble.quorum_grace_minutes, None);

        let mut with_grace = ensemble;
        with_grace.quorum_grace_minutes = Some(0);
        assert_eq!(with_grace.quorum_grace_minutes, Some(0));
    }

    #[test]
    fn graph_edge_condition_as_str() {
        assert_eq!(GraphEdgeCondition::Pass.as_str(), "pass");
        assert_eq!(GraphEdgeCondition::Fail.as_str(), "fail");
        assert_eq!(GraphEdgeCondition::Always.as_str(), "always");
    }

    #[test]
    fn graph_edge_condition_from_str() {
        assert_eq!(
            GraphEdgeCondition::from_str("pass"),
            Some(GraphEdgeCondition::Pass)
        );
        assert_eq!(
            GraphEdgeCondition::from_str("fail"),
            Some(GraphEdgeCondition::Fail)
        );
        assert_eq!(
            GraphEdgeCondition::from_str("always"),
            Some(GraphEdgeCondition::Always)
        );
        assert!(GraphEdgeCondition::from_str("invalid").is_none());
    }

    #[test]
    fn graph_run_status_as_str() {
        assert_eq!(GraphRunStatus::Running.as_str(), "running");
        assert_eq!(GraphRunStatus::Pass.as_str(), "pass");
        assert_eq!(GraphRunStatus::Fail.as_str(), "fail");
        assert_eq!(GraphRunStatus::Interrupted.as_str(), "interrupted");
    }

    #[test]
    fn graph_run_status_from_str() {
        assert_eq!(GraphRunStatus::from_str("pass"), GraphRunStatus::Pass);
        assert_eq!(GraphRunStatus::from_str("fail"), GraphRunStatus::Fail);
        assert_eq!(
            GraphRunStatus::from_str("interrupted"),
            GraphRunStatus::Interrupted
        );
        assert_eq!(GraphRunStatus::from_str("invalid"), GraphRunStatus::Running);
    }

    fn graph_with_trigger(status: GraphStatus, trigger: Option<super::Trigger>) -> super::Graph {
        super::Graph {
            archived: false,
            paused_by_reconciliation: false,
            infra_node_id: None,
            id: "wf".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status,
            trigger,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn manual_graph_has_no_schedule_and_is_not_cron_or_watch() {
        let lp = graph_with_trigger(GraphStatus::Draft, None);
        assert_eq!(lp.trigger_type_label(), "manual");
        assert_eq!(lp.schedule_expr(), None);
        assert!(!lp.is_cron());
        assert!(!lp.is_watch());
        assert_eq!(lp.watch_path(), None);
    }

    #[test]
    fn cron_graph_exposes_schedule_expr() {
        let lp = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Cron {
                schedule_expr: "30 8 * * *".to_string(),
            }),
        );
        assert_eq!(lp.trigger_type_label(), "cron");
        assert_eq!(lp.schedule_expr(), Some("30 8 * * *"));
        assert!(lp.is_cron());
        assert!(!lp.is_watch());
    }

    #[test]
    fn watch_graph_exposes_path_and_events() {
        let lp = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Watch {
                path: "/tmp/watch".to_string(),
                events: vec![super::WatchEvent::Create],
                debounce_seconds: 2,
                recursive: false,
            }),
        );
        assert_eq!(lp.trigger_type_label(), "watch");
        assert!(lp.is_watch());
        assert_eq!(lp.watch_path(), Some("/tmp/watch"));
        assert_eq!(lp.watch_events(), Some(&[super::WatchEvent::Create][..]));
    }

    #[test]
    fn running_or_paused_graph_is_not_fireable() {
        // A trigger must not relaunch a graph that is already executing.
        assert!(!graph_with_trigger(GraphStatus::Running, None).is_fireable());
        assert!(!graph_with_trigger(GraphStatus::Paused, None).is_fireable());
        assert!(graph_with_trigger(GraphStatus::Draft, None).is_fireable());
        assert!(graph_with_trigger(GraphStatus::Completed, None).is_fireable());
        assert!(graph_with_trigger(GraphStatus::Failed, None).is_fireable());
    }

    #[test]
    fn future_autorun_at_is_not_due() {
        let mut lp = graph_with_trigger(GraphStatus::Failed, None);
        lp.autorun_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
        assert!(!lp.is_autorun_due(chrono::Utc::now()));
    }

    #[test]
    fn past_autorun_at_is_due_on_a_fireable_graph() {
        let mut lp = graph_with_trigger(GraphStatus::Failed, None);
        lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        assert!(lp.is_autorun_due(chrono::Utc::now()));
    }

    #[test]
    fn no_autorun_at_is_never_due() {
        let lp = graph_with_trigger(GraphStatus::Failed, None);
        assert!(!lp.is_autorun_due(chrono::Utc::now()));
    }

    #[test]
    fn past_autorun_at_is_not_due_while_running_or_paused() {
        for status in [GraphStatus::Running, GraphStatus::Paused] {
            let mut lp = graph_with_trigger(status, None);
            lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
            assert!(
                !lp.is_autorun_due(chrono::Utc::now()),
                "{status:?} graph must not fire autorun_at"
            );
        }
    }

    /// C1: a graph reconciliation paused after an unclean daemon exit must
    /// still fire its pending, due `autorun_at` — that schedule is the
    /// resilience node's one shot at an unattended quota-reset resume, and
    /// nobody asked for the graph to stop.
    #[test]
    fn past_autorun_at_is_due_on_a_reconciliation_paused_graph() {
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.paused_by_reconciliation = true;
        lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        assert!(lp.is_autorun_due(chrono::Utc::now()));
    }

    /// C1: the operator's own pause must keep blocking the schedule even
    /// though a reconciliation pause no longer does — `paused_by_reconciliation`
    /// is what tells the two apart, not `Paused` alone.
    #[test]
    fn past_autorun_at_is_not_due_on_an_operator_paused_graph() {
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.paused_by_reconciliation = false;
        lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        assert!(!lp.is_autorun_due(chrono::Utc::now()));
    }

    #[test]
    fn past_auto_continue_at_is_due_while_paused() {
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.auto_continue_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        assert!(lp.is_auto_continue_due(chrono::Utc::now()));
    }

    #[test]
    fn future_auto_continue_at_is_not_due() {
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.auto_continue_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
        assert!(!lp.is_auto_continue_due(chrono::Utc::now()));
    }

    #[test]
    fn no_auto_continue_at_is_never_due() {
        let lp = graph_with_trigger(GraphStatus::Paused, None);
        assert!(!lp.is_auto_continue_due(chrono::Utc::now()));
    }

    #[test]
    fn past_auto_continue_at_is_not_due_while_not_paused() {
        for status in [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ] {
            let mut lp = graph_with_trigger(status, None);
            lp.auto_continue_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
            assert!(
                !lp.is_auto_continue_due(chrono::Utc::now()),
                "{status:?} graph must not fire auto_continue_at"
            );
            assert!(
                lp.is_auto_continue_time_reached(chrono::Utc::now()),
                "{status:?} graph's auto_continue_at time itself must still register as reached \
                 so the scheduler can clear the stale schedule"
            );
        }
    }

    // ── GraphStatus: as_str / from_str roundtrip ─────────────────────────

    #[test]
    fn graph_status_as_str_from_str_roundtrip() {
        let statuses = [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Paused,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ];
        for s in statuses {
            let s_str = s.as_str();
            assert_eq!(
                GraphStatus::from_str(s_str),
                s,
                "roundtrip failed for {s_str}"
            );
        }
    }

    #[test]
    fn graph_status_from_str_unknown_defaults_to_draft() {
        let unknowns = ["", "DRAFT", "Running", "PENDING", "unknown", "123"];
        for input in unknowns {
            assert_eq!(
                GraphStatus::from_str(input),
                GraphStatus::Draft,
                "expected Draft for {input:?}"
            );
        }
    }

    #[test]
    fn graph_status_as_str_returns_lowercase() {
        for s in [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Paused,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ] {
            assert_eq!(s.as_str(), s.as_str().to_lowercase());
        }
    }

    // ── GraphSpecStatus: as_str / from_str roundtrip ─────────────────────

    #[test]
    fn graph_spec_status_as_str_from_str_roundtrip() {
        let statuses = [
            GraphSpecStatus::Pending,
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Interrupted,
        ];
        for s in statuses {
            let s_str = s.as_str();
            assert_eq!(
                GraphSpecStatus::from_str(s_str),
                s,
                "roundtrip failed for {s_str}"
            );
        }
    }

    #[test]
    fn graph_spec_status_from_str_unknown_defaults_to_pending() {
        let unknowns = ["", "PENDING", "Running", "DONE", "unknown", "xyz"];
        for input in unknowns {
            assert_eq!(
                GraphSpecStatus::from_str(input),
                GraphSpecStatus::Pending,
                "expected Pending for {input:?}"
            );
        }
    }

    #[test]
    fn graph_spec_status_as_str_returns_lowercase() {
        for s in [
            GraphSpecStatus::Pending,
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Interrupted,
        ] {
            assert_eq!(s.as_str(), s.as_str().to_lowercase());
        }
    }

    #[test]
    fn graph_spec_status_pending_is_distinct_from_running() {
        assert_ne!(
            GraphSpecStatus::Pending.as_str(),
            GraphSpecStatus::Running.as_str()
        );
    }

    // ── GraphNodeKind: as_str / from_str / display_str roundtrip ─────────

    #[test]
    fn graph_node_kind_as_str_from_str_roundtrip() {
        let kinds = [
            GraphNodeKind::Agent,
            GraphNodeKind::Check,
            GraphNodeKind::Gate,
            GraphNodeKind::Join,
        ];
        for k in kinds {
            let k_str = k.as_str();
            assert_eq!(
                GraphNodeKind::from_str(k_str),
                Some(k),
                "roundtrip failed for {k_str}"
            );
        }
    }

    #[test]
    fn graph_node_kind_from_str_invalid_returns_none() {
        let invalids = ["", "AGENT", "Agent", "ensemble", "unknown", "workflow"];
        for input in invalids {
            assert_eq!(
                GraphNodeKind::from_str(input),
                None,
                "expected None for {input:?}"
            );
        }
    }

    #[test]
    fn graph_node_kind_display_str_matches_as_str_except_join() {
        for k in [
            GraphNodeKind::Agent,
            GraphNodeKind::Check,
            GraphNodeKind::Gate,
        ] {
            assert_eq!(k.display_str(), k.as_str());
        }
        assert_eq!(GraphNodeKind::Join.display_str(), "quorum");
        assert_ne!(
            GraphNodeKind::Join.display_str(),
            GraphNodeKind::Join.as_str()
        );
    }

    #[test]
    fn graph_node_kind_as_str_returns_lowercase() {
        for k in [
            GraphNodeKind::Agent,
            GraphNodeKind::Check,
            GraphNodeKind::Gate,
            GraphNodeKind::Join,
        ] {
            assert_eq!(k.as_str(), k.as_str().to_lowercase());
        }
    }

    // ── GraphEdgeCondition: as_str / from_str roundtrip ──────────────────

    #[test]
    fn graph_edge_condition_as_str_from_str_roundtrip() {
        let conds = [
            GraphEdgeCondition::Pass,
            GraphEdgeCondition::Fail,
            GraphEdgeCondition::Always,
        ];
        for c in conds {
            let c_str = c.as_str().to_string();
            assert_eq!(
                GraphEdgeCondition::from_str(&c_str),
                Some(c.clone()),
                "roundtrip failed for {c_str}"
            );
        }
    }

    #[test]
    fn graph_edge_condition_from_str_invalid_returns_none() {
        let invalids = ["", "PASS", "Pass", "never", "sometimes", "123"];
        for input in invalids {
            assert_eq!(
                GraphEdgeCondition::from_str(input),
                None,
                "expected None for {input:?}"
            );
        }
    }

    #[test]
    fn graph_edge_condition_as_str_returns_lowercase() {
        for c in [
            GraphEdgeCondition::Pass,
            GraphEdgeCondition::Fail,
            GraphEdgeCondition::Always,
        ] {
            assert_eq!(c.as_str(), c.as_str().to_lowercase());
        }
    }

    // ── GraphRunStatus: as_str / from_str roundtrip ──────────────────────

    #[test]
    fn graph_run_status_as_str_from_str_roundtrip() {
        let statuses = [
            GraphRunStatus::Running,
            GraphRunStatus::Pass,
            GraphRunStatus::Fail,
        ];
        for s in statuses {
            let s_str = s.as_str();
            assert_eq!(
                GraphRunStatus::from_str(s_str),
                s,
                "roundtrip failed for {s_str}"
            );
        }
    }

    #[test]
    fn graph_run_status_from_str_unknown_defaults_to_running() {
        let unknowns = ["", "RUNNING", "Running", "done", "unknown", "42"];
        for input in unknowns {
            assert_eq!(
                GraphRunStatus::from_str(input),
                GraphRunStatus::Running,
                "expected Running for {input:?}"
            );
        }
    }

    #[test]
    fn graph_run_status_as_str_returns_lowercase() {
        for s in [
            GraphRunStatus::Running,
            GraphRunStatus::Pass,
            GraphRunStatus::Fail,
        ] {
            assert_eq!(s.as_str(), s.as_str().to_lowercase());
        }
    }

    // ── SpecAdminStatusOutcome: variant construction & equality ─────────

    #[test]
    fn spec_admin_status_outcome_success_variants() {
        let a = SpecAdminStatusOutcome::Success;
        assert!(matches!(a, SpecAdminStatusOutcome::Success));
    }

    #[test]
    fn spec_admin_status_outcome_not_found_variants() {
        let a = SpecAdminStatusOutcome::NotFound;
        assert!(matches!(a, SpecAdminStatusOutcome::NotFound));
    }

    #[test]
    fn spec_admin_status_outcome_not_standalone_carries_id() {
        let outcome = SpecAdminStatusOutcome::NotStandalone("spec-42".to_string());
        match outcome {
            SpecAdminStatusOutcome::NotStandalone(id) => assert_eq!(id, "spec-42"),
            _ => panic!("expected NotStandalone"),
        }
    }

    #[test]
    fn spec_admin_status_outcome_active_run_carries_ids() {
        let outcome = SpecAdminStatusOutcome::ActiveRun {
            graph_id: "graph-1".to_string(),
            run_id: "run-2".to_string(),
        };
        match outcome {
            SpecAdminStatusOutcome::ActiveRun { graph_id, run_id } => {
                assert_eq!(graph_id, "graph-1");
                assert_eq!(run_id, "run-2");
            }
            _ => panic!("expected ActiveRun"),
        }
    }

    #[test]
    fn spec_admin_status_outcome_variants_are_distinct() {
        let success = SpecAdminStatusOutcome::Success;
        let not_found = SpecAdminStatusOutcome::NotFound;
        let not_standalone = SpecAdminStatusOutcome::NotStandalone("x".to_string());
        let active_run = SpecAdminStatusOutcome::ActiveRun {
            graph_id: "a".to_string(),
            run_id: "b".to_string(),
        };
        assert_ne!(format!("{success:?}"), format!("{not_found:?}"));
        assert_ne!(format!("{not_standalone:?}"), format!("{active_run:?}"));
    }

    // ── GraphResetOutcome: variant construction & equality ───────────────

    #[test]
    fn graph_reset_outcome_not_found() {
        let a = GraphResetOutcome::NotFound;
        assert!(matches!(a, GraphResetOutcome::NotFound));
    }

    #[test]
    fn graph_reset_outcome_in_flight_carries_run_info() {
        let started_at = chrono::Utc::now();
        let a = GraphResetOutcome::InFlight {
            run_id: "run-1".to_string(),
            node_id: "node-1".to_string(),
            started_at,
        };
        match a {
            GraphResetOutcome::InFlight {
                run_id,
                node_id,
                started_at: at,
            } => {
                assert_eq!(run_id, "run-1");
                assert_eq!(node_id, "node-1");
                assert_eq!(at, started_at);
            }
            _ => panic!("expected InFlight"),
        }
    }

    #[test]
    fn graph_reset_outcome_invalid_spec_carries_id() {
        let outcome = GraphResetOutcome::InvalidSpec("bad-spec".to_string());
        match outcome {
            GraphResetOutcome::InvalidSpec(id) => assert_eq!(id, "bad-spec"),
            _ => panic!("expected InvalidSpec"),
        }
    }

    #[test]
    fn graph_reset_outcome_reset_carries_count() {
        let outcome = GraphResetOutcome::Reset {
            spec_count: 5,
            skipped_count: 0,
        };
        match outcome {
            GraphResetOutcome::Reset {
                spec_count,
                skipped_count: _,
            } => assert_eq!(spec_count, 5),
            _ => panic!("expected Reset"),
        }
    }

    #[test]
    fn graph_reset_outcome_variants_are_distinct() {
        let not_found = GraphResetOutcome::NotFound;
        let in_flight = GraphResetOutcome::InFlight {
            run_id: "run-1".to_string(),
            node_id: "node-1".to_string(),
            started_at: chrono::Utc::now(),
        };
        let invalid = GraphResetOutcome::InvalidSpec("x".to_string());
        let reset = GraphResetOutcome::Reset {
            spec_count: 0,
            skipped_count: 0,
        };
        assert_ne!(format!("{not_found:?}"), format!("{in_flight:?}"));
        assert_ne!(format!("{invalid:?}"), format!("{reset:?}"));
    }

    // ── GraphStatus: equality & Debug ────────────────────────────────────

    #[test]
    fn graph_status_variants_are_distinct() {
        let all = [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Paused,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let a = all[i];
                let b = all[j];
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }

    #[test]
    fn graph_status_debug_format_matches_variant_name() {
        assert_eq!(format!("{:?}", GraphStatus::Draft), "Draft");
        assert_eq!(format!("{:?}", GraphStatus::Running), "Running");
        assert_eq!(format!("{:?}", GraphStatus::Paused), "Paused");
        assert_eq!(format!("{:?}", GraphStatus::Completed), "Completed");
        assert_eq!(format!("{:?}", GraphStatus::Failed), "Failed");
    }

    // ── GraphSpecStatus: equality & Debug ────────────────────────────────

    #[test]
    fn graph_spec_status_variants_are_distinct() {
        let all = [
            GraphSpecStatus::Pending,
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Interrupted,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let a = all[i];
                let b = all[j];
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }

    // ── GraphNodeKind: equality & Debug ──────────────────────────────────

    #[test]
    fn graph_node_kind_variants_are_distinct() {
        let all = [
            GraphNodeKind::Agent,
            GraphNodeKind::Check,
            GraphNodeKind::Gate,
            GraphNodeKind::Join,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let a = all[i];
                let b = all[j];
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }

    // ── GraphEdgeCondition: equality & Debug ─────────────────────────────

    #[test]
    fn graph_edge_condition_variants_are_distinct() {
        let all = [
            GraphEdgeCondition::Pass,
            GraphEdgeCondition::Fail,
            GraphEdgeCondition::Always,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let a = &all[i];
                let b = &all[j];
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }

    // ── GraphRunStatus: equality & Debug ─────────────────────────────────

    #[test]
    fn graph_run_status_variants_are_distinct() {
        let all = [
            GraphRunStatus::Running,
            GraphRunStatus::Pass,
            GraphRunStatus::Fail,
        ];
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let a = all[i];
                let b = all[j];
                assert_ne!(a, b, "{a:?} should differ from {b:?}");
            }
        }
    }

    // ── Clone: all status enums are Clone ───────────────────────────────

    #[test]
    fn graph_status_is_clone() {
        let s = GraphStatus::Running;
        let cloned = s;
        assert_eq!(s, cloned);
    }

    #[test]
    fn graph_spec_status_is_clone() {
        let s = GraphSpecStatus::Failed;
        let cloned = s;
        assert_eq!(s, cloned);
    }

    #[test]
    fn graph_node_kind_is_clone() {
        let k = GraphNodeKind::Join;
        let cloned = k;
        assert_eq!(k, cloned);
    }

    #[test]
    fn graph_edge_condition_is_clone() {
        let c = GraphEdgeCondition::Always;
        let cloned = c.clone();
        assert_eq!(c, cloned);
    }

    #[test]
    fn graph_run_status_is_clone() {
        let s = GraphRunStatus::Pass;
        let cloned = s;
        assert_eq!(s, cloned);
    }

    // ── Graph: is_fireable exhaustive coverage ───────────────────────────

    #[test]
    fn graph_is_fireable_exhaustive() {
        let expected_fireable = [
            (GraphStatus::Draft, true),
            (GraphStatus::Running, false),
            (GraphStatus::Pausing, false),
            (GraphStatus::Paused, false),
            (GraphStatus::Completed, true),
            (GraphStatus::Failed, true),
        ];
        for (status, expected) in expected_fireable {
            let lp = graph_with_trigger(status, None);
            assert_eq!(
                lp.is_fireable(),
                expected,
                "{status:?}.is_fireable() should be {expected}"
            );
        }
    }

    // ── Graph: trigger_type_label exhaustive ─────────────────────────────

    #[test]
    fn graph_trigger_type_label_exhaustive() {
        let lp_manual = graph_with_trigger(GraphStatus::Draft, None);
        assert_eq!(lp_manual.trigger_type_label(), "manual");

        let lp_cron = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Cron {
                schedule_expr: "* * * * *".to_string(),
            }),
        );
        assert_eq!(lp_cron.trigger_type_label(), "cron");

        let lp_watch = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Watch {
                path: "/tmp".to_string(),
                events: vec![],
                debounce_seconds: 2,
                recursive: false,
            }),
        );
        assert_eq!(lp_watch.trigger_type_label(), "watch");
    }

    // ── Graph: schedule_expr / watch_path exhaustive ─────────────────────

    #[test]
    fn graph_schedule_expr_only_some_for_cron() {
        let cron = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Cron {
                schedule_expr: "0 9 * * 1-5".to_string(),
            }),
        );
        assert_eq!(cron.schedule_expr(), Some("0 9 * * 1-5"));

        let manual = graph_with_trigger(GraphStatus::Draft, None);
        assert_eq!(manual.schedule_expr(), None);

        let watch = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Watch {
                path: "/src".to_string(),
                events: vec![],
                debounce_seconds: 2,
                recursive: false,
            }),
        );
        assert_eq!(watch.schedule_expr(), None);
    }

    #[test]
    fn graph_watch_path_only_some_for_watch() {
        let watch = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Watch {
                path: "/data".to_string(),
                events: vec![super::WatchEvent::Modify],
                debounce_seconds: 2,
                recursive: false,
            }),
        );
        assert_eq!(watch.watch_path(), Some("/data"));

        let cron = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Cron {
                schedule_expr: "* * * * *".to_string(),
            }),
        );
        assert_eq!(cron.watch_path(), None);

        let manual = graph_with_trigger(GraphStatus::Draft, None);
        assert_eq!(manual.watch_path(), None);
    }

    // ── Graph: is_autorun_due edge cases ─────────────────────────────────

    #[test]
    fn graph_autorun_due_exactly_at_threshold() {
        let now = chrono::Utc::now();
        let mut lp = graph_with_trigger(GraphStatus::Completed, None);
        lp.autorun_at = Some(now);
        assert!(lp.is_autorun_due(now));
    }

    #[test]
    fn graph_autorun_not_due_one_second_before() {
        let now = chrono::Utc::now();
        let mut lp = graph_with_trigger(GraphStatus::Completed, None);
        lp.autorun_at = Some(now + chrono::Duration::seconds(1));
        assert!(!lp.is_autorun_due(now));
    }

    // ── Graph: is_auto_continue_due edge cases ───────────────────────────

    #[test]
    fn graph_auto_continue_due_exactly_at_threshold() {
        let now = chrono::Utc::now();
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.auto_continue_at = Some(now);
        assert!(lp.is_auto_continue_due(now));
    }

    #[test]
    fn graph_auto_continue_not_due_one_second_before() {
        let now = chrono::Utc::now();
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.auto_continue_at = Some(now + chrono::Duration::seconds(1));
        assert!(!lp.is_auto_continue_due(now));
    }

    #[test]
    fn graph_auto_continue_time_reached_independent_of_status() {
        let now = chrono::Utc::now();
        for status in [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Paused,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ] {
            let mut lp = graph_with_trigger(status, None);
            lp.auto_continue_at = Some(now - chrono::Duration::seconds(1));
            assert!(
                lp.is_auto_continue_time_reached(now),
                "{status:?}: time_reached should be true"
            );
        }
    }

    #[test]
    fn graph_auto_continue_time_not_reached_in_future() {
        let now = chrono::Utc::now();
        let mut lp = graph_with_trigger(GraphStatus::Paused, None);
        lp.auto_continue_at = Some(now + chrono::Duration::hours(1));
        assert!(!lp.is_auto_continue_time_reached(now));
    }

    #[test]
    fn graph_auto_continue_time_not_reached_when_none() {
        let lp = graph_with_trigger(GraphStatus::Paused, None);
        assert!(!lp.is_auto_continue_time_reached(chrono::Utc::now()));
    }

    // ── Graph: is_cron / is_watch exhaustive ─────────────────────────────

    #[test]
    fn graph_is_cron_and_is_watch_exhaustive() {
        let manual = graph_with_trigger(GraphStatus::Draft, None);
        assert!(!manual.is_cron());
        assert!(!manual.is_watch());

        let cron = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Cron {
                schedule_expr: "* * * * *".to_string(),
            }),
        );
        assert!(cron.is_cron());
        assert!(!cron.is_watch());

        let watch = graph_with_trigger(
            GraphStatus::Draft,
            Some(super::Trigger::Watch {
                path: "/x".to_string(),
                events: vec![],
                debounce_seconds: 2,
                recursive: false,
            }),
        );
        assert!(!watch.is_cron());
        assert!(watch.is_watch());
    }

    // ── serde: GraphStatus roundtrip ─────────────────────────────────────

    #[test]
    fn graph_status_serde_roundtrip() {
        let statuses = [
            GraphStatus::Draft,
            GraphStatus::Running,
            GraphStatus::Paused,
            GraphStatus::Completed,
            GraphStatus::Failed,
        ];
        for s in statuses {
            let json = serde_json::to_string(&s).unwrap();
            let deserialized: GraphStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, s, "serde roundtrip failed for {json}");
        }
    }

    #[test]
    fn graph_status_serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&GraphStatus::Draft).unwrap(),
            "\"draft\""
        );
        assert_eq!(
            serde_json::to_string(&GraphStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&GraphStatus::Completed).unwrap(),
            "\"completed\""
        );
    }

    // ── serde: GraphSpecStatus roundtrip ─────────────────────────────────

    #[test]
    fn graph_spec_status_serde_roundtrip() {
        let statuses = [
            GraphSpecStatus::Pending,
            GraphSpecStatus::Running,
            GraphSpecStatus::Completed,
            GraphSpecStatus::Failed,
            GraphSpecStatus::Skipped,
            GraphSpecStatus::Interrupted,
        ];
        for s in statuses {
            let json = serde_json::to_string(&s).unwrap();
            let deserialized: GraphSpecStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, s, "serde roundtrip failed for {json}");
        }
    }

    #[test]
    fn graph_spec_status_serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&GraphSpecStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&GraphSpecStatus::Skipped).unwrap(),
            "\"skipped\""
        );
        assert_eq!(
            serde_json::to_string(&GraphSpecStatus::Interrupted).unwrap(),
            "\"interrupted\""
        );
    }

    // ── serde: GraphNodeKind roundtrip ───────────────────────────────────

    #[test]
    fn graph_node_kind_serde_roundtrip() {
        let kinds = [
            GraphNodeKind::Agent,
            GraphNodeKind::Check,
            GraphNodeKind::Gate,
            GraphNodeKind::Join,
        ];
        for k in kinds {
            let json = serde_json::to_string(&k).unwrap();
            let deserialized: GraphNodeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, k, "serde roundtrip failed for {json}");
        }
    }

    #[test]
    fn graph_node_kind_serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&GraphNodeKind::Agent).unwrap(),
            "\"agent\""
        );
        assert_eq!(
            serde_json::to_string(&GraphNodeKind::Gate).unwrap(),
            "\"gate\""
        );
    }

    // ── serde: GraphEdgeCondition roundtrip ──────────────────────────────

    #[test]
    fn graph_edge_condition_serde_roundtrip() {
        let conds = [
            GraphEdgeCondition::Pass,
            GraphEdgeCondition::Fail,
            GraphEdgeCondition::Always,
        ];
        for c in conds {
            let json = serde_json::to_string(&c).unwrap();
            let deserialized: GraphEdgeCondition = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, c, "serde roundtrip failed for {json}");
        }
    }

    #[test]
    fn graph_edge_condition_serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&GraphEdgeCondition::Pass).unwrap(),
            "\"pass\""
        );
        assert_eq!(
            serde_json::to_string(&GraphEdgeCondition::Always).unwrap(),
            "\"always\""
        );
    }

    // ── serde: GraphRunStatus roundtrip ──────────────────────────────────

    #[test]
    fn graph_run_status_serde_roundtrip() {
        let statuses = [
            GraphRunStatus::Running,
            GraphRunStatus::Pass,
            GraphRunStatus::Fail,
        ];
        for s in statuses {
            let json = serde_json::to_string(&s).unwrap();
            let deserialized: GraphRunStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized, s, "serde roundtrip failed for {json}");
        }
    }

    #[test]
    fn graph_run_status_serde_uses_snake_case() {
        assert_eq!(
            serde_json::to_string(&GraphRunStatus::Running).unwrap(),
            "\"running\""
        );
        assert_eq!(
            serde_json::to_string(&GraphRunStatus::Pass).unwrap(),
            "\"pass\""
        );
    }

    // ── serde: deserialization from string variants ─────────────────────

    #[test]
    fn graph_status_deserialize_from_json_string() {
        let input = "\"completed\"";
        let s: GraphStatus = serde_json::from_str(input).unwrap();
        assert_eq!(s, GraphStatus::Completed);
    }

    #[test]
    fn graph_spec_status_deserialize_from_json_string() {
        let input = "\"skipped\"";
        let s: GraphSpecStatus = serde_json::from_str(input).unwrap();
        assert_eq!(s, GraphSpecStatus::Skipped);
    }

    #[test]
    fn graph_node_kind_deserialize_from_json_string() {
        let input = "\"join\"";
        let k: GraphNodeKind = serde_json::from_str(input).unwrap();
        assert_eq!(k, GraphNodeKind::Join);
    }

    #[test]
    fn graph_edge_condition_deserialize_from_json_string() {
        let input = "\"always\"";
        let c: GraphEdgeCondition = serde_json::from_str(input).unwrap();
        assert_eq!(c, GraphEdgeCondition::Always);
    }

    #[test]
    fn graph_run_status_deserialize_from_json_string() {
        let input = "\"fail\"";
        let s: GraphRunStatus = serde_json::from_str(input).unwrap();
        assert_eq!(s, GraphRunStatus::Fail);
    }

    // ── SpecAdminStatusOutcome: clone & Debug ───────────────────────────

    #[test]
    fn spec_admin_status_outcome_success_is_clone() {
        let a = SpecAdminStatusOutcome::Success;
        let b = a;
        assert!(matches!(b, SpecAdminStatusOutcome::Success));
        fn _assert_clone<T: Clone>() {}
        _assert_clone::<SpecAdminStatusOutcome>();
    }

    #[test]
    fn spec_admin_status_outcome_not_found_is_clone() {
        let a = SpecAdminStatusOutcome::NotFound;
        let b = a;
        assert!(matches!(b, SpecAdminStatusOutcome::NotFound));
        fn _assert_clone<T: Clone>() {}
        _assert_clone::<SpecAdminStatusOutcome>();
    }

    #[test]
    fn spec_admin_status_outcome_not_standalone_is_clone() {
        let a = SpecAdminStatusOutcome::NotStandalone("s1".to_string());
        let b = a.clone();
        match (&a, &b) {
            (
                SpecAdminStatusOutcome::NotStandalone(o),
                SpecAdminStatusOutcome::NotStandalone(c),
            ) => {
                assert_eq!(o, c);
                assert_eq!(c, "s1");
            }
            _ => panic!("expected clone of NotStandalone"),
        }
    }

    #[test]
    fn spec_admin_status_outcome_active_run_is_clone() {
        let a = SpecAdminStatusOutcome::ActiveRun {
            graph_id: "l1".to_string(),
            run_id: "r1".to_string(),
        };
        let b = a.clone();
        match (&a, &b) {
            (
                SpecAdminStatusOutcome::ActiveRun {
                    graph_id: al,
                    run_id: ar,
                },
                SpecAdminStatusOutcome::ActiveRun {
                    graph_id: bl,
                    run_id: br,
                },
            ) => {
                assert_eq!(al, bl);
                assert_eq!(ar, br);
                assert_eq!(bl, "l1");
                assert_eq!(br, "r1");
            }
            _ => panic!("expected clone of ActiveRun"),
        }
    }

    // ── GraphResetOutcome: clone & Debug ─────────────────────────────────

    #[test]
    fn graph_reset_outcome_is_clone() {
        let outcomes = [
            GraphResetOutcome::NotFound,
            GraphResetOutcome::InFlight {
                run_id: "run-1".to_string(),
                node_id: "node-1".to_string(),
                started_at: chrono::Utc::now(),
            },
            GraphResetOutcome::InvalidSpec("x".to_string()),
            GraphResetOutcome::Reset {
                spec_count: 3,
                skipped_count: 0,
            },
        ];
        for o in outcomes {
            let cloned = o.clone();
            assert_eq!(format!("{o:?}"), format!("{cloned:?}"));
        }
    }

    // ── GraphNodeKind::Router: as_str / from_str / display_str ───────────

    #[test]
    fn graph_node_kind_router_as_str() {
        assert_eq!(GraphNodeKind::Router.as_str(), "router");
    }

    #[test]
    fn graph_node_kind_router_from_str_roundtrip() {
        assert_eq!(
            GraphNodeKind::from_str("router"),
            Some(GraphNodeKind::Router)
        );
        assert_eq!(
            GraphNodeKind::from_str(GraphNodeKind::Router.as_str()),
            Some(GraphNodeKind::Router)
        );
    }

    #[test]
    fn graph_node_kind_router_display_str_matches_as_str() {
        assert_eq!(GraphNodeKind::Router.display_str(), "router");
    }

    #[test]
    fn graph_node_kind_router_serde_roundtrip() {
        let json = serde_json::to_string(&GraphNodeKind::Router).unwrap();
        assert_eq!(json, "\"router\"");
        let deserialized: GraphNodeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, GraphNodeKind::Router);
    }

    // ── GraphEdgeCondition::Route: as_str / route_label / from_parts ─────

    #[test]
    fn graph_edge_condition_route_as_str_is_fixed_tag() {
        let c = GraphEdgeCondition::Route("escalate".to_string());
        assert_eq!(c.as_str(), "route");
    }

    #[test]
    fn graph_edge_condition_route_label_returns_the_label() {
        let c = GraphEdgeCondition::Route("escalate".to_string());
        assert_eq!(c.route_label(), Some("escalate"));
    }

    #[test]
    fn graph_edge_condition_non_route_has_no_route_label() {
        assert_eq!(GraphEdgeCondition::Pass.route_label(), None);
        assert_eq!(GraphEdgeCondition::Fail.route_label(), None);
        assert_eq!(GraphEdgeCondition::Always.route_label(), None);
    }

    #[test]
    fn graph_edge_condition_from_str_does_not_parse_route() {
        // `Route` needs the sibling label, which a bare string can't carry —
        // only `from_parts` (DB round trip) or the `route:` MCP param path
        // can construct it.
        assert_eq!(GraphEdgeCondition::from_str("route"), None);
    }

    #[test]
    fn graph_edge_condition_from_parts_reconstructs_route() {
        let c = GraphEdgeCondition::from_parts("route", Some("escalate".to_string()));
        assert_eq!(c, Some(GraphEdgeCondition::Route("escalate".to_string())));
    }

    #[test]
    fn graph_edge_condition_from_parts_route_without_label_is_none() {
        assert_eq!(GraphEdgeCondition::from_parts("route", None), None);
        assert_eq!(
            GraphEdgeCondition::from_parts("route", Some("".to_string())),
            None
        );
    }

    #[test]
    fn graph_edge_condition_from_parts_falls_back_to_from_str() {
        assert_eq!(
            GraphEdgeCondition::from_parts("pass", None),
            Some(GraphEdgeCondition::Pass)
        );
        assert_eq!(GraphEdgeCondition::from_parts("bogus", None), None);
    }

    // ── validate_router_routes ───────────────────────────────────────────

    fn two_routes() -> Vec<RouterRoute> {
        vec![
            RouterRoute {
                label: "retry".to_string(),
                description: "Retry the current step.".to_string(),
            },
            RouterRoute {
                label: "escalate".to_string(),
                description: "Hand off to a human.".to_string(),
            },
        ]
    }

    #[test]
    fn validate_router_routes_accepts_valid_shape() {
        assert!(validate_router_routes(&two_routes(), "retry").is_ok());
    }

    #[test]
    fn validate_router_routes_rejects_fewer_than_two() {
        let routes = vec![RouterRoute {
            label: "retry".to_string(),
            description: "Retry the current step.".to_string(),
        }];
        let err = validate_router_routes(&routes, "retry").unwrap_err();
        assert!(err.contains("at least 2 routes"));
    }

    #[test]
    fn validate_router_routes_rejects_more_than_eight() {
        let routes = (0..9)
            .map(|i| RouterRoute {
                label: format!("route{i}"),
                description: "A route.".to_string(),
            })
            .collect::<Vec<_>>();
        let err = validate_router_routes(&routes, "route0").unwrap_err();
        assert!(err.contains("at most 8 routes"));
    }

    #[test]
    fn validate_router_routes_rejects_empty_label() {
        let routes = vec![
            RouterRoute {
                label: "  ".to_string(),
                description: "desc".to_string(),
            },
            RouterRoute {
                label: "escalate".to_string(),
                description: "desc".to_string(),
            },
        ];
        let err = validate_router_routes(&routes, "escalate").unwrap_err();
        assert!(err.contains("non-empty label"));
    }

    #[test]
    fn validate_router_routes_rejects_empty_description() {
        let routes = vec![
            RouterRoute {
                label: "retry".to_string(),
                description: "".to_string(),
            },
            RouterRoute {
                label: "escalate".to_string(),
                description: "desc".to_string(),
            },
        ];
        let err = validate_router_routes(&routes, "retry").unwrap_err();
        assert!(err.contains("non-empty description"));
    }

    #[test]
    fn validate_router_routes_rejects_duplicate_labels() {
        let routes = vec![
            RouterRoute {
                label: "retry".to_string(),
                description: "desc one".to_string(),
            },
            RouterRoute {
                label: "retry".to_string(),
                description: "desc two".to_string(),
            },
        ];
        let err = validate_router_routes(&routes, "retry").unwrap_err();
        assert!(err.contains("declared more than once"));
    }

    #[test]
    fn validate_router_routes_rejects_no_fallback() {
        let err = validate_router_routes(&two_routes(), "").unwrap_err();
        assert!(err.contains("must declare one route as fallback"));
    }

    #[test]
    fn validate_router_routes_rejects_fallback_naming_undeclared_route() {
        let err = validate_router_routes(&two_routes(), "nonexistent").unwrap_err();
        assert!(err.contains("does not name a declared route"));
    }

    // ── validate_router_edges_declared ───────────────────────────────────

    fn route_edge(id: &str, from_node: &str, label: &str) -> GraphEdge {
        GraphEdge {
            id: id.to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: from_node.to_string(),
            to_node: "target".to_string(),
            condition: GraphEdgeCondition::Route(label.to_string()),
        }
    }

    #[test]
    fn validate_router_edges_declared_accepts_declared_labels() {
        let edges = vec![route_edge("e1", "router-1", "retry")];
        assert!(validate_router_edges_declared(&two_routes(), "router-1", &edges).is_ok());
    }

    #[test]
    fn validate_router_edges_declared_rejects_undeclared_label() {
        let edges = vec![route_edge("e1", "router-1", "nonexistent")];
        let err = validate_router_edges_declared(&two_routes(), "router-1", &edges).unwrap_err();
        assert!(err.contains("undeclared route 'nonexistent'"));
    }

    #[test]
    fn validate_router_edges_declared_ignores_edges_from_other_nodes() {
        let edges = vec![route_edge("e1", "other-node", "nonexistent")];
        assert!(validate_router_edges_declared(&two_routes(), "router-1", &edges).is_ok());
    }

    #[test]
    fn validate_router_edges_declared_ignores_non_route_edges() {
        let edges = vec![GraphEdge {
            id: "e1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "router-1".to_string(),
            to_node: "target".to_string(),
            condition: GraphEdgeCondition::Always,
        }];
        assert!(validate_router_edges_declared(&two_routes(), "router-1", &edges).is_ok());
    }

    // ── validate_router_route_coverage ───────────────────────────────────

    #[test]
    fn validate_router_route_coverage_unwired_router_is_valid() {
        // A freshly created router with no edges at all yet is a valid,
        // in-progress state — not every declared route needs an edge until
        // wiring has started.
        assert!(validate_router_route_coverage(&two_routes(), "router-1", &[]).is_ok());
    }

    #[test]
    fn validate_router_route_coverage_rejects_partial_coverage() {
        let edges = vec![route_edge("e1", "router-1", "retry")];
        let err = validate_router_route_coverage(&two_routes(), "router-1", &edges).unwrap_err();
        assert!(err.contains("Router route 'escalate' has no outgoing edge."));
    }

    #[test]
    fn validate_router_route_coverage_accepts_full_coverage() {
        let edges = vec![
            route_edge("e1", "router-1", "retry"),
            route_edge("e2", "router-1", "escalate"),
        ];
        assert!(validate_router_route_coverage(&two_routes(), "router-1", &edges).is_ok());
    }

    #[test]
    fn validate_router_route_coverage_ignores_edges_from_other_nodes() {
        // A route edge that exists but belongs to a different node must not
        // count as this router having started wiring — with zero edges of
        // its own, `router-1` is still in the valid "not yet wired" state.
        let edges = vec![route_edge("e1", "other-node", "retry")];
        assert!(validate_router_route_coverage(&two_routes(), "router-1", &edges).is_ok());
    }

    #[test]
    fn graph_hook_event_as_str_roundtrip() {
        assert_eq!(super::GraphHookEvent::OnCompleted.as_str(), "on_completed");
        assert_eq!(super::GraphHookEvent::OnFailed.as_str(), "on_failed");
        assert_eq!(super::GraphHookEvent::OnBlocked.as_str(), "on_blocked");
        assert_eq!(
            super::GraphHookEvent::OnSpecCompleted.as_str(),
            "on_spec_completed"
        );
    }

    #[test]
    fn graph_hook_event_from_str() {
        assert_eq!(
            super::GraphHookEvent::from_str("on_completed"),
            Some(super::GraphHookEvent::OnCompleted)
        );
        assert_eq!(
            super::GraphHookEvent::from_str("on_failed"),
            Some(super::GraphHookEvent::OnFailed)
        );
        assert_eq!(
            super::GraphHookEvent::from_str("on_blocked"),
            Some(super::GraphHookEvent::OnBlocked)
        );
        assert_eq!(
            super::GraphHookEvent::from_str("on_spec_completed"),
            Some(super::GraphHookEvent::OnSpecCompleted)
        );
        assert_eq!(super::GraphHookEvent::from_str("invalid"), None);
    }

    #[test]
    fn graph_hook_event_serde_roundtrip() {
        let events = [
            super::GraphHookEvent::OnCompleted,
            super::GraphHookEvent::OnFailed,
            super::GraphHookEvent::OnBlocked,
            super::GraphHookEvent::OnSpecCompleted,
        ];
        for event in events {
            let json = serde_json::to_string(&event).unwrap();
            let deserialized: super::GraphHookEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(event, deserialized);
        }
    }

    #[test]
    fn graph_edge_condition_from_str_rejects_legacy_break() {
        assert_eq!(GraphEdgeCondition::from_str("break"), None);
        assert_eq!(
            GraphEdgeCondition::from_str("error"),
            Some(GraphEdgeCondition::Error)
        );
        assert_eq!(GraphEdgeCondition::Error.as_str(), "error");
    }

    #[test]
    fn graph_edge_condition_serde_still_reads_legacy_break_for_import() {
        let parsed: GraphEdgeCondition = serde_json::from_str("\"break\"").unwrap();
        assert_eq!(parsed, GraphEdgeCondition::Error);
        assert_eq!(
            serde_json::to_string(&GraphEdgeCondition::Error).unwrap(),
            "\"error\""
        );
    }
}
