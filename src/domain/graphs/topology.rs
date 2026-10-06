use serde::{Deserialize, Serialize};

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
