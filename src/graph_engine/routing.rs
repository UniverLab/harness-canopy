use super::*;

pub(crate) fn find_entry_node(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    spec_name: &str,
) -> Result<String> {
    let incoming = edges
        .iter()
        .map(|edge| edge.to_node.as_str())
        .collect::<HashSet<_>>();
    let entry_nodes = nodes
        .iter()
        .filter(|node| !incoming.contains(node.id.as_str()))
        .collect::<Vec<_>>();

    match entry_nodes.as_slice() {
        [entry] => Ok(entry.id.clone()),
        // Every node has an incoming edge: the graph is a retry cycle (e.g.
        // implement <-> review). There is no source node, so fall back to the
        // designated start — the node with the lowest position.
        [] => nodes
            .iter()
            .min_by_key(|node| node.position)
            .map(|node| node.id.clone())
            .ok_or_else(|| anyhow!("Spec '{}' has no nodes.", spec_name)),
        _ => bail!("Spec '{}' has multiple entry nodes.", spec_name),
    }
}

/// Result of [`select_next_step`]: the next cursor plus the edge condition
/// that matched (needed for B43 lifecycle logging).
#[derive(Debug)]
pub(crate) struct StepSelection {
    pub(crate) cursor: SpecCursor,
    pub(crate) edge_condition: GraphEdgeCondition,
}

/// Cursor for a single matching edge, which normally steps to its target as
/// `SpecCursor::Node`. When that target is a member of an ensemble, the engine
/// must fan out through `execute_ensemble` instead: a 1-member
/// cascade/round_robin ensemble is a legal configuration
/// (`validate_ensemble_members` allows min 1) whose entry node has exactly one
/// outgoing edge — the one to its sole member. Resolving that edge to a bare
/// node would bypass the ensemble entirely, and the member's own edge into the
/// `Join` node would then be dispatched as an ordinary node, which the engine
/// refuses ("Quorum node '...' cannot execute directly").
fn cursor_for_single_target(ensembles: &[EnsembleDetails], to_node: &str) -> SpecCursor {
    for details in ensembles {
        if details
            .members
            .iter()
            .any(|member| member.node_id == to_node)
        {
            return SpecCursor::Ensemble(details.ensemble.id.clone());
        }
    }
    SpecCursor::Node(to_node.to_string())
}

/// Resolve the next graph step from `from_node`'s outgoing edges matching
/// `status`. Ordinarily a single matching edge (or several identical-target
/// edges) resolves to [`SpecCursor::Node`]. Multiple *distinct* targets are
/// ambiguous — unless they are exactly one ensemble's full member set, in
/// which case this is F1's fan-out point and resolves to
/// [`SpecCursor::Ensemble`] instead of erroring.
pub(crate) fn select_next_step(
    edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
    from_node: &str,
    status: GraphRunStatus,
) -> Result<Option<StepSelection>> {
    let matching = edges
        .iter()
        .filter(|edge| edge.from_node == from_node)
        .filter(|edge| match status {
            GraphRunStatus::Pass => {
                edge.condition == GraphEdgeCondition::Pass
                    || edge.condition == GraphEdgeCondition::Always
            }
            GraphRunStatus::Fail | GraphRunStatus::Interrupted => {
                edge.condition == GraphEdgeCondition::Fail
                    || edge.condition == GraphEdgeCondition::Always
            }
            GraphRunStatus::Error => edge.condition == GraphEdgeCondition::Error,
            GraphRunStatus::Running => false,
        })
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [] => Ok(None),
        [edge] => Ok(Some(StepSelection {
            cursor: cursor_for_single_target(ensembles, &edge.to_node),
            edge_condition: edge.condition.clone(),
        })),
        _ => {
            let distinct_targets = matching
                .iter()
                .map(|edge| edge.to_node.as_str())
                .collect::<HashSet<_>>();
            if distinct_targets.len() == 1 {
                let to_node = *distinct_targets.iter().next().expect("len == 1");
                return Ok(Some(StepSelection {
                    cursor: SpecCursor::Node(to_node.to_string()),
                    edge_condition: matching[0].condition.clone(),
                }));
            }
            for details in ensembles {
                let member_ids: HashSet<&str> = details
                    .members
                    .iter()
                    .map(|member| member.node_id.as_str())
                    .collect();
                if member_ids == distinct_targets {
                    return Ok(Some(StepSelection {
                        cursor: SpecCursor::Ensemble(details.ensemble.id.clone()),
                        edge_condition: matching[0].condition.clone(),
                    }));
                }
            }
            bail!("Node '{}' has ambiguous outgoing edges.", from_node)
        }
    }
}

