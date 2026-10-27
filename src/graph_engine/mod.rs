use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::Semaphore;

use crate::application::notification_service::{GraphFinishOutcome, NotificationService};
use crate::daemon::process::KILL_GRACE;
use crate::db::scheduled_sends::ScheduledSendProvenance;
use crate::db::Database;
use crate::domain::activity;
use crate::domain::graphs::{
    EnsembleDetails, EnsembleKind, EnsembleMember, Graph, GraphCompletionHook,
    GraphCompletionHookRun, GraphEdge, GraphEdgeCondition, GraphHookEvent, GraphNode,
    GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecDetails, GraphSpecStatus,
    GraphStatus, RouterRoute,
};
use crate::domain::models::Cli;
use crate::domain::sandbox::Sandbox;
use crate::domain::spec_type::{commit_message, SpecType};

mod agent_exec;
mod agent_spawn;
mod check;
mod check_command;
mod check_render;
mod crew_resolve;
mod decision;
mod directed;
mod dirty_commit;
mod dispatch;
mod dispatch_finish;
mod ensemble_cascade;
mod ensemble_exec;
mod ensemble_member;
mod ensemble_member_settle;
mod ensemble_parallel;
mod gate_router;
mod hooks_fire;
mod hooks_graph;
mod hooks_interactive;
mod hooks_render;
mod idea_run;
mod infra;
mod launch_check;
mod lifecycle;
mod mutex_gate;
mod node_attempt;
mod node_dispatch;
mod output_tee;
mod progress;
mod prompts;
mod roster_filter;
mod routing;
mod run_spec;
mod shell_utils;
mod spec_step;
mod verdict_shutdown;

pub(crate) use agent_exec::*;
pub(crate) use agent_spawn::*;
pub(crate) use check::*;
pub(crate) use check_render::*;
pub(crate) use decision::*;
pub(crate) use directed::*;
pub(crate) use dirty_commit::*;
#[allow(unused_imports)]
pub(crate) use dispatch::*;
pub(crate) use ensemble_cascade::*;
#[allow(unused_imports)]
pub(crate) use ensemble_exec::*;
#[allow(unused_imports)]
pub(crate) use ensemble_member::*;
pub(crate) use ensemble_parallel::*;
pub(crate) use gate_router::*;
#[allow(unused_imports)]
pub(crate) use hooks_fire::*;
#[allow(unused_imports)]
pub(crate) use hooks_graph::*;
#[allow(unused_imports)]
pub(crate) use hooks_interactive::*;
pub(crate) use hooks_render::*;
pub(crate) use infra::*;
#[allow(unused_imports)]
pub(crate) use launch_check::*;
#[allow(unused_imports)]
pub(crate) use lifecycle::*;
pub(crate) use mutex_gate::*;
pub(crate) use node_attempt::*;
#[allow(unused_imports)]
pub(crate) use node_dispatch::*;
#[allow(unused_imports)]
pub(crate) use progress::*;
pub(crate) use prompts::*;
pub(crate) use roster_filter::*;
pub(crate) use routing::*;
#[allow(unused_imports)]
pub(crate) use run_spec::*;
pub(crate) use shell_utils::*;
#[allow(unused_imports)]
pub(crate) use spec_step::*;

impl GraphEngine {
    pub fn new(db: Arc<Database>, notification_service: Arc<dyn NotificationService>) -> Self {
        Self {
            db,
            notification_service,
            ensemble_concurrency: Arc::new(Semaphore::new(crate::domain::settings::default_int(
                "ensemble_concurrency_cap",
            ) as usize)),
            dynamic_skills: None,
            spec_attempt_limit: crate::domain::settings::default_int("spec_attempt_limit") as usize,
            shutdown: Arc::new(AtomicBool::new(false)),
            canopy_dir: crate::domain::prompts::canopy_dir(),
        }
    }

    /// Override the default ensemble concurrency cap (F1) — e.g. from
    /// `CanopyConfig::ensemble_concurrency_cap` at daemon startup. `cap` is
    /// floored at 1 so a misconfigured `0` can't wedge every ensemble join
    /// forever.
    pub fn with_ensemble_concurrency_cap(mut self, cap: usize) -> Self {
        self.ensemble_concurrency = Arc::new(Semaphore::new(cap.max(1)));
        self
    }

    /// Override the default cross-run attempt budget (C19) — e.g. from
    /// `CanopyConfig::spec_attempt_limit` at daemon startup. `limit` is
    /// floored at 1 so a misconfigured `0` can't block every spec on its
    /// very first genuine failure.
    pub fn with_spec_attempt_limit(mut self, limit: usize) -> Self {
        self.spec_attempt_limit = limit.max(1);
        self
    }

    /// Give this engine the dynamic skill store (S1) it resolves pinned
    /// `skills` (S2) through at spawn time — e.g. the same store instance
    /// the daemon's `skill_list`/`skill_get` MCP tools use, so there is only
    /// ever one fetch/TTL-cache path for a given skill.
    pub fn with_dynamic_skills(mut self, store: Arc<crate::dynamic_skills::SkillStore>) -> Self {
        self.dynamic_skills = Some(store);
        self
    }

