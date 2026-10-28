//! CM49: crew-binding blockers shared by the engine's dispatch paths.
//!
//! A crew-bound node, hook or pending ensemble blocks naming the crew it was
//! bound to, so the operator can create it (or fix the roster) — mirroring the
//! CM48 node/ensemble blocker shape.

use crate::domain::graphs::GraphRunStatus;

use super::NodeExecution;

/// A crew-bound node/hook/ensemble whose crew does not exist locally (a
/// pending import). Names the crew so the operator can create it.
pub(crate) fn crew_missing_blocker(owner: &str, crew: &str) -> String {
    format!("{owner}: crew '{crew}' does not exist — create it with `crew create`, or unbind it.")
}

/// A crew-bound node/hook whose crew exists but has no member the roster
/// reports available.
pub(crate) fn crew_no_member_blocker(owner: &str, crew: &str) -> String {
    format!("{owner}: crew '{crew}' has no available member (all unavailable on the roster).")
}

/// A `Fail`-shaped verdict for a crew-bound node whose every member is
/// unavailable mid-retry: stop retrying and surface the block naming the crew.
pub(crate) fn crew_exhausted_execution(owner: &str, crew: &str) -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({ "error": "crew_unavailable", "crew": crew }),
        summary: crew_no_member_blocker(owner, crew),
    }
}
