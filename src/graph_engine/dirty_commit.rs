use super::*;

/// The workdir's current `git rev-parse HEAD`, or `None` if it isn't a git
/// repo (or the command otherwise fails). Never errors the caller — a check
/// node that references `{{spec_start_head}}` in a non-git workdir just sees
/// an empty string and decides for itself, per `execute_check_node`.
pub(crate) async fn capture_workdir_head(workdir: &str) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(workdir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!head.is_empty()).then_some(head)
}

pub(crate) async fn capture_workdir_dirty(workdir: &str) -> Option<i64> {
    let output = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workdir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).lines().count() as i64)
}

/// CM29: same `git status --porcelain` call as `capture_workdir_dirty`, but
/// also keeps the first 20 paths (stripped of the 2-character status prefix
/// and the following space) for the failure report. Used only at spec end
/// and by the dirty-start refusal check — never per node.
pub(crate) async fn capture_workdir_dirty_with_paths(workdir: &str) -> (Option<i64>, Vec<String>) {
    let Ok(output) = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workdir)
        .output()
        .await
    else {
        return (None, Vec::new());
    };
    if !output.status.success() {
        return (None, Vec::new());
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let all_paths: Vec<String> = text
        .lines()
        .map(|line| line.get(3..).unwrap_or(line).trim().to_string())
        .collect();
    let count = all_paths.len() as i64;
    (Some(count), all_paths.into_iter().take(20).collect())
}

/// CM29/CB69: the result of `GraphEngine::dirty_start_check` — `refuse ==
/// true` means the caller must not launch (the message then carries the
/// `not launched` sentence and the `allow_dirty_start` hint);
/// `refuse == false` means launch — the graph has `allow_dirty_start` set,
/// or the next spec is resuming the dirt it left itself — but surface
/// `message` to the operator.
pub struct DirtyStartNotice {
    pub refuse: bool,
    pub message: String,
}

/// CM29: the one-line dirty-tree note folded onto a failed/interrupted
/// spec's terminal report and (for `on_failed`) the hook's `{{node}}`
/// payload. `None` when the tree was clean (or unknown — non-git workdir).
pub(crate) fn dirty_tree_note(dirty: Option<i64>, paths: &[String]) -> Option<String> {
    let count = dirty.filter(|n| *n > 0)?;
    let plural = if count == 1 { "y" } else { "ies" };
    Some(format!(
        "worktree left dirty: {count} entr{plural}: {}",
        paths.join(", ")
    ))
}

/// CB39: whether `start_head` is still an ancestor of the workdir's current
/// HEAD. Returns `Some(true)` when it is (the invariant holds), `Some(false)`
/// when it is not (history was rewritten past the spec's baseline), and `None`
/// when the check cannot be performed (non-git workdir or missing start head)
/// — the caller skips verification in that case.
pub(crate) async fn check_start_head_ancestry(workdir: &str, start_head: &str) -> Option<bool> {
    let output = Command::new("git")
        .arg("merge-base")
        .arg("--is-ancestor")
        .arg(start_head)
        .arg("HEAD")
        .current_dir(workdir)
        .output()
        .await
        .ok()?;
    Some(output.status.success())
}

/// Whether this node is a designated committer (B37): its own
/// `commit_rights: true` config, or — for a node owned by an ensemble —
/// that ensemble's own `commit_rights` flag (CM28). Never inferred from
/// name, kind, or prompt. A member's own config never carries
/// `commit_rights`: the right is the ensemble's, resolved through
/// `ensemble_owning_node`.
pub(crate) fn node_has_commit_rights(node: &GraphNode, ensembles: &[EnsembleDetails]) -> bool {
    if node.config.get("commit_rights").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    match ensemble_owning_node(&node.id, ensembles) {
        Some(ensemble_id) => ensembles
            .iter()
            .any(|d| d.ensemble.id == ensemble_id && d.ensemble.commit_rights),
        None => false,
    }
}

/// Whether a graph opts into commit-rights enforcement (B37) — i.e. whether
/// any of its nodes (or the ensemble owning it) declares `commit_rights:
/// true`.
///
/// Enforcement is per-graph opt-in on purpose. A graph that designates nobody
/// cannot be told apart from one whose committer simply predates this key, so
/// enforcing there would fail exactly the node the graph relies on to land
/// work. Once ONE node or ensemble declares the right, the graph's intent is
/// unambiguous and every other node in it is held to it.
pub(crate) fn graph_enforces_commit_rights(
    nodes: &[GraphNode],
    ensembles: &[EnsembleDetails],
) -> bool {
    nodes.iter().any(|n| node_has_commit_rights(n, ensembles))
}

