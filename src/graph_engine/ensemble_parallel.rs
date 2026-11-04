use super::*;

/// Build the round-robin winner join execution (pure).
pub(crate) fn round_robin_winner_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    member: &EnsembleMember,
    node: &GraphNode,
    execution: &NodeExecution,
    offset: i64,
    next_index: i64,
) -> NodeExecution {
    let join_status = execution.status;
    let members_tried = offset + 1;
    let status_str = if join_status == GraphRunStatus::Pass {
        "pass"
    } else {
        "fail"
    };
    let join_output = serde_json::json!({
        "kind": "round_robin",
        "ensemble_id": ensemble.id,
        "member": {
            "node_id": member.node_id,
            "platform": member.platform,
            "model": member.model,
            "position": member.position,
            "status": status_str,
            "output": execution.output.clone(),
        },
        "next_index": next_index,
        "winner": {
            "node_id": member.node_id,
            "platform": member.platform,
            "model": member.model,
            "status": status_str,
            "output": execution.output.clone(),
        },
        "members_tried": members_tried,
        "members_total": details.members.len(),
    });
    NodeExecution {
        status: join_status,
        output: join_output,
        summary: format!(
            "Round-robin ensemble '{}' {} (member '{}' at position {}).",
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

/// Build the round-robin all-no-verdict join execution (pure).
pub(crate) fn round_robin_no_verdict_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    tried_no_verdict: &[(String, String, Value, String)],
    next_index: i64,
) -> NodeExecution {
    let members: Vec<Value> = tried_no_verdict
        .iter()
        .map(|(name, platform, output, summary)| {
            serde_json::json!({
                "node": name,
                "platform": platform,
                "reason": no_verdict_reason(output, summary),
            })
        })
        .collect();
    let join_output = serde_json::json!({
        "kind": "round_robin",
        "ensemble_id": ensemble.id,
        "no_verdict": true,
        "members": members,
        "next_index": next_index,
        // CM48: the members actually tried — a member stepped over as
        // unavailable never ran and must not be counted as tried.
        "members_tried": tried_no_verdict.len(),
        "members_total": details.members.len(),
    });
    NodeExecution {
        status: GraphRunStatus::Error,
        output: join_output,
        summary: format!(
            "Round-robin ensemble '{}' produced no verdict: all {} members failed without filing a verdict.",
            ensemble.name,
            details.members.len(),
        ),
    }
}

/// Apply the round-robin commit-rights override (no warn log, as before).
pub(crate) async fn apply_round_robin_commit_watch(
    mut join_execution: NodeExecution,
    commit_watch: &Option<CommitRightsWatch>,
    workdir: &str,
    label: String,
    join_node_id: &str,
) -> NodeExecution {
    let Some(watch) = commit_watch else {
        return join_execution;
    };
    let Some(head_after) = watch.violation(workdir).await else {
        return join_execution;
    };
    join_execution = commit_rights_failure(
        &label,
        join_node_id,
        &watch.head_before,
        &head_after,
        workdir,
        join_execution.output,
    )
    .await;
    join_execution
}

/// Ordered tally of parallel ensemble member results.
pub(crate) struct Consolidation {
    pub(crate) passed: i64,
    pub(crate) verdicts: i64,
    pub(crate) doc: String,
    pub(crate) summaries: Vec<Value>,
    pub(crate) no_verdict: Vec<Value>,
}

/// Owned inputs for one spawned parallel ensemble member task.
pub(crate) struct ParallelMemberTask {
    pub(crate) db: Arc<Database>,
    pub(crate) lp: Graph,
    pub(crate) spec: GraphSpec,
    pub(crate) node: GraphNode,
    pub(crate) previous_output: Option<Value>,
    pub(crate) workdir: String,
    pub(crate) semaphore: Arc<Semaphore>,
    pub(crate) label: String,
    pub(crate) ensemble_id: String,
    pub(crate) dynamic_skills: Option<Arc<crate::dynamic_skills::SkillStore>>,
    pub(crate) node_outputs: HashMap<String, Value>,
    pub(crate) all_node_names: Vec<String>,
    pub(crate) resume_candidate_seed: Option<String>,
    pub(crate) run_id: String,
    pub(crate) iteration: usize,
    pub(crate) straggler_minutes: u64,
    /// Mutex set this member acquires ahead of every attempt: its node's own
    /// plus the ensemble's.
    pub(crate) mutex_resources: Vec<String>,
    /// The engine's shutdown flag — the task is spawned detached, so the flag
    /// travels in the task.
    pub(crate) shutdown: Arc<AtomicBool>,
}

/// Outcome of one member's straggler-bounded retry sequence.
pub(crate) type MemberAttemptOutcome = Result<
    Result<(NodeExecution, GraphNodeRun, String, bool), anyhow::Error>,
    tokio::time::error::Elapsed,
>;

/// Settle one member's straggler-bounded retry outcome into a verdict triple.
pub(crate) async fn settle_parallel_member_outcome(
    outcome: MemberAttemptOutcome,
    db: &Database,
    node: &GraphNode,
    ensemble_id: &str,
    straggler_minutes: u64,
) -> (NodeExecution, Option<String>, bool) {
    match outcome {
        Ok(Ok((execution, run, final_run_id, no_verdict))) => {
            tracing::info!(
                run_id = %final_run_id,
                status = ?execution.status,
                node = %node.name,
                ensemble_id = %ensemble_id,
                "ensemble member run completed"
            );
            let session_id = run.session_id.clone();
            let execution = if matches!(
                run.status,
                GraphRunStatus::Running | GraphRunStatus::Waiting
            ) {
                let _ = db.update_graph_run_result(
                    &final_run_id,
                    execution.status,
                    Some(&execution.output),
                    Some(chrono::Utc::now()),
                );
                execution
            } else {
                let output = super::verdict_shutdown::merge_process_record(
                    db,
                    &final_run_id,
                    &run,
                    &execution.output,
                );
                NodeExecution {
                    status: run.status,
                    output,
                    summary: execution.summary,
                }
            };
            (execution, session_id, no_verdict)
        }
        Ok(Err(error)) => {
            let output = db
                .get_active_graph_run_for_node(&node.id)
                .ok()
                .flatten()
                .and_then(|run| run.output)
                .unwrap_or_else(|| serde_json::json!({ "error": error.to_string() }));
            (
                NodeExecution {
                    status: GraphRunStatus::Fail,
                    output,
                    summary: format!("Ensemble member '{}' failed: {error}", node.name),
                },
                None,
                true,
            )
        }
        Err(_elapsed) => {
            if let Ok(Some(run)) = db.get_active_graph_run_for_node(&node.id) {
                let workdir = crate::graph_engine::routing::workdir_for_run(db, &run);
                terminate_run_row(db, &run, "ensemble straggler timeout", &workdir);
            }
            (
                NodeExecution {
                    status: GraphRunStatus::Fail,
                    output: serde_json::json!({
                        "kind": "agent",
                        "node_id": node.id,
                        "error": "straggler timeout",
                        "straggler_timeout_minutes": straggler_minutes,
                    }),
                    summary: format!(
                        "Ensemble member '{}' killed: straggler timeout after {straggler_minutes}m.",
                        node.name
                    ),
                },
                None,
                true,
            )
        }
    }
}

/// Drive one parallel member through its straggler-bounded infra-retry sequence.
pub(crate) async fn run_parallel_member_task(task: ParallelMemberTask) -> MemberJoinResult {
    let _permit = task
        .semaphore
        .clone()
        .acquire_owned()
        .await
        .expect("ensemble concurrency semaphore is never closed");
    // B26: same B19 infra-crash retry as a lone agent node, inside the ONE
    // straggler timeout; a member still crashing when the window expires
    // counts as failed deterministically.
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(task.straggler_minutes * 60),
        drive_parallel_member_attempts(&task),
    )
    .await;
    let (execution, member_session_id, member_had_no_verdict) = settle_parallel_member_outcome(
        outcome,
        &task.db,
        &task.node,
        &task.ensemble_id,
        task.straggler_minutes,
    )
    .await;
    publish_parallel_member_outcome(&task.db, &task, &execution);
    (
        task.node.id.clone(),
        task.label.clone(),
        execution,
        member_session_id,
        member_had_no_verdict,
    )
}

/// Drive one member's infra-retry loop to a verdict: acquire the member's
/// mutexes ahead of every attempt, execute, and retry infra crashes —
/// returning `(execution, run, final_run_id, member_had_no_verdict)`. The
/// caller bounds this future with the straggler timeout.
async fn drive_parallel_member_attempts(
    task: &ParallelMemberTask,
) -> Result<(NodeExecution, GraphNodeRun, String, bool)> {
    let mut chain = AttemptChain::from_node(&task.node);
    let mut member_run_id = task.run_id.clone();
    let mut resume_candidate: Option<String> = task.resume_candidate_seed.clone();
    loop {
        // The mutex gate sits ahead of every member attempt, inside
        // the straggler window: the member's own agent timeout starts
        // at the grant, while the join's straggler bound covers the
        // member's total time including waits (and kills a member
        // still waiting when it expires). A refused wait ends the
        // member with no verdict.
        if !acquire_mutexes_before_run(
            &task.db,
            &task.shutdown,
            &task.lp,
            &task.spec.id,
            &task.node,
            &member_run_id,
            &task.mutex_resources,
        )
        .await?
        {
            let run = task.db.get_graph_run(&member_run_id)?.ok_or_else(|| {
                anyhow!("Graph run '{member_run_id}' was deleted while waiting for its mutex.")
            })?;
            break Ok((
                interrupted_execution(format!(
                    "Ensemble member '{}' stopped while waiting for its mutex.",
                    task.node.name
                )),
                run,
                member_run_id.clone(),
                true,
            ));
        }
        let execution = execute_agent_node(
            &task.db,
            &task.lp,
            &task.spec,
            &task.node,
            task.previous_output.as_ref(),
            &member_run_id,
            &task.workdir,
            resume_candidate.as_deref(),
            false,
            chain.nudged,
            task.dynamic_skills.as_ref(),
            &task.node_outputs,
            &task.all_node_names,
        )
        .await?;
        let run = task
            .db
            .get_graph_run(&member_run_id)?
            .ok_or_else(|| anyhow!("Graph run '{member_run_id}' not found after execution."))?;
        match chain.decide(&task.node, &execution, &run) {
            AttemptRetry::Settle => {
                let mut execution = execution;
                let mut run = run;
                let member_had_no_verdict = finalize_infra_crash_shape(
                    &task.db,
                    &member_run_id,
                    &task.node,
                    &mut execution,
                    &mut run,
                    chain.attempt,
                    chain.retry_limit,
                    chain.crash_max_secs,
                )
                .await?;
                break Ok((execution, run, member_run_id.clone(), member_had_no_verdict));
            }
            kind => {
                resume_candidate = run.session_id.clone();
                member_run_id = begin_infra_retry(
                    &task.db,
                    &task.lp,
                    &task.spec,
                    &task.node,
                    task.previous_output.as_ref(),
                    task.iteration as i64,
                    &member_run_id,
                    &execution.output,
                    chain.attempt,
                    chain.backoff_secs,
                    &task.mutex_resources,
                )
                .await?;
                chain.advance(kind);
                continue;
            }
        }
    }
}

/// Publish one settled member's pass/fail activity line.
pub(crate) fn publish_parallel_member_outcome(
    db: &Database,
    task: &ParallelMemberTask,
    execution: &NodeExecution,
) {
    let (platform, model) = executed_pair_for_node(&task.node);
    activity::record(
        db,
        activity::GraphEvent {
            graph_id: &task.lp.id,
            graph_name: &task.lp.name,
            workdir: &task.workdir,
            spec_id: Some(&task.spec.id),
            node_id: Some(&task.node.id),
            run_id: Some(&task.run_id),
            kind: activity::GraphEventKind::NodeFinished,
            message: match execution.status {
                GraphRunStatus::Pass => format!("Ensemble member '{}' passed.", task.node.name),
                _ => format!(
                    "Ensemble member '{}' failed: {}",
                    task.node.name,
                    failure_reason_text(&execution.output, &execution.summary).unwrap_or_default()
                ),
            },
            payload: Some(serde_json::json!({
                "verdict": execution.status.as_str(),
                "platform": platform,
                "model": model,
                "duration_s": Value::Null,
            })),
        },
    );
}

/// Immutable inputs for collecting parallel ensemble member results.
pub(crate) struct ParallelCollectCtx<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) ensemble: &'a crate::domain::graphs::Ensemble,
    pub(crate) details: &'a EnsembleDetails,
    pub(crate) member_nodes: &'a HashMap<String, GraphNode>,
    pub(crate) workdir: &'a str,
}

