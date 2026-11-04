use super::*;

/// Outcome of one attempt of an ensemble member: either the attempt produced
/// an execution to settle against `is_infra_crash`, or it produced no verdict
/// (a hard error or the member's own timeout) and the member is done.
pub(crate) enum MemberAttempt {
    Execution(NodeExecution),
    NoVerdict(NodeExecution),
}

/// Run one cascade/round-robin member attempt under the member's own full
/// timeout. (Moved here from `ensemble_member.rs`, pure move plus the
/// `report_nudge` parameter, to keep that file under the 800-line limit.)
pub(crate) async fn run_member_attempt(
    ctx: &EnsembleMemberContext,
    member_run_id: &str,
    resume_candidate: Option<&str>,
    report_nudge: bool,
) -> Result<MemberAttempt> {
    let attempt_outcome = tokio::time::timeout(
        std::time::Duration::from_secs(ctx.bound_minutes * 60),
        execute_agent_node(
            &ctx.db,
            &ctx.lp,
            &ctx.spec,
            &ctx.node,
            ctx.previous_output.as_ref(),
            member_run_id,
            &ctx.workdir,
            resume_candidate,
            false,
            report_nudge,
            ctx.dynamic_skills.as_ref(),
            &ctx.node_outputs,
            &ctx.all_node_names,
        ),
    )
    .await;

    match attempt_outcome {
        Ok(Ok(execution)) => Ok(MemberAttempt::Execution(execution)),
        // A hard error inside this attempt — the member never
        // produced a verdict, so the caller moves on to the next one.
        Ok(Err(error)) => Ok(MemberAttempt::NoVerdict(NodeExecution {
            status: GraphRunStatus::Fail,
            output: serde_json::json!({ "error": error.to_string() }),
            summary: format!("Ensemble member '{}' failed: {error}", ctx.node.name),
        })),
        // This attempt's own full `bound_minutes` elapsed — kill
        // whatever's live now (B12) and settle this member as failed.
        // Never retried: a timeout is a hard kill, not a crash shape
        // to feed back into `is_infra_crash`.
        Err(_elapsed) => {
            let (terminate_reason, source) =
                GraphEngine::member_timeout_source(ctx.member_timeout_minutes);
            if let Ok(Some(run)) = ctx.db.get_active_graph_run_for_node(&ctx.node.id) {
                terminate_run_row(&ctx.db, &run, terminate_reason, &ctx.workdir);
            }
            Ok(MemberAttempt::NoVerdict(NodeExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "kind": "agent",
                    "node_id": ctx.node.id,
                    "error": "member timeout",
                    "timeout_minutes": ctx.bound_minutes,
                }),
                summary: format!(
                    "Ensemble member '{}' timed out: {source} ({}m) elapsed.",
                    ctx.node.name, ctx.bound_minutes
                ),
            }))
        }
    }
}
