use super::*;
use std::collections::VecDeque;

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
    /// The dirty paths the worktree reported (`git status --porcelain`),
    /// so a refusal can list what blocks the launch.
    pub paths: Vec<String>,
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

/// The commits between two HEADs, newest first, each rendered as
/// `<short hash> <subject> (<author date>)` — the listing the commit-rights
/// blocker appends so a commit that belongs to a different spec (or another
/// process entirely, e.g. a leftover session of a previous run) is visible
/// at a glance. Capped at 5 entries, then `and N more`: the blocker's job is
/// diagnosis, not dumping a long range. Empty on any git failure (non-git
/// workdir, missing ref) — the caller still reports the moved HEAD without
/// the listing, exactly like `capture_workdir_head` never errors.
pub(crate) async fn new_commits_between(
    workdir: &str,
    head_before: &str,
    head_after: &str,
) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .arg("log")
        .arg("--format=%h%x09%s%x09%ad")
        .arg("--date=short")
        .arg(format!("{head_before}..{head_after}"))
        .current_dir(workdir)
        .output()
        .await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let commits: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            // Parsed from the right: the date has no tab and the hash has
            // none either, so a tab inside a commit subject can never shift
            // the fields — the rendered line keeps the spec's
            // `<short hash> <subject> (<author date>)` shape regardless.
            let (rest, date) = line.rsplit_once('\t')?;
            let (hash, subject) = rest.split_once('\t')?;
            Some(format!("{hash} {subject} ({date})"))
        })
        .collect();
    if commits.len() > 5 {
        let remaining = commits.len() - 5;
        let mut capped: Vec<String> = commits.into_iter().take(5).collect();
        capped.push(format!("and {remaining} more"));
        capped
    } else {
        commits
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
///
/// The wording never claims the node itself committed: canopy cannot know
/// who moved HEAD — the commit may come from this node, another session, a
/// hook, or a human — so it states the observed move and lists the new
/// commits (capped at 5) for the human to judge.
pub(crate) async fn commit_rights_failure(
    label: &str,
    node_id: &str,
    head_before: &str,
    head_after: &str,
    workdir: &str,
    node_output: Value,
) -> NodeExecution {
    let mut message = format!(
        "{label}: HEAD moved from {head_before} to {head_after} while it ran. \
         The commit may come from this node or from another process (another session, a hook, a human). \
         The run has no commit rights — only a node configured with `commit_rights: true` may move git history. \
         The commit was left in place — undo it yourself if it does not belong there."
    );
    let commits = new_commits_between(workdir, head_before, head_after).await;
    if !commits.is_empty() {
        message.push_str("\nNew commits in that range:");
        for line in &commits {
            message.push_str(&format!("\n  {line}"));
        }
    }
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
        spec_code: None,
        spec_scope: None,
        spec_type: None,
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
    directed: Option<&DirectedStart>,
) -> Result<(
    SpecCursor,
    HashMap<String, Value>,
    Option<Value>,
    HashMap<String, usize>,
)> {
    // A directed run starts the spec at the operator-named node
    // regardless of status, seeding every earlier node's recorded output so
    // reachable `{{output:X}}` markers still bind. `iterations` stays empty,
    // which restarts the per-node iteration counters at 1.
    if let Some(start) = directed.filter(|d| d.spec_id == spec.id) {
        let target = resolve_directed_target(&start.node_arg, nodes, edges, ensembles)?;
        // Only nodes upstream of the resolved cursor reuse recorded outputs.
        // The cursor's own node (and everything downstream) is executed
        // fresh, so its prior output/input is never seeded.
        let upstream_ids = directed_upstream_node_ids(edges, &target.start_node_ids);
        check_upstream_prerequisites(nodes, &upstream_ids, existing_runs, ensembles)?;
        let node_outputs = upstream_node_outputs(nodes, &upstream_ids, existing_runs);
        return Ok((target.cursor, node_outputs, None, HashMap::new()));
    }
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

/// Node ids upstream of a directed cursor's start nodes: every node that can
/// reach one of the start nodes (walking edges backwards), excluding the
/// start nodes themselves and everything downstream of them.
pub(crate) fn directed_upstream_node_ids(
    edges: &[GraphEdge],
    start_node_ids: &[String],
) -> HashSet<String> {
    let start: HashSet<&str> = start_node_ids.iter().map(String::as_str).collect();
    let mut can_reach_start: HashSet<String> = HashSet::new();
    let mut back: VecDeque<String> = start_node_ids.iter().cloned().collect();
    while let Some(id) = back.pop_front() {
        if !can_reach_start.insert(id.clone()) {
            continue;
        }
        for edge in edges.iter().filter(|e| e.to_node == id) {
            if !can_reach_start.contains(&edge.from_node) {
                back.push_back(edge.from_node.clone());
            }
        }
    }
    can_reach_start
        .into_iter()
        .filter(|id| !start.contains(id.as_str()))
        .collect()
}

/// FR2: the run a directed start should judge `node` by. For a member of an
/// ensemble (never the join node itself), that is the ensemble's quorum result
/// — the latest run of `join_node_id` — because a round-robin ensemble passes
/// through whichever member carried the vote: one member's failure under a
/// passed quorum is not an unmet prerequisite. When the ensemble has not
/// reported a quorum yet (no join run), fall back to the member's own latest
/// run, which is then the only evidence there is.
///
/// A quorum only judges the pass it settled, though. A member whose latest run
/// is *newer* than the last join ran again after that verdict — the daemon died
/// between the member and its join — so that attempt has no quorum and is
/// judged by its own run: accepting it would seed a failed member's output,
/// exactly what this check exists to prevent.
fn prerequisite_run<'a>(
    node: &'a GraphNode,
    nodes: &'a [GraphNode],
    runs: &'a [GraphNodeRun],
    ensembles: &'a [EnsembleDetails],
) -> Option<(&'a GraphNode, &'a GraphNodeRun)> {
    let own = runs.iter().rev().find(|run| run.node_id == node.id);
    let quorum = ensembles
        .iter()
        .filter(|details| details.ensemble.join_node_id != node.id)
        .find(|details| {
            details
                .members
                .iter()
                .any(|member| member.node_id == node.id)
        })
        .and_then(|details| {
            runs.iter()
                .rev()
                .find(|run| run.node_id == details.ensemble.join_node_id)
        })
        // One pass of an ensemble runs its members and its join under the same
        // `iteration` (a bounce re-enters it within that iteration), so a join
        // at the member's iteration or later has seen that member's attempt.
        .filter(|join| own.is_none_or(|run| join.iteration >= run.iteration));
    if let Some(run) = quorum {
        let join = nodes.iter().find(|candidate| candidate.id == run.node_id);
        return Some((join.unwrap_or(node), run));
    }
    own.map(|run| (node, run))
}

/// Refuses a directed start when an upstream prerequisite (one whose output
/// would be reused) last ran Fail, Error or Interrupted. Skipping it as if it
/// had passed would route on work that never cleared its gates.
pub(crate) fn check_upstream_prerequisites(
    nodes: &[GraphNode],
    upstream_ids: &HashSet<String>,
    runs: &[GraphNodeRun],
    ensembles: &[EnsembleDetails],
) -> anyhow::Result<()> {
    for node in nodes.iter().filter(|n| upstream_ids.contains(&n.id)) {
        let Some((judged, latest)) = prerequisite_run(node, nodes, runs, ensembles) else {
            continue;
        };
        if matches!(
            latest.status,
            GraphRunStatus::Fail | GraphRunStatus::Error | GraphRunStatus::Interrupted
        ) {
            bail!(
                "Directed run refused: prerequisite node '{}' last run was {} ({}). Re-run it first or start at an earlier node.",
                judged.name,
                latest.status.as_str(),
                latest.id
            );
        }
    }
    Ok(())
}

/// Recorded outputs a directed start may reuse: only nodes upstream of its
/// cursor. The cursor's own output (and anything downstream) is never seeded.
pub(crate) fn upstream_node_outputs(
    nodes: &[GraphNode],
    upstream_ids: &HashSet<String>,
    runs: &[GraphNodeRun],
) -> HashMap<String, Value> {
    let upstream_runs: Vec<GraphNodeRun> = runs
        .iter()
        .filter(|run| upstream_ids.contains(&run.node_id))
        .cloned()
        .collect();
    retained_node_outputs(nodes, &upstream_runs)
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
