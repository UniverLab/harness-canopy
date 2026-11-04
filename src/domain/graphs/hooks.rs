use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::graphs::GraphRunStatus;

/// Config for a graph completion hook — an agent payload
/// (`platform`/`model`/`prompt`), a direct shell command (`command`), an
/// interactive message into a live session (`prompt` + `target_session_id` or
/// `target_session_name`), or a graph launch (`target_graph_id`). Exactly one mode must be configured;
/// the engine validates this at creation time.
///
/// An interactive hook is fire-and-forget: firing it enqueues one due-now
/// row in `scheduled_prompts` for the configured session (`target_session_id`
/// or `target_session_name`, resolved to a live id at fire time), delivered
/// asynchronously by the TUI (which stays queued, not lost, while no TUI is
/// running). It never reads or waits for a reply.
///
/// A graph hook is fire-and-forget: firing it launches another graph in-process
/// without waiting for it (CH4). The launched graph's outcome never changes the
/// launching graph's status. Depth is capped at one: a graph launched by a hook
/// cannot itself launch another graph via hooks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphCompletionHook {
    /// CLI platform for agent hooks (e.g. "mimo", "claude"). `None` for
    /// command and interactive hooks. Mutually exclusive with `crew`: a hook
    /// names either a platform or a crew, never both.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// CM49: crew name for agent hooks, the alternative to `platform`. The
    /// hook resolves its pair from the crew at fire time (first member the
    /// roster reports available). `None` for command/interactive hooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crew: Option<String>,
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
    /// Session *name* an interactive hook delivers to, resolved to a live
    /// session id at fire time (CM27) — the alternative to
    /// `target_session_id` for hooks that must survive a session's id
    /// changing (daemon reinstall, TUI restart) while its name stays the
    /// same. Mutually exclusive with `target_session_id`; exactly one must
    /// be set for an interactive hook. Resolution is never cached: it reads
    /// the same live-session set `session_list` and `target_session_id`
    /// verification both read. Zero or more-than-one live match at fire
    /// time fails the hook loudly (naming the name, or listing every
    /// matching id) rather than guessing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_session_name: Option<String>,
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
        self.platform.is_some() || self.crew.is_some()
    }

    /// Whether this is an interactive hook (enqueues a scheduled send to a
    /// live session). Selected when `target_session_id` and `prompt` are
    /// present while `platform` and `command` are absent.
    pub fn is_interactive(&self) -> bool {
        (self
            .target_session_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty())
            || self
                .target_session_name
                .as_deref()
                .is_some_and(|s| !s.trim().is_empty()))
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
    /// CM45: the hook config that actually fired, captured at fire time. A
    /// later read sees what ran even after `graph_edit` replaces the hook.
    /// `None` for pre-migration rows and for rows whose hook was never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_config: Option<GraphCompletionHook>,
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