/// One settled parallel member: node id, label, execution, session, no-verdict flag.
pub(crate) type MemberJoinResult = (String, String, NodeExecution, Option<String>, bool);

/// Drain every task before consolidating (wait-all, F1).
pub(crate) async fn drain_join_set(
    set: &mut tokio::task::JoinSet<MemberJoinResult>,
) -> Result<HashMap<String, (String, NodeExecution, Option<String>, bool)>> {
    let mut results = HashMap::new();
    while let Some(joined) = set.join_next().await {
        let (node_id, label, execution, session_id, had_no_verdict) =
            joined.map_err(|error| anyhow!("Ensemble member task panicked: {error}"))?;
        results.insert(node_id, (label, execution, session_id, had_no_verdict));
    }
    Ok(results)
}

/// Record one grace-path completion and open the grace window when quorum is met.
pub(crate) fn record_completed_member(
    results: &mut HashMap<String, (String, NodeExecution, Option<String>, bool)>,
    passed_so_far: &mut i64,
    completed: Option<MemberJoinResult>,
    ensemble_min_pass: i64,
    grace: i64,
    quorum_met_at: &mut Option<chrono::DateTime<chrono::Utc>>,
    grace_deadline: &mut Option<tokio::time::Instant>,
) {
    let Some((node_id, label, execution, session_id, had_no_verdict)) = completed else {
        return;
    };
    if execution.status == GraphRunStatus::Pass {
        *passed_so_far += 1;
    }
    results.insert(node_id, (label, execution, session_id, had_no_verdict));
    if grace_deadline.is_none() && *passed_so_far >= ensemble_min_pass {
        *quorum_met_at = Some(chrono::Utc::now());
        *grace_deadline = Some(
            tokio::time::Instant::now() + std::time::Duration::from_secs(grace.max(0) as u64 * 60),
        );
    }
}

