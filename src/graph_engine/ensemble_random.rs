use super::*;
use rand::rngs::StdRng;
use rand::{Rng, RngExt as _, SeedableRng as _};

/// A uniformly shuffled visit order over `0..len` (Fisher-Yates).
///
/// Every dispatch of a `random` ensemble draws its whole candidate order up
/// front, so a member is never repeated within one dispatch and a test can
/// seed the RNG and replay the exact fall-through sequence.
pub(crate) fn draw_order(len: usize, rng: &mut dyn Rng) -> Vec<usize> {
    let mut order: Vec<usize> = (0..len).collect();
    for index in (1..len).rev() {
        let swap_with = rng.random_range(0..=index);
        order.swap(index, swap_with);
    }
    order
}

/// The dispatch RNG: `Some(seed)` for deterministic tests, otherwise a fresh
/// `StdRng` seeded from the thread RNG. Production never sets a seed.
pub(crate) fn random_source(seed: Option<u64>) -> StdRng {
    match seed {
        Some(seed) => StdRng::seed_from_u64(seed),
        None => StdRng::from_rng(&mut rand::rng()),
    }
}

/// Build the random winner join execution (pure). Only a passing member can
/// become the winner — a real failure falls through like any other.
pub(crate) fn random_winner_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    member: &EnsembleMember,
    node: &GraphNode,
    execution: &NodeExecution,
    draw_index: usize,
) -> NodeExecution {
    let join_output = serde_json::json!({
        "kind": "random",
        "ensemble_id": ensemble.id,
        "winner": {
            "node_id": member.node_id,
            "platform": member.platform,
            "model": member.model,
            "status": "pass",
            "output": execution.output.clone(),
        },
        "members_tried": draw_index + 1,
        "members_total": details.members.len(),
    });
    NodeExecution {
        status: execution.status,
        output: join_output,
        summary: format!(
            "Random ensemble '{}' passed (member '{}' at position {}).",
            ensemble.name, node.name, member.position,
        ),
    }
}

/// Build the random terminal (every candidate tried) join execution (pure).
///
/// Unlike cascade, a real failure verdict does not end the walk — so the
/// terminal always reports every tried member and its cause, whether the walk
/// ended on real failures (`Fail`) or only infrastructure crashes
/// (`Error`/`no_verdict`).
pub(crate) fn random_terminal_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    saw_verdict: bool,
    tried_members: &[(String, String, Value, String)],
) -> NodeExecution {
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
    let mut join_output = serde_json::json!({
        "kind": "random",
        "ensemble_id": ensemble.id,
        "members": members,
        "members_tried": tried,
        "members_total": details.members.len(),
    });
    let (status, summary) = if saw_verdict {
        join_output["error"] = serde_json::json!("no member produced a passing verdict");
        (
            GraphRunStatus::Fail,
            format!(
                "Random ensemble '{}' failed: no member produced a passing verdict ({tried} tried).",
                ensemble.name,
            ),
        )
    } else {
        join_output["no_verdict"] = serde_json::json!(true);
        (
            GraphRunStatus::Error,
            format!(
                "Random ensemble '{}' produced no verdict: no member filed a verdict ({tried} tried, all infrastructure failures).",
                ensemble.name,
            ),
        )
    };
    NodeExecution {
        status,
        output: join_output,
        summary,
    }
}

/// Log why the random walk falls through to another candidate.
pub(crate) fn log_random_fallthrough(
    ensemble_id: &str,
    node: &GraphNode,
    member: &EnsembleMember,
    member_had_no_verdict: bool,
) {
    tracing::info!(
        ensemble_id = %ensemble_id,
        member = %node.name,
        position = member.position,
        no_verdict = member_had_no_verdict,
        "random member failed, drawing another"
    );
}

/// Invariant inputs shared by every candidate of one random dispatch.
struct RandomCandidateCtx<'a> {
    lp: &'a crate::domain::graphs::Graph,
    spec: &'a GraphSpec,
    details: &'a EnsembleDetails,
    previous_output: Option<&'a Value>,
    iteration: usize,
    workdir: &'a str,
    ensemble_timeout_minutes: u64,
    node_outputs: &'a HashMap<String, Value>,
    all_node_names: &'a [String],
}