    /// Share the daemon's shutdown flag with this engine (CB82) — the daemon
    /// sets it first in graceful shutdown, before any node process is
    /// SIGTERMed; the engine then finalizes in-flight runs as interrupted
    /// instead of routing their edges.
    pub fn with_shutdown_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.shutdown = flag;
        self
    }

    /// Point this engine at a fixture `.canopy` dir (tests). The
    /// `graphs.max_iterations_per_node` ceiling is read from here at each
    /// iteration check, so a test can set it to 2 and watch the third bounce
    /// block.
    #[cfg(test)]
    pub fn with_canopy_dir(mut self, dir: std::path::PathBuf) -> Self {
        self.canopy_dir = dir;
        self
    }

    pub(crate) fn graph_settings(&self) -> crate::domain::canopy_config::GraphsConfig {
        crate::domain::canopy_config::CanopyConfig::load(&self.canopy_dir).graphs
    }

    pub(crate) fn is_shutting_down(&self) -> bool {
        self.shutdown.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_shutting_down(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

// CONVENTION: Any spec that asks an agent to compile or execute a canopy
// binary MUST instruct the agent to set HOME to a disposable temp directory
// (e.g., `export HOME=$(mktemp -d)`) so the binary builds its own empty
// state instead of touching the real ~/.canopy.

/// Default cap (F1) on ensemble members actually executing at once, across
/// every graph run this engine drives — an 8-member ensemble queues past this
/// many rather than fork-bombing the host. Overridable via
/// [`GraphEngine::with_ensemble_concurrency_cap`]
/// (`CanopyConfig::ensemble_concurrency_cap`); the default lives in the CM51
/// settings registry.
#[derive(Clone)]
pub struct GraphEngine {
    db: Arc<Database>,
    notification_service: Arc<dyn NotificationService>,
    /// Global semaphore (F1) bounding how many ensemble members run
    /// concurrently across every graph this engine drives. Shared (not
    /// per-run) so an 8-member ensemble in one graph can't starve another
    /// graph's ensemble running at the same time — they queue for the same
    /// queue of permits.
    ensemble_concurrency: Arc<Semaphore>,
    /// Backing store (S1) for resolving skills pinned on agent nodes (S2)
    /// at spawn time. `None` in engines built without one (most tests) —
    /// a node's `skills` config is then treated as unresolvable and
    /// degrades to a WARN + note per skill, exactly like a store that's
    /// there but can't reach its sources.
    dynamic_skills: Option<Arc<crate::dynamic_skills::SkillStore>>,
    /// Cross-run attempt budget (C19) — see the settings registry key
    /// `spec_attempt_limit`.
    spec_attempt_limit: usize,
    /// Process-wide daemon-shutdown flag (CB82): set once, as the first step
    /// of graceful shutdown, before any node process is SIGTERMed. Guards in
    /// `run_spec`/`spec_step`/ensemble loops check it before routing any edge
    /// so a shutdown kill never traverses a fail edge.
    shutdown: Arc<AtomicBool>,
    /// Canopy home dir (`~/.canopy` parent) this engine reads
    /// `graphs.*` settings from at the point of use. A path, not a cached
    /// value, so `NextRun`/`Hot` settings stay live. Tests override it with
    /// [`GraphEngine::with_canopy_dir`].
    canopy_dir: std::path::PathBuf,
}

/// Where a spec's sequential graph cursor currently is: at a single ordinary
/// node, or about to fan out into (or having just fanned out into) an
/// ensemble's members. The cursor is a single value at all times — a spec
/// never has two of these in flight — which is what lets `run_spec`'s graph
/// stay a plain `loop { ... }` even though an `Ensemble` step internally runs
/// N member nodes concurrently before it resolves to a single result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpecCursor {
    Node(String),
    /// Ensemble id — resolves to the join's own [`NodeExecution`] once every
    /// member has terminated (F1's wait-all join).
    Ensemble(String),
}

pub(crate) enum SpecExecutionOutcome {
    /// `summary` is the completing node's own summary text — the "one-line
    /// summary" [`render_completion_hook_prompt`]'s `{{completed_specs}}`
    /// placeholder reports for this spec.
    Completed {
        summary: String,
    },
    Paused,
    Failed(String),
    /// This spec's in-flight node run was terminated out from under this
    /// dispatch by the engine itself — a newer attempt at the same node
    /// (B42), a concurrent `graph_reset`, `graph_pause`, iteration-budget
    /// exhaustion, or `fail_graph`'s own sweep — see
    /// `run_was_terminated_out_of_band`. Pure engine bookkeeping, not a node
    /// failure: the dispatch that owned the run stops silently — it routes
    /// down no edge, fails nothing, and completes nothing. Whatever
    /// terminated it (a newer attempt, or the dispatch that won the graph
    /// claim after a reset) is what now drives the graph.
    Superseded,
    /// The spec has paused mid-attempt under a human-visible blocker — surfaced
    /// by [`GraphEngine::block_graph`]/[`GraphEngine::handle_spec_outcome`] — so
    /// someone can inspect and clear it instead of the engine relaunching
    /// blindly. Two sources: (a) C19 — a genuine verdict failure that has now
    /// exceeded its persisted cross-run attempt budget
    /// ([`GraphEngine::spec_attempt_limit`]); the spec is left `Failed`, which
    /// every resume validation refuses (`retry_current_node` and
    /// `skip_next_spec` both, and the scheduler's auto-continue applies the
    /// same two) — the way back in is an explicit `graph_reset` (naming the
    /// spec also clears its cross-run count) followed by a fresh `graph_run`
    /// dispatch, and
    /// (b) FR4 — a parallel/round-robin ensemble exited with no verdict and no
    /// `Error` edge to absorb it; that spec is left `Interrupted`
    /// (resumable via `retry_current_node` without a `graph_reset`) so an
    /// operator can rewire or retry the ensemble. In both cases the `String` is
    /// the blocker text, and the graph is paused with exactly one `on_blocked`
    /// firing per transition.
    Blocked(String),
}

pub(crate) struct NodeExecution {
    status: GraphRunStatus,
    output: Value,
    summary: String,
}

/// (B17) Distinct failure mode for [`GraphEngine::run_graph_dispatch`]'s launch
/// guard: the graph's effective spec set (bound specs, or the given queue's
/// pending members) was empty, so the run never actually launched. Unlike
/// every other error `run_graph_dispatch` can return, this one must never flip
/// the graph to `Failed` — [`GraphEngine::start_background_run`] and
/// [`GraphEngine::resume_background`] downcast for it and skip `fail_graph`,
/// leaving the graph's status exactly as it was before the call.
#[derive(Debug)]
pub(crate) struct EmptySpecSetError(String);

impl std::fmt::Display for EmptySpecSetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EmptySpecSetError {}

/// Immutable inputs for recording an ensemble join row.
pub(crate) struct EnsembleJoinCtx<'a> {
    lp: &'a Graph,
    spec: &'a GraphSpec,
    join_node_id: &'a str,
    workdir: &'a str,
    iteration: usize,
    input: Option<&'a Value>,
}

