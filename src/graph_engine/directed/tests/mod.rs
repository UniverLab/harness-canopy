use super::*;
use crate::domain::graphs::{Ensemble, EnsembleKind};

fn node(id: &str, name: &str, position: i64, config: Value) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: None,
        graph_id: Some("g".to_string()),
        name: name.to_string(),
        kind: GraphNodeKind::Agent,
        config,
        position,
        created_at: chrono::Utc::now(),
    }
}

fn edge(from: &str, to: &str, condition: GraphEdgeCondition) -> GraphEdge {
    GraphEdge {
        id: format!("e-{from}-{to}"),
        spec_id: None,
        graph_id: Some("g".to_string()),
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition,
    }
}

fn run(
    node_id: &str,
    status: GraphRunStatus,
    output: Option<Value>,
    input: Option<Value>,
) -> GraphNodeRun {
    GraphNodeRun {
        id: format!("run-{node_id}"),
        graph_id: "g".to_string(),
        spec_id: "spec-1".to_string(),
        node_id: node_id.to_string(),
        status,
        input,
        output,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    }
}

fn spec(id: &str) -> GraphSpec {
    GraphSpec {
        intent: None,
        id: id.to_string(),
        graph_id: Some("g".to_string()),
        name: "S".to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Pending,
        started_at: None,
        completed_at: None,
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: None,
        completed_via_reason: None,
        completed_via_at: None,
        spec_code: None,
        spec_scope: None,
        spec_type: None,
        review_state: crate::domain::graphs::SpecReviewState::Approved,
        review_tier: None,
        review_findings: None,
        created_by_session: None,
    }
}

fn ensemble_details(
    id: &str,
    name: &str,
    entry: &str,
    members: &[&str],
    join: &str,
) -> EnsembleDetails {
    EnsembleDetails {
        ensemble: Ensemble {
            mutex: Vec::new(),
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("g".to_string()),
            name: name.to_string(),
            prompt_template: "do the thing {{output:A}}".to_string(),
            join_node_id: join.to_string(),
            entry_from_node: entry.to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: members.len() as i64,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 10,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "done".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            commit_rights: false,
            read_only: false,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: members
            .iter()
            .enumerate()
            .map(|(position, node_id)| EnsembleMember {
                ensemble_id: id.to_string(),
                node_id: node_id.to_string(),
                position: position as i64,
                platform: "opencode".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect(),
    }
}

fn abc_graph() -> (Vec<GraphNode>, Vec<GraphEdge>) {
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("B", "B", 1, Value::Null),
        node("C", "C", 2, Value::Null),
    ];
    let edges = vec![
        edge("A", "B", GraphEdgeCondition::Pass),
        edge("B", "C", GraphEdgeCondition::Pass),
    ];
    (nodes, edges)
}

/// The node/edge shape of a round-robin ensemble sitting between A and B:
/// A → m1/m2/m3 → j → B.
fn round_robin_between_a_and_b() -> (Vec<GraphNode>, Vec<GraphEdge>, Vec<EnsembleDetails>) {
    let nodes = vec![
        node("A", "A", 0, Value::Null),
        node("m1", "M1", 1, Value::Null),
        node("m2", "M2", 2, Value::Null),
        node("m3", "M3", 3, Value::Null),
        node("j", "Join", 4, Value::Null),
        node("B", "B", 5, Value::Null),
    ];
    let edges = vec![
        edge("A", "m1", GraphEdgeCondition::Always),
        edge("A", "m2", GraphEdgeCondition::Always),
        edge("A", "m3", GraphEdgeCondition::Always),
        edge("m1", "j", GraphEdgeCondition::Always),
        edge("m2", "j", GraphEdgeCondition::Always),
        edge("m3", "j", GraphEdgeCondition::Always),
        edge("j", "B", GraphEdgeCondition::Pass),
    ];
    let mut details = ensemble_details("ens", "rr", "A", &["m1", "m2", "m3"], "j");
    details.ensemble.kind = EnsembleKind::RoundRobin;
    details.ensemble.min_pass = 1;
    (nodes, edges, vec![details])
}

mod looped;
mod quorum;
mod resolve;
mod upstream;