impl GraphEngine {
    /// Resolve and run one candidate of a random dispatch.
    async fn run_random_candidate(
        &self,
        ctx: &RandomCandidateCtx<'_>,
        member: &EnsembleMember,
        member_nodes: &HashMap<String, GraphNode>,
        resumable_sessions: &mut HashMap<String, String>,
    ) -> Result<(GraphNode, NodeExecution, bool)> {
        let node = round_robin_member_node(member_nodes, member)?;
        let (execution, member_had_no_verdict) = self
            .run_ensemble_member(
                ctx.lp,
                ctx.spec,
                &node,
                ctx.previous_output,
                ctx.iteration,
                ctx.workdir,
                member.timeout_minutes,
                ctx.ensemble_timeout_minutes,
                ctx.node_outputs,
                ctx.all_node_names,
                // A resumed dispatch draws fresh: no cascade-style walk
                // start and no resume candidate.
                None,
                resumable_sessions,
                &ctx.details.ensemble.mutex,
                ctx.details.ensemble.commit_rights,
                crate::graph_engine::read_only::member_is_read_only(ctx.details, &node),
            )
            .await?;
        Ok((node, execution, member_had_no_verdict))
    }

    /// Run one `random` ensemble dispatch: draw a uniformly shuffled order
    /// over the roster-available candidates, run them one at a time, and stop
    /// at the first member that passes. Unlike cascade, a real `Fail` verdict
    /// also falls through to the next untried candidate. Every dispatch draws
    /// fresh — there is no resumed counterpart to the cascade walk start.
    #[expect(clippy::too_many_arguments)]
    pub(crate) async fn execute_ensemble_random(
        &self,
        lp: &crate::domain::graphs::Graph,
        spec: &GraphSpec,
        details: &EnsembleDetails,
        available_members: &[EnsembleMember],
        previous_output: Option<&Value>,
        iteration: usize,
        workdir: &str,
        enforce_commit_rights: bool,
        node_outputs: &HashMap<String, Value>,
        all_node_names: &[String],
        resumable_sessions: &mut HashMap<String, String>,
        rng: &mut StdRng,
    ) -> Result<NodeExecution> {
        let ensemble = &details.ensemble;
        let ensemble_timeout_minutes = ensemble.timeout_minutes.max(0) as u64;
        let member_nodes = load_ensemble_member_nodes(&self.db, details)?;
        let commit_watch = CommitRightsWatch::begin(
            enforce_commit_rights,
            details.ensemble.commit_rights,
            workdir,
        )
        .await;
        let previous_output_owned = previous_output.cloned();
        let join_ctx = EnsembleJoinCtx {
            lp,
            spec,
            join_node_id: &ensemble.join_node_id,
            workdir,
            iteration,
            input: previous_output_owned.as_ref(),
        };
        let candidate_ctx = RandomCandidateCtx {
            lp,
            spec,
            details,
            previous_output,
            iteration,
            workdir,
            ensemble_timeout_minutes,
            node_outputs,
            all_node_names,
        };
        let order = draw_order(available_members.len(), rng);
        let mut saw_verdict = false;
        let mut tried_members: Vec<(String, String, Value, String)> = Vec::new();
        for (draw_index, candidate_index) in order.iter().enumerate() {
            let member = &available_members[*candidate_index];
            // CB82: stop before running another member — settle its active run
            // if any, return interrupted with no join row and no advance.
            if self.is_shutting_down() {
                return Ok(self.shutdown_interrupted_member_execution(
                    &member.node_id,
                    format!(
                        "Ensemble '{}' (random member) interrupted: daemon shutdown.",
                        ensemble.name
                    ),
                ));
            }
            let (node, execution, member_had_no_verdict) = self
                .run_random_candidate(&candidate_ctx, member, &member_nodes, resumable_sessions)
                .await?;
            saw_verdict |= !member_had_no_verdict;
            tried_members.push((
                node.name.clone(),
                member.platform.clone(),
                execution.output.clone(),
                execution.summary.clone(),
            ));
            if execution.status == GraphRunStatus::Pass {
                let join_execution = random_winner_execution(
                    ensemble, details, member, &node, &execution, draw_index,
                );
                let join_execution = apply_round_robin_commit_watch(
                    join_execution,
                    &commit_watch,
                    workdir,
                    format!("Ensemble '{}' (random member)", ensemble.name),
                    &ensemble.join_node_id,
                )
                .await;
                self.record_ensemble_join(&join_ctx, &join_execution)
                    .await?;
                return Ok(join_execution);
            }
            log_random_fallthrough(&ensemble.id, &node, member, member_had_no_verdict);
        }
        let execution = random_terminal_execution(ensemble, details, saw_verdict, &tried_members);
        let execution = apply_round_robin_commit_watch(
            execution,
            &commit_watch,
            workdir,
            format!("Ensemble '{}' (random)", ensemble.name),
            &ensemble.join_node_id,
        )
        .await;
        self.record_ensemble_join(&join_ctx, &execution).await?;
        Ok(execution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{Ensemble, EnsembleKind};

    fn ensemble() -> Ensemble {
        Ensemble {
            mutex: Vec::new(),
            id: "ens".to_string(),
            spec_id: Some("spec".to_string()),
            graph_id: None,
            name: "Picker".to_string(),
            prompt_template: "{{spec_content}}".to_string(),
            join_node_id: "join".to_string(),
            entry_from_node: "entry".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "entry".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Random,
            round_robin_index: None,
            commit_rights: false,
            read_only: false,
            created_at: chrono::Utc::now(),
            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        }
    }

    fn member(node_id: &str, position: i64) -> EnsembleMember {
        EnsembleMember {
            ensemble_id: "ens".to_string(),
            node_id: node_id.to_string(),
            position,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    fn details(count: usize) -> EnsembleDetails {
        EnsembleDetails {
            ensemble: ensemble(),
            members: (0..count)
                .map(|index| member(&format!("m{index}"), index as i64))
                .collect(),
        }
    }

    #[test]
    fn seeded_draws_are_uniform_within_tolerance() {
        let mut counts = [0usize; 3];
        for seed in 0..1000u64 {
            let order = draw_order(3, &mut StdRng::seed_from_u64(seed));
            counts[order[0]] += 1;
        }
        for count in counts {
            assert!(
                (250..=420).contains(&count),
                "each member must be drawn first 25-42% of the time: {counts:?}"
            );
        }
    }

    #[test]
    fn a_dispatch_never_repeats_a_member() {
        for seed in 0..100u64 {
            let order = draw_order(5, &mut StdRng::seed_from_u64(seed));
            let mut sorted = order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, vec![0, 1, 2, 3, 4], "got {order:?}");
        }
    }

    #[test]
    fn all_fail_terminal_reports_every_tried_cause() {
        let details = details(3);
        let tried = vec![
            (
                "m0".to_string(),
                "claude".to_string(),
                serde_json::json!({"error": "first reason"}),
                "member m0 failed".to_string(),
            ),
            (
                "m1".to_string(),
                "copilot".to_string(),
                serde_json::json!({"error": "second reason"}),
                "member m1 failed".to_string(),
            ),
            (
                "m2".to_string(),
                "opencode".to_string(),
                serde_json::json!({"error": "third reason"}),
                "member m2 failed".to_string(),
            ),
        ];
        let execution = random_terminal_execution(&details.ensemble, &details, true, &tried);
        assert_eq!(execution.status, GraphRunStatus::Fail);
        assert_eq!(execution.output["kind"], "random");
        assert_eq!(execution.output["members_tried"], 3);
        let members = execution.output["members"].as_array().unwrap();
        assert_eq!(members.len(), 3);
        for (index, reason) in ["first reason", "second reason", "third reason"]
            .iter()
            .enumerate()
        {
            assert_eq!(members[index]["node"], format!("m{index}"));
            assert!(
                members[index]["reason"].as_str().unwrap().contains(reason),
                "every tried member's cause must be reported: {members:?}"
            );
        }
    }

    #[test]
    fn no_verdict_terminal_is_error_and_names_every_member() {
        let details = details(2);
        let tried = vec![
            (
                "m0".to_string(),
                "claude".to_string(),
                serde_json::json!({"error": "crash one"}),
                "member m0 crashed".to_string(),
            ),
            (
                "m1".to_string(),
                "copilot".to_string(),
                serde_json::json!({"error": "crash two"}),
                "member m1 crashed".to_string(),
            ),
        ];
        let execution = random_terminal_execution(&details.ensemble, &details, false, &tried);
        assert_eq!(execution.status, GraphRunStatus::Error);
        assert_eq!(execution.output["no_verdict"], true);
        assert_eq!(execution.output["members"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn winner_output_is_tagged_random() {
        let details = details(3);
        let node = GraphNode {
            id: "m1".to_string(),
            spec_id: None,
            graph_id: None,
            name: "member-2".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 2,
            created_at: chrono::Utc::now(),
        };
        let execution = NodeExecution {
            status: GraphRunStatus::Pass,
            output: serde_json::json!({"text": "ok"}),
            summary: "ok".to_string(),
        };
        let join = random_winner_execution(
            &details.ensemble,
            &details,
            &details.members[1],
            &node,
            &execution,
            1,
        );
        assert_eq!(join.status, GraphRunStatus::Pass);
        assert_eq!(join.output["kind"], "random");
        assert_eq!(join.output["winner"]["node_id"], "m1");
        assert_eq!(join.output["members_tried"], 2);
    }
}