/// Immutable inputs for finishing a parallel ensemble join.
pub(crate) struct ParallelFinishCtx<'a> {
    pub(crate) lp: &'a Graph,
    pub(crate) spec: &'a GraphSpec,
    pub(crate) ensemble: &'a crate::domain::graphs::Ensemble,
    pub(crate) details: &'a EnsembleDetails,
    pub(crate) workdir: &'a str,
    pub(crate) iteration: usize,
    pub(crate) input: Option<&'a Value>,
    pub(crate) quorum_met_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) quorum_grace: Option<i64>,
    pub(crate) commit_watch: &'a Option<CommitRightsWatch>,
}

/// Consolidate parallel member results in member position order.
pub(crate) fn consolidate_parallel_results(
    details: &EnsembleDetails,
    member_nodes: &HashMap<String, GraphNode>,
    results: &mut HashMap<String, (String, NodeExecution, Option<String>, bool)>,
    resumable_sessions: &mut HashMap<String, String>,
) -> Result<Consolidation> {
    let mut passed = 0i64;
    let mut verdicts = 0i64;
    let mut consolidated_doc = String::new();
    let mut member_summaries = Vec::with_capacity(details.members.len());
    let mut no_verdict_members = Vec::with_capacity(details.members.len());
    for member in &details.members {
        let (label, execution, session_id, had_no_verdict) =
            results.remove(&member.node_id).ok_or_else(|| {
                anyhow!(
                    "Ensemble member '{}' produced no result after wait-all.",
                    member.node_id
                )
            })?;
        if !had_no_verdict {
            verdicts += 1;
        }
        let status_label = if execution.status == GraphRunStatus::Pass {
            passed += 1;
            "pass"
        } else {
            "fail"
        };
        if execution.status == GraphRunStatus::Pass {
            if let Some(sid) = session_id {
                resumable_sessions.insert(member.node_id.clone(), sid);
            }
        } else {
            resumable_sessions.remove(&member.node_id);
        }
        consolidated_doc.push_str(&format!(
            "## {label} [{status_label}]\n\n{}\n\n",
            member_output_text(&execution.output)
        ));
        let member_node_name = member_nodes
            .get(&member.node_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| member.node_id.clone());
        no_verdict_members.push(serde_json::json!({
            "node": member_node_name,
            "platform": member.platform,
            "reason": no_verdict_reason(&execution.output, &execution.summary),
        }));
        member_summaries.push(serde_json::json!({
            "node_id": member.node_id,
            "platform": member.platform,
            "model": member.model,
            "status": status_label,
            "output": execution.output,
        }));
    }
    Ok(Consolidation {
        passed,
        verdicts,
        doc: consolidated_doc,
        summaries: member_summaries,
        no_verdict: no_verdict_members,
    })
}

