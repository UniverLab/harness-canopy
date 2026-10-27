use super::*;

/// Resolve the cascade walk start (CM26 bounce support).
pub(crate) fn cascade_walk_start(
    details: &EnsembleDetails,
    resumable_sessions: &HashMap<String, String>,
) -> (usize, Option<String>) {
    let resume_start = details
        .members
        .iter()
        .position(|m| resumable_sessions.contains_key(&m.node_id));
    let initial_resume_candidate =
        resume_start.and_then(|p| resumable_sessions.get(&details.members[p].node_id).cloned());
    (resume_start.unwrap_or(0), initial_resume_candidate)
}

/// CM26 FR3 predicate: only the resumed starting member's own genuine `Fail`
/// continues the walk like "no verdict".
pub(crate) fn cascade_is_resumed_start_failed(
    is_resume_start: bool,
    candidate_is_some: bool,
    member_had_no_verdict: bool,
    status: GraphRunStatus,
) -> bool {
    is_resume_start && candidate_is_some && !member_had_no_verdict && status != GraphRunStatus::Pass
}

/// Build the cascade winner join execution (pure).
pub(crate) fn cascade_winner_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    member: &EnsembleMember,
    node: &GraphNode,
    execution: &NodeExecution,
    offset: usize,
    walk_start: usize,
) -> NodeExecution {
    let join_status = execution.status;
    let join_output = serde_json::json!({
        "kind": "cascade",
        "ensemble_id": ensemble.id,
        "winner": {
            "node_id": member.node_id,
            "platform": member.platform,
            "model": member.model,
            "status": if join_status == GraphRunStatus::Pass { "pass" } else { "fail" },
            "output": execution.output.clone(),
        },
        "members_tried": (offset - walk_start) + 1,
        "members_total": details.members.len(),
    });
    NodeExecution {
        status: join_status,
        output: join_output,
        summary: format!(
            "Cascade ensemble '{}' {} (member '{}' at position {}).",
            ensemble.name,
            if join_status == GraphRunStatus::Pass {
                "passed"
            } else {
                "failed"
            },
            node.name,
            member.position,
        ),
    }
}

/// Build the cascade terminal (exhausted walk) join execution (pure).
pub(crate) fn cascade_terminal_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    walk_start: usize,
    saw_verdict: bool,
    tried_members: &[(String, String, Value, String)],
) -> NodeExecution {
    if !saw_verdict {
        let members: Vec<Value> = tried_members
            .iter()
            .map(|(name, platform, output, summary)| {
                serde_json::json!({
                    "node": name,
                    "platform": platform,
                    "reason": no_verdict_reason(output, summary),
                })
            })
            .collect();
        let tried = tried_members.len();
        let join_output = serde_json::json!({
            "kind": "cascade",
            "ensemble_id": ensemble.id,
            "no_verdict": true,
            "members": members,
            "members_tried": tried,
            "members_total": details.members.len(),
        });
        return NodeExecution {
            status: GraphRunStatus::Error,
            output: join_output,
            summary: format!(
                "Cascade ensemble '{}' produced no verdict: no member from position {} onward filed a verdict ({} tried, all infrastructure failures).",
                ensemble.name,
                walk_start + 1,
                tried,
            ),
        };
    }
    let join_output = serde_json::json!({
        "kind": "cascade",
        "ensemble_id": ensemble.id,
        "error": "no member produced a passing verdict",
        "members_tried": details.members.len() - walk_start,
        "members_total": details.members.len(),
    });
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: join_output,
        summary: format!(
            "Cascade ensemble '{}' failed: no member from position {} onward produced a passing verdict.",
            ensemble.name,
            walk_start + 1,
        ),
    }
}

/// Log why the cascade walk falls through to the next member.
pub(crate) fn log_cascade_fallthrough(
    ensemble_id: &str,
    node: &GraphNode,
    member: &EnsembleMember,
    resumed_start_failed: bool,
) {
    if resumed_start_failed {
        tracing::info!(
            ensemble_id = %ensemble_id,
            member = %node.name,
            position = member.position,
            "cascade resumed start member failed on retry, trying next"
        );
    } else {
        tracing::info!(
            ensemble_id = %ensemble_id,
            member = %node.name,
            position = member.position,
            "cascade member infra-crashed, trying next"
        );
    }
}

/// Inputs for inserting one parallel ensemble member run row.
pub(crate) struct MemberRunSpec<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) spec: &'a GraphSpec,
    pub(crate) node: &'a GraphNode,
    pub(crate) member: &'a EnsembleMember,
    pub(crate) previous_output: Option<&'a Value>,
    pub(crate) iteration: usize,
    pub(crate) ensemble_id: &'a str,
    pub(crate) ensemble_mutex: &'a [String],
}

/// Load every member node fresh (CM15), shared by all ensemble kinds.
pub(crate) fn load_ensemble_member_nodes(
    db: &Database,
    details: &EnsembleDetails,
) -> Result<HashMap<String, GraphNode>> {
    let mut member_nodes: HashMap<String, GraphNode> = HashMap::new();
    for member in &details.members {
        let n = db
            .get_graph_node(&member.node_id)
            .map_err(|e| {
                anyhow!(
                    "Ensemble member node '{}' lookup failed: {e}",
                    member.node_id
                )
            })?
            .ok_or_else(|| anyhow!("Ensemble member node '{}' not found.", member.node_id))?;
        member_nodes.insert(member.node_id.clone(), n);
    }
    Ok(member_nodes)
}