/// Iteration-budget key distinguishing node and ensemble cursors.
fn budget_key(cursor: &SpecCursor) -> String {
    match cursor {
        SpecCursor::Node(node_id) => node_id.clone(),
        SpecCursor::Ensemble(ensemble_id) => format!("ensemble:{ensemble_id}"),
    }
}
/// Previous-output fallback for one spec iteration: the last stepped node's
/// output, falling back to the resumed spec's own input before the first step.
fn current_previous_output(state: &SpecRunState) -> Option<Value> {
    state
        .previous_node_name
        .as_ref()
        .and_then(|name| state.node_outputs.get(name))
        .cloned()
        .or_else(|| state.resume_previous_output.clone())
}
/// Index graph nodes by id for edge routing lookups.
fn build_nodes_by_id(nodes: &[GraphNode]) -> HashMap<&str, &GraphNode> {
    nodes.iter().map(|node| (node.id.as_str(), node)).collect()
}
/// Immutable per-call inputs of one spec run.
pub(crate) struct SpecStepCtx<'a> {
    lp: &'a Graph,
    spec: &'a GraphSpec,
    workdir: &'a str,
    queue_id: Option<&'a str>,
    enforce_commit_rights: bool,
    edges: &'a [GraphEdge],
    ensembles: &'a [EnsembleDetails],
    nodes_by_id: &'a HashMap<&'a str, &'a GraphNode>,
    all_node_names: &'a [String],
    spec_start_head: Option<&'a str>,
    spec_group: Option<&'a str>,
}
/// Mutable cursor/loop state carried across spec iterations.
pub(crate) struct SpecRunState {
    cursor: SpecCursor,
    node_outputs: HashMap<String, Value>,
    resume_previous_output: Option<Value>,
    iterations: HashMap<String, usize>,
    previous_node_name: Option<String>,
    spec_committed_head: Option<String>,
    resumable_sessions: HashMap<String, String>,
    round_robin_pins: HashMap<String, i64>,
}
/// Settled result of one cursor step: the execution plus routing inputs.
pub(crate) struct NodeStepResult {
    execution: NodeExecution,
    from_node_id: String,
    run_id: Option<String>,
    had_infra_crash: bool,
}
/// Control flow out of one cursor step.
pub(crate) enum SpecStepFlow {
    Routed(NodeStepResult),
    Done(SpecExecutionOutcome),
}
/// Control flow out of post-step routing.
pub(crate) enum SpecLoopFlow {
    Continue,
    Done(SpecExecutionOutcome),
}
/// How a spec-graph load resolved.
pub(crate) enum SpecGraphLoad<'a> {
    Ready {
        nodes: &'a [GraphNode],
        edges: &'a [GraphEdge],
        ensembles: Vec<EnsembleDetails>,
    },
    NoNodes(SpecExecutionOutcome),
}

#[cfg(test)]
mod tests;
