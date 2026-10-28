use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
/// path used by both the `graph_control` MCP tool and the scheduler's
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
    /// Spec is bound to a graph (not standalone); spec_edit set_status only
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
    /// not by the work itself failing. Set by reconciliation (in place of the
    /// `git stash` it used to run), or by a no-verdict ensemble exit (FR4) that
    /// pauses the graph with a blocker the operator can clear by rewiring or
    /// retrying the ensemble. In every case the working tree is left exactly
    /// as the interrupted attempt left it, and the spec is picked up again like
    /// any other runnable spec (queue selection treats this exactly like
    /// `Pending`), with its node prompt telling the agent a prior attempt
    /// exists so it continues rather than restarts.
    ///
    /// Contract: `loop_continue` { retry_current_node } resumes a spec in
    /// this status at its cursor (the node/ensemble its last run was on,
    /// via [`GraphSpecStatus::is_resumable`]); `loop_run` restarts it from
    /// the entry node instead. The two verbs stay distinct on purpose — see
    /// CB50.
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

    /// Whether `loop_continue` { retry_current_node } may resume a spec in
    /// this status at its cursor — `Running` (already active) or
    /// `Interrupted` (paused by an explicit interrupt or left behind by
    /// `reconcile_orphaned_graphs`). Any other status (`Pending`, `Failed`,
    /// `Completed`, `Skipped`) is not resumable: nothing has started, or the
    /// spec is finished. The single source of truth for this distinction —
    /// used by both `handle_retry_current_node`
    /// (`daemon::handler`) and `resolve_spec_start` (`graph_engine`) so the
    /// two can't diverge (CB50).
    pub fn is_resumable(self) -> bool {
        matches!(self, Self::Running | Self::Interrupted)
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
pub enum GraphRunStatus {
    Running,
    /// Queued for named mutexes (`mutex` on the node or its ensemble): the
    /// run row exists but no work has started and the node's timeout has not
    /// begun. Transitions to `Running` when every lease is granted.
    Waiting,
    Pass,
    Fail,
    /// Operator-interrupted: the node was explicitly stopped by an operator
    /// via `graph_control(interrupt: true)`, not a failure of the node's work.
    Interrupted,
    /// Quorum/ensemble settled with no filed verdict: every member ended as
    /// an infrastructure failure (crash, unreported, interrupted,
    /// straggler-terminated or timed out). Routes via `Error` edges, never
    /// `Fail`.
    Error,
}

impl GraphRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Interrupted => "interrupted",
            Self::Error => "error",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "waiting" => Self::Waiting,
            "pass" => Self::Pass,
            "fail" => Self::Fail,
            "interrupted" => Self::Interrupted,
            "error" => Self::Error,
            _ => Self::Running,
        }
    }
}
