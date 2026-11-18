//! SPECCODE: pre-spawn validation of a spec's deterministic commit message.

use super::NodeExecution;
use crate::domain::graphs::{GraphNode, GraphRunStatus, GraphSpec};
use crate::domain::spec_type::{check_message, commit_message_opt};
use serde_json::Value;

/// `Some` Fail execution when a committing agent's prebuilt message is
/// malformed — checked before anything spawns, so nothing is committed.
/// `may_commit` is the node's own `commit_rights` or its ensemble's; the
/// node's own config counts too, because the ensemble paths only pass the
/// ensemble's flag and a member may declare the right itself.
pub(crate) fn agent_commit_guard(
    spec: &GraphSpec,
    node: &GraphNode,
    may_commit: bool,
) -> Option<NodeExecution> {
    let may_commit =
        may_commit || node.config.get("commit_rights").and_then(Value::as_bool) == Some(true);
    if !may_commit {
        return None;
    }
    let message = commit_message_opt(spec.spec_type, spec.spec_scope.as_deref(), &spec.name)?;
    let error = check_message(&message).err()?;
    Some(commit_subject_invalid_execution(node, &message, &error))
}

/// SPECCODE: the Fail execution for a committing agent node whose prebuilt
/// commit message is malformed — the agent-path twin of the check-node
/// `commit_subject_invalid` verdict. `spawn_permanent` marks it
/// deterministic (the spec snapshot is fixed for the dispatch, so a retry
/// could never pass): the infra-retry machinery settles it instead of
/// retrying, and the row keeps `failure_kind: "commit_subject_invalid"` so
/// triage can tell it from a gate failure. Nothing spawns and nothing is
/// committed.
pub(crate) fn commit_subject_invalid_execution(
    node: &GraphNode,
    message: &str,
    error: &str,
) -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: serde_json::json!({
            "kind": "agent",
            "node_id": node.id,
            "failure_kind": "commit_subject_invalid",
            "message": message,
            "error": error,
            "spawn_permanent": true,
        }),
        summary: format!(
            "Agent node '{}' has an invalid commit message: {error}.",
            node.name
        ),
    }
}