/// Build the quorum no-verdict join execution (pure).
pub(crate) fn quorum_no_verdict_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    tally: &Consolidation,
    quorum_met_at: Option<chrono::DateTime<chrono::Utc>>,
    quorum_grace: Option<i64>,
) -> NodeExecution {
    let mut join_output = serde_json::json!({
        "kind": "quorum",
        "ensemble_id": ensemble.id,
        "members": tally.no_verdict,
        "no_verdict": true,
        "passed": 0,
        "min_pass": ensemble.min_pass,
        "consolidated_doc": tally.doc,
    });
    if let Some(met_at) = quorum_met_at {
        join_output["quorum_met_at"] = serde_json::json!(met_at.to_rfc3339());
        join_output["grace_minutes"] = serde_json::json!(quorum_grace.unwrap_or(0));
    }
    NodeExecution {
        status: GraphRunStatus::Error,
        output: join_output,
        summary: format!(
            "Ensemble '{}' produced no verdict ({} members, all infrastructure failures).",
            ensemble.name,
            details.members.len(),
        ),
    }
}

/// Build the normal quorum join execution (pure, pre-commit).
pub(crate) fn quorum_execution(
    ensemble: &crate::domain::graphs::Ensemble,
    details: &EnsembleDetails,
    tally: &Consolidation,
    quorum_met_at: Option<chrono::DateTime<chrono::Utc>>,
    quorum_grace: Option<i64>,
) -> NodeExecution {
    let join_status = if tally.passed >= ensemble.min_pass {
        GraphRunStatus::Pass
    } else {
        GraphRunStatus::Fail
    };
    let mut join_output = serde_json::json!({
        "kind": "quorum",
        "ensemble_id": ensemble.id,
        "members": tally.summaries,
        "passed": tally.passed,
        "min_pass": ensemble.min_pass,
        "consolidated_doc": tally.doc,
    });
    if let Some(met_at) = quorum_met_at {
        join_output["quorum_met_at"] = serde_json::json!(met_at.to_rfc3339());
        join_output["grace_minutes"] = serde_json::json!(quorum_grace.unwrap_or(0));
    }
    NodeExecution {
        status: join_status,
        output: join_output,
        summary: format!(
            "Ensemble '{}' {} ({}/{} passed).",
            ensemble.name,
            if join_status == GraphRunStatus::Pass {
                "passed"
            } else {
                "failed"
            },
            tally.passed,
            details.members.len(),
        ),
    }
}