/// A pre/post `git rev-parse HEAD` comparison around one node's execution
/// (B37). Deterministic and cheap — two `git rev-parse` calls, no LLM — and
/// entirely absent (`begin` yields `None`) for the cases that must not change
/// behavior: graphs that designate no committer, the designated committer
/// itself, and non-git workdirs.
///
/// A prompt-level "you have no commit rights" rule has been broken by three
/// different models (a haiku implementer on 2026-07-16, an
/// opencode/mimo-v2.5-free implementer committing `21109a5` on 2026-07-18
/// against a caps-locked HARD RULE). The cascade is what makes it costly: the
/// work lands in history, the reviewer ensemble then reviews an empty or
/// formatting-only working diff, and the graph's quality gate silently becomes
/// a no-op.
pub(crate) struct CommitRightsWatch {
    pub(crate) head_before: String,
}

impl CommitRightsWatch {
    /// Start watching, or `None` if there is nothing to watch.
    pub(crate) async fn begin(enforced: bool, may_commit: bool, workdir: &str) -> Option<Self> {
        if !enforced || may_commit {
            return None;
        }
        capture_workdir_head(workdir)
            .await
            .map(|head_before| Self { head_before })
    }

    /// The HEAD the watched node left behind, if it moved history. `None`
    /// when HEAD is unchanged — including a node that edited files without
    /// committing, which is the normal, unaffected case.
    pub(crate) async fn violation(&self, workdir: &str) -> Option<String> {
        let head_after = capture_workdir_head(workdir).await?;
        (head_after != self.head_before).then_some(head_after)
    }
}

/// Turn a detected commit-rights violation into the node's actual result: a
/// deterministic FAIL carrying the reason, routed through the fail edge like
/// any other failure.
///
/// Deliberately reports and routes only. The engine never reverts, resets, or
/// otherwise rewrites the user's history — an automatic `git reset` on a
/// misbehaving agent risks destroying real work (the commit is frequently the
/// *correct* work, made by the wrong node), and history rewriting is not
/// something an unattended daemon should ever do on its own. Undoing is left
/// to the operator, who now has both hashes in the run output.
pub(crate) fn commit_rights_failure(
    label: &str,
    node_id: &str,
    head_before: &str,
    head_after: &str,
    node_output: Value,
) -> NodeExecution {
    let message = format!(
        "{label} committed but has no commit rights: HEAD moved {head_before} -> {head_after}. \
         Only a node configured with `commit_rights: true` may move git history. \
         The commit was left in place — undo it yourself if it does not belong there."
    );
    // Built by hand rather than with `json!` so the node's own output moves
    // in whole — it can be a full review document, and this runs on a path
    // that is already reporting a failure.
    let mut output = serde_json::Map::new();
    output.insert(
        "commit_rights_violation".to_string(),
        serde_json::json!({
            "node": label,
            "node_id": node_id,
            "head_before": head_before,
            "head_after": head_after,
            "message": message,
        }),
    );
    output.insert("node_output".to_string(), node_output);

    NodeExecution {
        output: Value::Object(output),
        summary: message,
        status: GraphRunStatus::Fail,
    }
}

/// CB39: turn a detected ancestry violation into the node's actual result.
/// The message names the spec, the recorded start head, and the current HEAD
/// — all inline, because the triage node that reads this has no repository
/// access.
pub(crate) fn ancestry_failure(
    spec_name: &str,
    spec_start_head: &str,
    current_head: &str,
    node_output: Value,
) -> NodeExecution {
    let message = format!(
        "Spec '{spec_name}': recorded start head {spec_start_head} is no longer an ancestor \
         of HEAD ({current_head}). History was rewritten past the spec's baseline \
         (amend, reset, or rebase). The run is failed as infrastructure — \
         the commit was left in place, a human must decide."
    );
    let mut output = serde_json::Map::new();
    output.insert(
        "ancestry_violation".to_string(),
        serde_json::json!({
            "spec": spec_name,
            "spec_start_head": spec_start_head,
            "current_head": current_head,
            "message": message,
        }),
    );
    output.insert("node_output".to_string(), node_output);

    NodeExecution {
        output: Value::Object(output),
        summary: message,
        status: GraphRunStatus::Fail,
    }
}

/// The ensemble id `node_id` belongs to, whether as a member or as the join
/// itself — used both to resume onto [`SpecCursor::Ensemble`] (rather than a
/// single member node) and to key the iteration budget per-ensemble instead
/// of per-member.
pub(crate) fn ensemble_owning_node(node_id: &str, ensembles: &[EnsembleDetails]) -> Option<String> {
    ensembles
        .iter()
        .find(|details| {
            details.ensemble.join_node_id == node_id
                || details
                    .members
                    .iter()
                    .any(|member| member.node_id == node_id)
        })
        .map(|details| details.ensemble.id.clone())
}