/// CM2: like [`select_next_step`], but matches edges by an explicit condition
/// rather than by run status. Used to find `Error` edges after an
/// infrastructure failure — the engine knows there was an infra crash, but
/// the agent declared nothing, so only the engine can resolve this edge.
pub(crate) fn select_next_step_with_condition(
    edges: &[GraphEdge],
    ensembles: &[EnsembleDetails],
    from_node: &str,
    condition: &GraphEdgeCondition,
) -> Result<Option<StepSelection>> {
    let matching = edges
        .iter()
        .filter(|edge| edge.from_node == from_node)
        .filter(|edge| edge.condition == *condition)
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [] => Ok(None),
        [edge] => Ok(Some(StepSelection {
            cursor: cursor_for_single_target(ensembles, &edge.to_node),
            edge_condition: edge.condition.clone(),
        })),
        _ => {
            let distinct_targets = matching
                .iter()
                .map(|edge| edge.to_node.as_str())
                .collect::<HashSet<_>>();
            if distinct_targets.len() == 1 {
                let to_node = *distinct_targets.iter().next().expect("len == 1");
                return Ok(Some(StepSelection {
                    cursor: SpecCursor::Node(to_node.to_string()),
                    edge_condition: matching[0].condition.clone(),
                }));
            }
            for details in ensembles {
                let member_ids: HashSet<&str> = details
                    .members
                    .iter()
                    .map(|member| member.node_id.as_str())
                    .collect();
                if member_ids == distinct_targets {
                    return Ok(Some(StepSelection {
                        cursor: SpecCursor::Ensemble(details.ensemble.id.clone()),
                        edge_condition: matching[0].condition.clone(),
                    }));
                }
            }
            bail!(
                "Node '{}' has ambiguous outgoing '{}' edges.",
                from_node,
                condition.as_str()
            )
        }
    }
}

/// Resolve a router node's next graph step: the edge out of `from_node`
/// whose declared route matches `route_label` exactly (see
/// [`GraphEdgeCondition::Route`]). Used instead of [`select_next_step`] only
/// when the node just executed is a [`GraphNodeKind::Router`] that finished
/// `Pass` — a router's own verdict carries no notion of success/failure, only
/// a choice among its declared routes, so Pass/Fail/Always-conditioned edges
/// never apply here. A failed router (spawn failure/timeout) instead falls
/// through to `select_next_step` like any other node, per its fail-edge
/// contract.
pub(crate) fn select_router_step(
    edges: &[GraphEdge],
    from_node: &str,
    route_label: &str,
) -> Result<Option<StepSelection>> {
    let matching = edges
        .iter()
        .filter(|edge| edge.from_node == from_node)
        .filter(|edge| edge.condition.route_label() == Some(route_label))
        .collect::<Vec<_>>();

    match matching.as_slice() {
        [] => Ok(None),
        [edge] => Ok(Some(StepSelection {
            cursor: SpecCursor::Node(edge.to_node.clone()),
            edge_condition: edge.condition.clone(),
        })),
        _ => {
            let distinct_targets = matching
                .iter()
                .map(|edge| edge.to_node.as_str())
                .collect::<HashSet<_>>();
            if distinct_targets.len() == 1 {
                let to_node = *distinct_targets.iter().next().expect("len == 1");
                return Ok(Some(StepSelection {
                    cursor: SpecCursor::Node(to_node.to_string()),
                    edge_condition: matching[0].condition.clone(),
                }));
            }
            bail!(
                "Router node '{}' has ambiguous outgoing edges for route '{}'.",
                from_node,
                route_label
            )
        }
    }
}