/// Apply the parallel-quorum commit-rights override (warns, as before).
pub(crate) async fn apply_parallel_commit_watch(
    mut execution: NodeExecution,
    commit_watch: &Option<CommitRightsWatch>,
    workdir: &str,
    ensemble_name: &str,
    join_node_id: &str,
) -> NodeExecution {
    let Some(watch) = commit_watch else {
        return execution;
    };
    let Some(head_after) = watch.violation(workdir).await else {
        return execution;
    };
    tracing::warn!(
        ensemble = %ensemble_name,
        head_before = %watch.head_before,
        head_after = %head_after,
        "ensemble without commit rights: HEAD moved during its run"
    );
    execution = commit_rights_failure(
        &format!("Ensemble '{ensemble_name}' (one of its members)"),
        join_node_id,
        &watch.head_before,
        &head_after,
        workdir,
        execution.output,
    )
    .await;
    execution
}

/// Look up one round-robin member node from the preloaded map.
pub(crate) fn round_robin_member_node(
    member_nodes: &HashMap<String, GraphNode>,
    member: &EnsembleMember,
) -> Result<GraphNode> {
    member_nodes
        .get(&member.node_id)
        .cloned()
        .ok_or_else(|| anyhow!("Ensemble member node '{}' not found.", member.node_id))
}

/// Record a no-verdict fall-through for one round-robin member.
pub(crate) fn push_round_robin_no_verdict(
    tried_no_verdict: &mut Vec<(String, String, Value, String)>,
    ensemble_id: &str,
    node: &GraphNode,
    member: &EnsembleMember,
    execution: &NodeExecution,
) {
    tracing::info!(
        ensemble_id = %ensemble_id,
        member = %node.name,
        position = member.position,
        "round-robin member infra-crashed, trying next"
    );
    tried_no_verdict.push((
        node.name.clone(),
        member.platform.clone(),
        execution.output.clone(),
        execution.summary.clone(),
    ));
}