/// The internal bookkeeping row [`GraphEngine::run_graph_dispatch`]'s
/// bound-spec (`None` queue) branch inserts for an explicit-`idea` run only:
/// a graph launched with zero bound specs and a non-empty `idea` that
/// `empty_launch_check` let through. The blank `name` keeps it hidden from
/// work listings (`spec_list`/`graph_get` filter on it); the caller-supplied
/// idea text in `description` is what `{{spec_content}}` resolves from, so
/// this row always carries content and is never the blank row CB22 refuses.
/// `graph_add_spec` rejects an empty name for every real, user-authored spec,
/// so `""` can never collide with one. Never created for a no-idea launch —
/// those are refused before reaching the dispatch body.
pub(crate) fn no_spec_placeholder(graph_id: &str) -> GraphSpec {
    GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: Some(graph_id.to_string()),
        name: String::new(),
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
    }
}

/// Whether `spec` is the blank-name bookkeeping row [`no_spec_placeholder`]
/// creates for an idea-driven run (and that legacy graph-only runs also
/// wrote). The blank `name` is the sentinel — idea text may live in
/// `description` — so `run_graph_dispatch` can purge a prior attempt's
/// terminal bookkeeping row without touching a still-live resume row or any
/// real user-authored spec (`graph_add_spec` rejects an empty name).
pub(crate) fn is_no_spec_placeholder(spec: &GraphSpec) -> bool {
    spec.name.trim().is_empty()
}

#[expect(clippy::type_complexity)]
pub(crate) fn resolve_spec_start(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    spec: &GraphSpec,
    existing_runs: &[GraphNodeRun],
    ensembles: &[EnsembleDetails],
    is_resume: bool,
) -> Result<(
    SpecCursor,
    HashMap<String, Value>,
    Option<Value>,
    HashMap<String, usize>,
)> {
    // CB50 FR4: `Interrupted` resumes at its cursor only via `loop_continue`
    // (`is_resume: true`); `loop_run` (`is_resume: false`) restarts it from the
    // entry node. `Running` keeps its historic unconditional resume — a stale
    // `Running` from an abandoned attempt still resumes, which is the pre-CB50
    // behaviour for bound specs and is intentionally preserved.
    let should_resume = spec.status == GraphSpecStatus::Running
        || (spec.status == GraphSpecStatus::Interrupted && is_resume);
    if should_resume {
        if let Some(last_run) = existing_runs.last() {
            // An ensemble's N members (+ its join) each get their own
            // `graph_runs` row sharing the same `iteration` number — dedupe
            // on (budget key, iteration) so a bounce into the ensemble
            // still counts as exactly one iteration (F1), not N+1.
            let mut iterations = HashMap::<String, usize>::new();
            let mut seen = HashSet::<(String, i64)>::new();
            for run in existing_runs {
                let key = match ensemble_owning_node(&run.node_id, ensembles) {
                    Some(ensemble_id) => format!("ensemble:{ensemble_id}"),
                    None => run.node_id.clone(),
                };
                if seen.insert((key.clone(), run.iteration)) {
                    *iterations.entry(key).or_insert(0) += 1;
                }
            }
            let cursor = match ensemble_owning_node(&last_run.node_id, ensembles) {
                Some(ensemble_id) => SpecCursor::Ensemble(ensemble_id),
                None => SpecCursor::Node(last_run.node_id.clone()),
            };
            // CM1 multi-hop retention: every already-completed node's output,
            // keyed by name, so a resumed pass can still resolve
            // `{{output:NodeName}}` for any earlier node — not only the last.
            let node_outputs = retained_node_outputs(nodes, existing_runs);
            // `{{previous_feedback}}` on the resumed node must reproduce exactly
            // what it saw on its interrupted attempt: the value propagated
            // *into* it, captured as `last_run.input` when its row was inserted
            // — never that node's own (later) output row.
            return Ok((cursor, node_outputs, last_run.input.clone(), iterations));
        }
    }

    Ok((
        SpecCursor::Node(find_entry_node(nodes, edges, &spec.name)?),
        HashMap::new(),
        None,
        HashMap::new(),
    ))
}

pub(crate) fn retained_node_outputs(
    nodes: &[GraphNode],
    runs: &[GraphNodeRun],
) -> HashMap<String, Value> {
    let mut node_outputs = HashMap::new();
    for run in runs {
        if (run.status == GraphRunStatus::Pass || run.status == GraphRunStatus::Fail)
            && run.output.is_some()
        {
            if let Some(node) = nodes.iter().find(|n| n.id == run.node_id) {
                node_outputs.insert(node.name.clone(), run.output.clone().unwrap_or(Value::Null));
            }
        }
    }
    node_outputs
}