/// Every node id belonging to a cursor step — one for [`SpecCursor::Node`],
/// or every member plus the join for [`SpecCursor::Ensemble`]. Used to reap
/// stale `running` rows (B12) across a whole ensemble fan-out, not just one
/// node.
pub(crate) fn cursor_node_ids(cursor: &SpecCursor, ensembles: &[EnsembleDetails]) -> Vec<String> {
    match cursor {
        SpecCursor::Node(node_id) => vec![node_id.clone()],
        SpecCursor::Ensemble(ensemble_id) => ensembles
            .iter()
            .find(|details| &details.ensemble.id == ensemble_id)
            .map(|details| {
                details
                    .members
                    .iter()
                    .map(|member| member.node_id.clone())
                    .chain(std::iter::once(details.ensemble.join_node_id.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// Verbatim failure reason for an activity line: prefers the structured
/// `error`/`failure_kind` fields the engine itself stamps (`is_infra_crash`
/// family — see `src/graph_engine.rs:4309-4327`), falls back to the node's
/// own summary.
pub(crate) fn failure_reason_text(output: &serde_json::Value, summary: &str) -> Option<String> {
    if let Some(reason) = output.get("reason").and_then(serde_json::Value::as_str) {
        return Some(reason.to_string());
    }
    if let Some(err) = output.get("error").and_then(serde_json::Value::as_str) {
        return Some(err.to_string());
    }
    if let Some(kind) = output
        .get("failure_kind")
        .and_then(serde_json::Value::as_str)
    {
        return Some(kind.to_string());
    }
    if summary.trim().is_empty() {
        None
    } else {
        Some(summary.to_string())
    }
}

/// First line of `failure_reason_text(output, summary)` (or `summary`),
/// trimmed and truncated to 200 chars; never empty (falls back to `summary`
/// or "no reason recorded"). Used for the per-member `reason` in a
/// no-verdict join output.
pub(crate) fn no_verdict_reason(output: &Value, summary: &str) -> String {
    let text = failure_reason_text(output, summary).unwrap_or_else(|| summary.to_string());
    let first = text.lines().next().unwrap_or("").trim();
    let fallback = if summary.trim().is_empty() {
        "no reason recorded".to_string()
    } else {
        summary.lines().next().unwrap_or("").trim().to_string()
    };
    let chosen = if first.is_empty() {
        fallback
    } else {
        first.to_string()
    };
    let mut out: String = chosen.chars().take(200).collect();
    out = out.trim().to_string();
    if out.is_empty() {
        "no reason recorded".to_string()
    } else {
        out
    }
}

/// Human-readable label for a cursor step, for failure summaries.
pub(crate) fn cursor_label(cursor: &SpecCursor, ensembles: &[EnsembleDetails]) -> String {
    match cursor {
        SpecCursor::Node(node_id) => format!("node '{node_id}'"),
        SpecCursor::Ensemble(ensemble_id) => ensembles
            .iter()
            .find(|details| &details.ensemble.id == ensemble_id)
            .map(|details| format!("ensemble '{}'", details.ensemble.name))
            .unwrap_or_else(|| format!("ensemble '{ensemble_id}'")),
    }
}

/// Reason recorded on a node run terminated because a newer attempt at the
/// same node superseded it (B42). One of several reasons
/// [`run_was_terminated_out_of_band`] recognises — see that function's doc
/// for why every one of them is treated identically.
pub(crate) const SUPERSEDE_REASON: &str = "superseded by a new attempt at this node";

/// Whether `run` was terminated by the engine out from under the dispatch
/// that owned it — a same-node supersede ([`SUPERSEDE_REASON`], B42), a
/// concurrent `graph_reset`, `graph_pause`, iteration-budget exhaustion, or
/// `fail_graph`'s own sweep — rather than a genuine node outcome (a clean
/// exit, or a self-report via `graph_complete_node`). Every one of those
/// paths is pure engine bookkeeping, not a node failure: the dispatch that
/// owned the run must recognise it and stop silently here, routing down no
/// edge, failing nothing, and completing nothing (see its use in
/// [`GraphEngine::run_spec`]) — exactly what let a stale dispatch's late
/// completion route a fail edge and take down a healthy sibling dispatch on
/// 2026-08-05.
///
/// Recognised by the `{ "terminated": true, "reason": … }` marker every one
/// of those paths writes via [`terminate_run_row`] (or, for `graph_reset`,
/// the identically-shaped write in [`crate::db::Database::reset_graph`]) —
/// matched on the `terminated` key alone, not a specific reason string, so
/// nothing that terminates a run out-of-band can be missed here. A genuine
/// agent output can never be mistaken for one: self-reports never set this
/// key.
///
/// CB82 also recognises `{ "interrupted": true }`, written by the graceful-
/// shutdown finalizer and the shutdown sweep — a late completion from a
/// shutdown-killed run must likewise route no edge. Genuine node outputs
/// never set either key, so genuine fails still route their fail edge.
pub(crate) fn run_was_terminated_out_of_band(run: &GraphNodeRun) -> bool {
    let Some(output) = run.output.as_ref() else {
        return false;
    };
    output.get("terminated").and_then(Value::as_bool) == Some(true)
        || output.get("interrupted").and_then(Value::as_bool) == Some(true)
}

/// CB82 output marker for a run cut short by daemon shutdown: `Fail` status
/// with this output, never routed down any edge. Same shape as the crash
/// path's `reconcile_orphan_run` marker, so resume/reconcile code treats
/// graceful-shutdown rows exactly like crash rows.
pub(crate) fn shutdown_interrupted_output() -> Value {
    serde_json::json!({ "interrupted": true, "reason": "daemon shutdown" })
}

/// CB82 execution naming a shutdown interruption — a `Fail`-shaped verdict
/// so rows settle uniformly, always paired with
/// [`shutdown_interrupted_output`].
pub(crate) fn interrupted_execution(summary: String) -> NodeExecution {
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: shutdown_interrupted_output(),
        summary,
    }
}

impl GraphEngine {
    /// CB82: finalize `run_id` as interrupted by daemon shutdown and stop
    /// silently — route no edge, fail nothing. The dispatch's outcome is
    /// `Superseded`, whose dispatch semantics are exactly this.
    pub(crate) fn finalize_shutdown_interrupted(&self, run_id: &str) -> SpecExecutionOutcome {
        let _ = self.db.update_graph_run_result(
            run_id,
            GraphRunStatus::Fail,
            Some(&shutdown_interrupted_output()),
            Some(chrono::Utc::now()),
        );
        SpecExecutionOutcome::Superseded
    }
}

/// Resolve the workdir for a run from its graph row — the same directory the
/// harness was launched in. CB84: the session-end command runs with this as
/// cwd, and the title lookup is scoped to it. A blank result just means the
/// session end is skipped (same as a platform with no `session_end_args`).
pub(crate) fn workdir_for_run(db: &Database, run: &GraphNodeRun) -> String {
    db.get_graph(&run.graph_id)
        .ok()
        .flatten()
        .map(|g| g.workdir)
        .unwrap_or_default()
}

/// Best-effort termination (B12) of `run`'s OS process, if it still has one
/// recorded, and finalization of its DB row as `Fail`. Free-function core of
/// [`GraphEngine::terminate_run`] — also used by ensemble member tasks, which
/// don't have a `&GraphEngine` to call the method on.
///
/// CB84: after the process group is killed, best-effort end the harness
/// session behind this run (the opencode v2 service-side session that keeps
/// ticking after the client is gone). `workdir` is the run's workdir — the
/// session-end command runs with it as cwd, and the title lookup is scoped to
/// it. Never fails the caller: the session end is spawned detached with its
/// own 30s timeout.
pub(crate) fn terminate_run_row(db: &Database, run: &GraphNodeRun, reason: &str, workdir: &str) {
    tracing::info!(
        run_id = %run.id,
        node_id = %run.node_id,
        reason,
        "node run terminated"
    );
    if let Some(pid) = run.pid {
        crate::daemon::process::terminate_process_group_async(pid, KILL_GRACE);
    }
    let _ = db.update_graph_run_result(
        &run.id,
        GraphRunStatus::Fail,
        Some(&serde_json::json!({ "terminated": true, "reason": reason })),
        Some(chrono::Utc::now()),
    );
    // CB84: end the harness session (best-effort, async, detached) and record
    // the outcome on the run row.
    let platform = run.executed_platform.clone().unwrap_or_default();
    let run_clone = run.clone();
    let workdir = workdir.to_string();
    let run_id = run.id.clone();
    let db_clone = db.clone();
    if !platform.is_empty() {
        tokio::spawn(async move {
            let outcome = end_harness_session(&platform, &run_clone, &workdir).await;
            record_session_end(&db_clone, &run_id, &outcome).await;
        });
    }
}

/// CB84: end the harness session behind a terminated node run. Returns a
/// String outcome for recording on the run row. Best-effort — never fails
/// the caller.
pub(crate) async fn end_harness_session(
    platform: &str,
    run: &GraphNodeRun,
    workdir: &str,
) -> String {
    let Some(cli_config) = load_cli_config(platform) else {
        return "not configured".to_string();
    };
    if cli_config.session_end_args.is_none() {
        return "not configured".to_string();
    }
    let strategy = crate::domain::cli_strategy::CliStrategy::from_cli_config(&cli_config);

    let session_id = resolve_session_id(&strategy, run, workdir, &cli_config).await;
    let Some(session_id) = session_id else {
        return "not found".to_string();
    };

    run_session_end_command(&strategy, &session_id, workdir, &run.id).await
}

/// Resolve the session id for a run: from the run row first, then by title
/// lookup. If title lookup finds nothing and the platform has session_list_args,
/// retry with the home directory (for sessions created before the cwd fix).
async fn resolve_session_id(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    run: &GraphNodeRun,
    workdir: &str,
    cli_config: &crate::domain::cli_config::CliConfig,
) -> Option<String> {
    if let Some(sid) = &run.session_id {
        return Some(sid.clone());
    }

    let by_title = lookup_session_id_by_title(strategy, workdir, &run.id).await;
    if by_title.is_some() {
        return by_title;
    }

    // Retry with home directory if the platform has session_list_args
    // (covers sessions created before the cwd fix).
    if cli_config.session_list_args.is_some() {
        if let Some(home) = dirs::home_dir() {
            let home_str = home.to_string_lossy().to_string();
            if let Some(id) = lookup_session_id_by_title(strategy, &home_str, &run.id).await {
                return Some(id);
            }
        }
    }

    None
}

/// Run the platform's session-end command with a 30s timeout. Returns the
/// outcome string for recording.
async fn run_session_end_command(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    session_id: &str,
    workdir: &str,
    run_id: &str,
) -> String {
    let Some(mut cmd) = strategy.build_session_end_command(session_id, workdir) else {
        return "failed: binary not resolved".to_string();
    };
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let timeout = std::time::Duration::from_secs(30);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => classify_session_end_output(&output, session_id, run_id),
        Ok(Err(e)) => {
            tracing::warn!(
                run_id = %run_id,
                session_id = %session_id,
                error = %e,
                "session end command failed"
            );
            format!("failed: {}", e)
        }
        Err(_) => {
            tracing::warn!(
                run_id = %run_id,
                session_id = %session_id,
                "session end command timed out"
            );
            "failed: timed out".to_string()
        }
    }
}

/// Turn a finished session-end invocation into the recorded outcome: success
/// is `ended <id>`, a non-zero exit is `failed: exit <code>`.
fn classify_session_end_output(
    output: &std::process::Output,
    session_id: &str,
    run_id: &str,
) -> String {
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        tracing::warn!(
            run_id = %run_id,
            session_id = %session_id,
            exit_code = %code,
            "session end command failed"
        );
        return format!("failed: exit {}", code);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let preview = stdout.chars().take(200).collect::<String>();
    tracing::info!(
        run_id = %run_id,
        session_id = %session_id,
        exit_code = ?output.status.code(),
        stdout_preview = %preview,
        "harness session ended"
    );
    format!("ended {}", session_id)
}

/// Load a CLI config by platform name from `~/.canopy/config.toml`.
fn load_cli_config(platform_name: &str) -> Option<crate::domain::cli_config::CliConfig> {
    let canopy_dir = crate::domain::prompts::canopy_dir();
    let config = crate::domain::canopy_config::CanopyConfig::load(&canopy_dir);
    config.get_cli(platform_name).cloned()
}

/// CB84 + verdict shutdown: end a terminated harness's session and record the
/// outcome on its run row, awaiting completion (unlike the detached path used
/// by `terminate_run_row`). Reuses `end_harness_session` + `record_session_end`.
pub(crate) async fn end_harness_session_for_verdict(
    db: &Database,
    platform: &str,
    run: &GraphNodeRun,
    workdir: &str,
    run_id: &str,
) -> String {
    let outcome = end_harness_session(platform, run, workdir).await;
    record_session_end(db, run_id, &outcome).await;
    outcome
}

/// Record the session end outcome on the run's output JSON. Best-effort:
/// errors are ignored so the caller is never delayed.
pub(crate) async fn record_session_end(db: &Database, run_id: &str, outcome: &str) {
    let Ok(Some(run)) = db.get_graph_run(run_id) else {
        return;
    };
    let mut output = run.output.clone().unwrap_or_default();
    if !output.is_object() {
        output = serde_json::Value::Object(serde_json::Map::new());
    }
    output["session_end"] = serde_json::Value::String(outcome.to_string());
    let _ = db.update_graph_run_result(run_id, run.status, Some(&output), None);
}

/// Look up a session id by title: run the platform's session-list command
/// (cwd = workdir) and find the entry whose `title` equals `canopy-<run_id>`.
/// Returns `None` when the list command isn't configured, fails, times out,
/// or the JSON is malformed — never panics.
async fn lookup_session_id_by_title(
    strategy: &crate::domain::cli_strategy::CliStrategy,
    workdir: &str,
    run_id: &str,
) -> Option<String> {
    let mut cmd = strategy.build_session_list_command_by_title(workdir)?;
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output())
        .await
        .ok()
        .and_then(|r| r.ok())?;
    find_session_id_by_title(&String::from_utf8_lossy(&output.stdout), run_id)
}

/// Parse a session-list JSON array and return the id of the entry whose
/// `title` equals `canopy-<run_id>`. Returns `None` on malformed JSON, a
/// missing or non-string `id`/`title`, or no match — never panics.
fn find_session_id_by_title(json: &str, run_id: &str) -> Option<String> {
    let sessions: Vec<serde_json::Value> = serde_json::from_str(json).ok()?;
    let target_title = format!("canopy-{}", run_id);
    sessions.iter().find_map(|s| {
        let title = s.get("title")?.as_str()?;
        let id = s.get("id")?.as_str()?;
        (title == target_title).then(|| id.to_string())
    })
}

/// `"platform #N"` or `"platform/model #N"` (1-based `N` from the member's
/// position) — the label used in an ensemble's consolidated
/// `"## <label> [pass|fail]"` sections and in the TUI's collapsed ensemble
/// view. The position suffix is always included, not just when it would
/// disambiguate: a multi-angle panel commonly runs several members on the
/// same platform/model with only their `prompt_override` differing, so
/// platform/model alone can name the same label for every section — the
/// position is what actually attributes a section to one member.
pub(crate) fn member_label(member: &EnsembleMember) -> String {
    let base = match member.model.as_deref().map(str::trim) {
        Some(model) if !model.is_empty() => format!("{}/{}", member.platform, model),
        _ => member.platform.clone(),
    };
    format!("{base} #{}", member.position + 1)
}

/// The human-readable text to carry into an ensemble's consolidated doc for
/// one member's output — its stdout when it produced any, the recorded
/// error when it didn't, else the raw output JSON.
pub(crate) fn member_output_text(output: &Value) -> String {
    if let Some(stdout) = output.get("stdout").and_then(Value::as_str) {
        if !stdout.trim().is_empty() {
            return stdout.to_string();
        }
    }
    if let Some(error) = output.get("error").and_then(Value::as_str) {
        return format!("(error: {error})");
    }
    serde_json::to_string_pretty(output).unwrap_or_default()
}

pub(crate) fn should_advance_to_next_spec(node: &GraphNode, status: GraphRunStatus) -> bool {
    let route_key = match status {
        GraphRunStatus::Pass => "pass_route",
        GraphRunStatus::Fail | GraphRunStatus::Interrupted => "fail_route",
        GraphRunStatus::Running | GraphRunStatus::Error => return false,
    };

    node.kind == GraphNodeKind::Gate
        && node
            .config
            .get(route_key)
            .and_then(Value::as_str)
            .is_some_and(|route| route == "next_spec")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_session_id_by_title_matches() {
        let json = r#"[{"id":"ses_1","title":"canopy-run-abc"},{"id":"ses_2","title":"other"}]"#;
        assert_eq!(
            find_session_id_by_title(json, "run-abc"),
            Some("ses_1".to_string())
        );
    }

    #[test]
    fn find_session_id_by_title_no_match() {
        let json = r#"[{"id":"ses_1","title":"other"}]"#;
        assert_eq!(find_session_id_by_title(json, "run-abc"), None);
    }

    #[test]
    fn find_session_id_by_title_malformed_json_returns_none() {
        assert_eq!(find_session_id_by_title("not valid json", "run-abc"), None);
    }

    #[test]
    fn find_session_id_by_title_ignores_entries_missing_fields() {
        // An entry with a matching title but no `id`, and one with an id but
        // a non-string title, must not panic — just be skipped.
        let json = r#"[{"title":"canopy-run-abc"},{"id":"ses_2","title":7}]"#;
        assert_eq!(find_session_id_by_title(json, "run-abc"), None);
    }
}
