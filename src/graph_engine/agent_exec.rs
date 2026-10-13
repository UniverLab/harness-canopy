use super::*;

/// Execute an agent node, RESUMING its captured session (RS2) when the engine
/// hands down a `resume_session_id` for a re-run of this node (a fail-edge
/// bounce, or a B19 infra retry of an attempt that had created a session) and
/// the node opts in and the platform supports headless resume-by-id.
///
/// A resumed spawn gets only the incremental prompt (new feedback + the
/// report contract), continues the same session (recorded on the new run row,
/// capture skipped), and — if the resume flag is rejected / crashes at spawn —
/// falls back to a byte-identical cold start whose verdict the node then uses.
/// Every other case cold-starts exactly as before.
///
/// `resume_crosses_spec` (RS3) marks the one exception to "only feedback, no
/// spec": when the session being resumed was captured by a DIFFERENT spec (a
/// context-group handoff), the resumed session has never seen this spec's
/// content, so the incremental prompt renders it — plus a boundary notice
/// that the previous spec is done — instead of the bare feedback-only
/// template. See [`render_resume_prompt`].
#[expect(clippy::too_many_arguments)]
pub(crate) async fn execute_agent_node(
    db: &Arc<Database>,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    previous_output: Option<&Value>,
    run_id: &str,
    workdir: &str,
    resume_session_id: Option<&str>,
    resume_crosses_spec: bool,
    dynamic_skills: Option<&Arc<crate::dynamic_skills::SkillStore>>,
    node_outputs: &HashMap<String, Value>,
    all_node_names: &[String],
) -> Result<NodeExecution> {
    let cli_name = node
        .config
        .get("platform")
        .or_else(|| node.config.get("cli"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Agent node '{}' is missing a platform/cli.", node.name))?;
    let cli = Cli::resolve(Some(cli_name)).map_err(anyhow::Error::msg)?;
    let model = node.config.get("model").and_then(Value::as_str);
    let effort = node.config.get("effort").and_then(Value::as_str);
    let timeout_minutes = node
        .config
        .get("timeout_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(30);
    let base_strategy = cli.strategy();

    // Per-node opt-out: `resume: false` forces cold starts. Default is to
    // resume whenever the engine offers a session and the platform supports it.
    let node_allows_resume = node.config.get("resume").and_then(Value::as_bool) != Some(false);

    // ── RS2 resume attempt ──────────────────────────────────────────────
    if let Some(sid) = resume_session_id {
        if node_allows_resume && base_strategy.supports_resume_by_id() {
            // RS3: a cross-spec resume always uses the engine's own boundary
            // template, ignoring any node-level `resume_prompt` override —
            // the choice depends on runtime state (which spec the resumed
            // session was captured under) that a static per-node template
            // cannot know, so it is not the node's to make.
            let resume_template = if resume_crosses_spec {
                RESUME_PROMPT_CROSS_SPEC_DEFAULT
            } else {
                node.config
                    .get("resume_prompt")
                    .and_then(Value::as_str)
                    .unwrap_or(RESUME_PROMPT_DEFAULT)
            };
            let ctx = AgentRunCtx {
                db,
                lp,
                spec,
                node,
                previous_output,
                run_id,
                workdir,
                dynamic_skills,
                node_outputs,
                all_node_names,
                cli: &cli,
                base_strategy: &base_strategy,
                model,
                effort,
                timeout_minutes,
            };
            if let Some(out) = try_resume_agent_node(&ctx, sid, resume_template).await? {
                return Ok(out);
            }
            // else: fall through to the cold path below
        }
    }

    // ── Cold start (byte-identical to the pre-RS2 path) ─────────────────
    let prompt_template = resolve_node_prompt_template(
        node,
        &crate::domain::prompts::prompts_dir(&crate::domain::prompts::canopy_dir()),
    );
    // CM47: the spec's working-memory notes travel with every cold start,
    // before `[REPORTING]`.
    let spec_notes_section = build_spec_notes_section(db, lp, spec, node);
    let prompt = match render_agent_prompt(
        lp,
        spec,
        node,
        &prompt_template,
        previous_output,
        workdir,
        run_id,
        node_outputs,
        all_node_names,
        &spec_notes_section,
    ) {
        Ok(p) => p,
        Err(e) => {
            return Ok(NodeExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "failure_kind": "invalid_template",
                    "error": e.to_string(),
                }),
                summary: format!("Agent node '{}' prompt template is invalid: {e}", node.name),
            });
        }
    };
    let prompt = append_pinned_skills(prompt, node, dynamic_skills).await;
    let strategy = sized_strategy(&base_strategy, &prompt);
    let execution = run_agent_process(
        db,
        run_id,
        &cli,
        &strategy,
        node,
        &prompt,
        model,
        effort,
        workdir,
        timeout_minutes,
        None,
    )
    .await?;

    if let Some(reported) = self_reported_execution(db.get_graph_run(run_id)?.as_ref(), node) {
        return Ok(reported);
    }
    Ok(execution)
}

/// CM47: build the `[SPEC NOTES]` section for a cold-start prompt. Fetches
/// the spec's notes, resolves the caller's owner (ensemble when the node is
/// a member, else the node — the same `daemon::handler_notes` ownership the
/// write path enforces) and the Designer, then composes the section. A
/// notes-fetch failure degrades to notes-free (instructions-only) rather
/// than failing the node — notes are working memory, not a gate.
pub(crate) fn build_spec_notes_section(
    db: &Arc<Database>,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
) -> String {
    let notes = db.spec_notes_get(&spec.id).unwrap_or_default();
    let views: Vec<SpecNoteView<'_>> = notes
        .iter()
        .map(|note| SpecNoteView {
            owner_kind: note.owner_kind.as_str(),
            owner_id: note.owner_id.as_str(),
            owner_name: note.owner_name.as_str(),
            body: note.body.as_str(),
            updated_at: note.updated_at.as_str(),
        })
        .collect();
    let caller = crate::daemon::handler_notes::resolve_owner(db, &node.id)
        .ok()
        .flatten()
        .map(|owner| (owner.kind, owner.id));
    let designer = designer_owner_for_prompt(db, lp, spec);
    let caller_ref = caller.as_ref().map(|(kind, id)| (*kind, id.as_str()));
    let designer_ref = designer
        .as_ref()
        .map(|(kind, id)| (kind.as_str(), id.as_str()));
    compose_spec_notes_section(&views, caller_ref, designer_ref)
}

/// CM47: the spec's own graph when it has one, else the top-level graph —
/// the same precedence `GraphEngine::load_spec_graph` uses. Returns the
/// nodes/edges the Designer's DFS walks, plus member→ensemble ids.
type SpecGraphScope = (Vec<GraphNode>, Vec<GraphEdge>, HashMap<String, String>);

fn spec_graph_scope(
    db: &Arc<Database>,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
) -> Option<SpecGraphScope> {
    let spec_nodes = db.list_graph_nodes(&spec.id).ok()?;
    let use_top_level = spec_nodes.is_empty();
    let nodes = if use_top_level {
        db.list_graph_nodes_for_graph(&lp.id).ok()?
    } else {
        spec_nodes
    };
    let edges = if use_top_level {
        db.list_graph_edges_for_graph(&lp.id).ok()?
    } else {
        db.list_graph_edges(&spec.id).ok()?
    };
    let ensembles = if use_top_level {
        db.list_ensembles_for_graph(&lp.id).ok()?
    } else {
        db.list_ensembles_for_spec(&spec.id).ok()?
    };
    let mut member_to_ensemble = HashMap::new();
    for details in &ensembles {
        for member in &details.members {
            member_to_ensemble.insert(member.node_id.clone(), details.ensemble.id.clone());
        }
    }
    Some((nodes, edges, member_to_ensemble))
}

fn designer_owner_for_prompt(
    db: &Arc<Database>,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
) -> Option<(String, String)> {
    let (nodes, edges, member_to_ensemble) = spec_graph_scope(db, lp, spec)?;
    resolve_designer_owner(&nodes, &edges, &member_to_ensemble)
}

/// Invariant inputs of an agent node's RS2 resume attempt, gathered once by
/// [`execute_agent_node`] and consumed by [`try_resume_agent_node`].
pub(crate) struct AgentRunCtx<'a> {
    pub(crate) db: &'a Arc<Database>,
    pub(crate) lp: &'a crate::domain::graphs::Graph,
    pub(crate) spec: &'a GraphSpec,
    pub(crate) node: &'a GraphNode,
    pub(crate) previous_output: Option<&'a Value>,
    pub(crate) run_id: &'a str,
    pub(crate) workdir: &'a str,
    pub(crate) dynamic_skills: Option<&'a Arc<crate::dynamic_skills::SkillStore>>,
    pub(crate) node_outputs: &'a HashMap<String, Value>,
    pub(crate) all_node_names: &'a [String],
    pub(crate) cli: &'a Cli,
    pub(crate) base_strategy: &'a crate::domain::cli_strategy::CliStrategy,
    pub(crate) model: Option<&'a str>,
    pub(crate) effort: Option<&'a str>,
    pub(crate) timeout_minutes: u64,
}

/// One RS2 resume attempt. Returns `None` when the attempt failed at spawn and
/// the caller must fall back to a cold start; `Some(execution)` otherwise.
pub(crate) async fn try_resume_agent_node(
    ctx: &AgentRunCtx<'_>,
    sid: &str,
    resume_template: &str,
) -> Result<Option<NodeExecution>> {
    let resume_prompt = match render_resume_prompt(
        ctx.lp,
        ctx.spec,
        ctx.node,
        resume_template,
        ctx.previous_output,
        ctx.workdir,
        ctx.run_id,
        ctx.node_outputs,
        ctx.all_node_names,
    ) {
        Ok(p) => p,
        Err(e) => {
            return Ok(Some(NodeExecution {
                status: GraphRunStatus::Fail,
                output: serde_json::json!({
                    "failure_kind": "invalid_template",
                    "error": e.to_string(),
                }),
                summary: format!(
                    "Agent node '{}' prompt template is invalid: {e}",
                    ctx.node.name
                ),
            }));
        }
    };
    let resume_prompt = append_pinned_skills(resume_prompt, ctx.node, ctx.dynamic_skills).await;
    let strategy = sized_strategy(ctx.base_strategy, &resume_prompt);
    let execution = run_agent_process(
        ctx.db,
        ctx.run_id,
        ctx.cli,
        &strategy,
        ctx.node,
        &resume_prompt,
        ctx.model,
        ctx.effort,
        ctx.workdir,
        ctx.timeout_minutes,
        Some(sid),
    )
    .await?;

    let run = ctx.db.get_graph_run(ctx.run_id)?;
    // The resumed agent self-reported → route its verdict normally.
    if let Some(reported) = self_reported_execution(run.as_ref(), ctx.node) {
        return Ok(Some(reported));
    }
    // Resume flag rejected / crashed at spawn (quick, non-self-reported
    // failure)? Fall back to a cold start whose result the node uses.
    // A resumed run that did real work and then failed (slow, or a
    // timeout) is a genuine fail and routes normally — never redone.
    // A `require_report` downgrade (`failure_kind: "no_report"`) is
    // excluded the same way `is_infra_crash` excludes it: the process
    // ran to completion and exited 0, so it never "crashed at spawn"
    // — it must route down the fail edge, not get silently redone.
    let (_, crash_max_secs, _) = read_infra_config(ctx.node);
    let elapsed = run
        .as_ref()
        .map(|r| (chrono::Utc::now() - r.started_at).num_seconds())
        .unwrap_or(i64::MAX);
    let no_report =
        execution.output.get("failure_kind").and_then(Value::as_str) == Some("no_report");
    // CM13: an exit-0 run that never reported ran to completion — it is
    // unreported infra, not a spawn rejection, so it must NOT be
    // silently redone cold (that would reproduce the identical silent
    // result and clobber the resumed session id). Only a non-zero
    // exit (or a spawn failure with no exit code at all) reads as
    // "rejected at spawn" and falls back.
    let exited_zero = execution.output.get("exit_code").and_then(Value::as_i64) == Some(0);
    let resume_failed_at_spawn = execution.status == GraphRunStatus::Fail
        && elapsed < crash_max_secs as i64
        && !no_report
        && !exited_zero;
    if !resume_failed_at_spawn {
        return Ok(Some(execution));
    }
    tracing::warn!(
        run_id = ctx.run_id,
        node = %ctx.node.name,
        "resume failed at spawn; falling back to a cold start"
    );
    Ok(None)
}
/// Shape-matches a harness's stderr against an "untrusted working directory"
/// refusal — the class of failure that produced exit 0, empty stdout, and
/// this stderr in the 2026-08-13 `gitkit-composition` incident:
///
/// ```text
/// Warning: /home/.../gitkit is not trusted; project configuration (.agents/)
///          will be ignored. Re-run with --trust to trust this folder temporarily.
/// ```
///
/// `.agents/` is where MCP server configuration lives, so a harness hitting
/// this silently loses every tool it needed — including the two calls
/// (`graph_complete_node`/`graph_report_blocker`) it would need to report that
/// loss. Matches on the *shape* of the refusal (an explicit "not trusted"
/// verdict alongside project configuration being ignored) rather than this
/// one CLI's exact sentence, since a different harness or a future wording
/// change must still be caught — see [`agent_finished_execution`], which
/// keeps the stderr verbatim in the report so a wording drift is visible
/// there rather than silently swallowed by ever-looser matching here.
pub(crate) fn is_untrusted_workdir_signal(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("not trusted") && lower.contains("ignored")
}

/// Turn a completed (non-timeout, non-spawn-failure) CLI run into its
/// verdict. `exit_code == 0` is necessary but not sufficient for `Pass`: a
/// CLI that fails to start its model, prints to stderr, and still exits 0
/// produces empty stdout — no self-report, no route answer, nothing a
/// downstream node can read as a result. Crediting that with `Pass` is
/// exactly the defect from the 2026-08 `mimocode`/`mimo-auto` incident,
/// where four such runs were routed down the `pass` edge and the resilience
/// node whose entire job was to catch this never ran once.
///
/// So: empty stdout is `Fail` regardless of `exit_code`, marked `no_output`
/// so [`is_infra_crash`] can tell it apart from a fast nonzero-exit crash
/// (see that function's doc comment for why the two must not be treated the
/// same), and — when stderr has text — carried into `error` so
/// [`member_output_text`]'s existing `error` fallback surfaces it in an
/// ensemble's consolidated doc, and a downstream node reading
/// `{{previous_feedback}}` (the whole JSON blob, not just `stdout`) sees it
/// too instead of a silent `(none)`.
///
/// An agent that exits 0 with real stdout keeps passing exactly as before —
/// this only changes the empty-stdout case, which used to be an
/// unconditional `Pass`.
///
/// `self_reported` — whether this run's row already carries a
/// `graph_complete_node`/`graph_report_blocker` verdict — decides two more
/// things unrelated to `zero_exit_no_output`:
///
/// - `unreported: true` is stamped on the output whenever it's `false`,
///   whatever the verdict ends up being. This is unconditional (not gated on
///   `require_report`) so a resilience node downstream can always tell "the
///   harness ran and chose not to report" apart from "the harness never ran",
///   without any config of its own.
/// - When the node opts in with `require_report: true` in its config, an
///   otherwise-passing run (exit 0, real stdout) that never self-reported is
///   downgraded to `Fail` with `failure_kind: "no_report"`. This is the hole
///   `zero_exit_no_output` doesn't cover: codex, copilot and antigravity have
///   all been observed exiting 0 with non-empty stdout — including the
///   model's own success sentinel — while every tool call they attempted was
///   refused or unavailable and nothing was actually done. Self-reporting
///   always wins regardless of this flag: this function only ever runs for
///   the *unreported* branch (see the `self_reported_execution` check at
///   this function's call sites), so there is no case here where an explicit
///   `graph_complete_node` verdict could be overridden.
///
/// A fourth, more specific `failure_kind` — `"untrusted_workdir"` — joins
/// `"no_report"` in this vocabulary when [`is_untrusted_workdir_signal`]
/// matches stderr on a `zero_exit_no_output` run (C3).
pub(crate) fn agent_finished_execution(
    node: &GraphNode,
    cli: &Cli,
    model: Option<&str>,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    self_reported: bool,
) -> NodeExecution {
    // Only the exit-0 + empty-stdout combination is the new failure shape
    // (a process that ran to completion and said nothing). A nonzero exit
    // with empty stdout is the ordinary fast-crash signature `is_infra_crash`
    // already retries — most crashing CLIs print nothing before dying — so it
    // must NOT pick up the `no_output` marker or this fix would silently
    // stop retrying every plain crash that happens not to log to stdout.
    let zero_exit_no_output = exit_code == 0 && stdout.is_empty();
    let exit_says_pass = exit_code == 0 && !zero_exit_no_output;
    // Only meaningful within the `zero_exit_no_output` shape: a harness that
    // produced real output warned about many things, and an untrusted-workdir
    // mention alongside a successful result must never downgrade it (harnesses
    // warn about all kinds of things and still finish the job).
    let untrusted_workdir = zero_exit_no_output && is_untrusted_workdir_signal(stderr);

    let require_report = node
        .config
        .get("require_report")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let no_report_override = require_report && !self_reported && exit_says_pass;

    // CM13: An unreported AGENT run is never a pass, regardless of exit code.
    // Silence is not success; inferring it is how unreviewed work reaches a commit.
    // Scoped to Agent nodes: a Router node's verdict IS its stdout (it never
    // self-reports by design), so its pass/fail still comes from the
    // exit/output shape below.
    let verdict_must_be_reported = node.kind == GraphNodeKind::Agent;
    let status =
        if exit_says_pass && !no_report_override && (!verdict_must_be_reported || self_reported) {
            GraphRunStatus::Pass
        } else {
            GraphRunStatus::Fail
        };
    let mut output = serde_json::json!({
        "kind": "agent",
        "node_id": node.id,
        "cli": cli.as_str(),
        "model": model,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
        "prompt_source": agent_prompt_source(&node.config),
    });
    if zero_exit_no_output {
        let reason = if untrusted_workdir {
            format!(
                "agent produced no output because its working directory was not trusted \
                 by the harness, so project configuration (including MCP tools) was \
                 ignored; stderr: {stderr}"
            )
        } else if stderr.is_empty() {
            "agent produced no output".to_string()
        } else {
            format!("agent produced no output; stderr: {stderr}")
        };
        if let Value::Object(map) = &mut output {
            map.insert("no_output".to_string(), Value::Bool(true));
            map.insert("error".to_string(), Value::String(reason));
            if untrusted_workdir {
                map.insert(
                    "failure_kind".to_string(),
                    Value::String("untrusted_workdir".to_string()),
                );
            }
        }
    }
    if !self_reported {
        if let Value::Object(map) = &mut output {
            map.insert("unreported".to_string(), Value::Bool(true));
        }
    }
    // CM13: stamp `failure_kind: "unreported"` so history readers can
    // distinguish "never reported" from "failed at work" and from "process crashed".
    // Agent-only, matching the verdict gate above: routers keep their own output shape.
    // Never clobbers a more specific kind already set above (e.g. "untrusted_workdir").
    if !self_reported && !no_report_override && node.kind == GraphNodeKind::Agent {
        if let Value::Object(map) = &mut output {
            if !map.contains_key("failure_kind") {
                map.insert(
                    "failure_kind".to_string(),
                    Value::String("unreported".to_string()),
                );
            }
        }
    }
    if no_report_override {
        if let Value::Object(map) = &mut output {
            map.insert(
                "failure_kind".to_string(),
                Value::String("no_report".to_string()),
            );
        }
    }
    NodeExecution {
        status,
        summary: if no_report_override {
            format!(
                "Agent node '{}' exited 0 but never called graph_complete_node (require_report).",
                node.name
            )
        } else if untrusted_workdir {
            format!(
                "Agent node '{}' produced no output: the harness reported its working \
                 directory as untrusted and ignored project configuration (exit code 0).",
                node.name
            )
        } else if zero_exit_no_output {
            format!(
                "Agent node '{}' produced no output (exit code 0).",
                node.name
            )
        } else {
            format!("Agent node '{}' exited with code {}.", node.name, exit_code)
        },
        output,
    }
}

pub(crate) fn agent_spawn_failure(
    node: &GraphNode,
    cli: &Cli,
    model: Option<&str>,
    error: &SpawnError,
) -> NodeExecution {
    let mut output = serde_json::json!({
        "kind": "agent",
        "node_id": node.id,
        "cli": cli.as_str(),
        "model": model,
        "error": error.message,
        "prompt_source": agent_prompt_source(&node.config),
    });
    // A permanent failure is recorded with its reason so the run reads as
    // "failed fast on purpose" rather than "retried and gave up" — the two
    // are otherwise indistinguishable in a persisted run row.
    if let (Some(reason), serde_json::Value::Object(map)) = (error.permanent_reason, &mut output) {
        map.insert("spawn_permanent".to_string(), Value::Bool(true));
        map.insert(
            "infra_retry_skipped".to_string(),
            Value::String(reason.to_string()),
        );
    }
    NodeExecution {
        status: GraphRunStatus::Fail,
        output,
        summary: format!(
            "Agent node '{}' failed to spawn: {}",
            node.name, error.message
        ),
    }
}

#[cfg(test)]
mod spec_notes_prompt_tests {
    use super::*;
    use crate::daemon::handler_notes::spec_note_write_from_run;
    use crate::domain::graphs::{
        Ensemble, EnsembleKind, EnsembleMember, Graph, GraphEdgeCondition, GraphNode,
        GraphNodeKind, GraphNodeRun, GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
    };

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn seed_graph(db: &Database) {
        db.insert_graph(&Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "g".to_string(),
            name: "Graph".to_string(),
            description: None,
            workdir: "/tmp/test".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        })
        .unwrap();
    }

    fn spec(id: &str) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
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

    fn agent_node_at(id: &str, spec_id: &str, name: &str, position: i64) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: Some(spec_id.to_string()),
            graph_id: None,
            name: name.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position,
            created_at: chrono::Utc::now(),
        }
    }

    fn running_run(id: &str, spec_id: &str, node_id: &str) -> GraphNodeRun {
        GraphNodeRun {
            id: id.to_string(),
            graph_id: "g".to_string(),
            spec_id: spec_id.to_string(),
            node_id: node_id.to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 1,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: Some("claude".to_string()),
            executed_model: Some("opus".to_string()),
        }
    }

    /// T1: a member of a round_robin ensemble writes a note, its run is cut
    /// (timeout), and the next member's prompt contains that note under the
    /// ensemble's name (req 9.1).
    #[test]
    fn replacing_member_prompt_carrys_ensemble_note() {
        let db = std::sync::Arc::new(test_db());
        seed_graph(&db);
        db.insert_graph_spec(&spec("spec-1")).unwrap();
        db.insert_graph_node(&agent_node_at("entry", "spec-1", "Entry", 0))
            .unwrap();
        let member = |node_id: &str, position: i64| EnsembleMember {
            ensemble_id: "ens-1".to_string(),
            node_id: node_id.to_string(),
            position,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        };
        db.insert_ensemble_unit(
            &Ensemble {
                id: "ens-1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "Implementer".to_string(),
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
                kind: EnsembleKind::RoundRobin,
                round_robin_index: Some(0),
                commit_rights: false,
                created_at: chrono::Utc::now(),
            },
            &[member("m1", 0), member("m2", 1)],
            &[
                agent_node_at("m1", "spec-1", "Member 1", 1),
                agent_node_at("m2", "spec-1", "Member 2", 2),
            ],
            &{
                let mut join = agent_node_at("join", "spec-1", "Join", 3);
                join.kind = GraphNodeKind::Join;
                join
            },
            &[],
        )
        .unwrap();

        // Member 1 starts and writes its note.
        db.insert_graph_run(&running_run("run-m1", "spec-1", "m1"))
            .unwrap();
        spec_note_write_from_run(&db, "run-m1", "halfway: schema done").unwrap();

        // Cut member 1: Fail is the honest "not running" terminal (there is
        // no Timeout GraphRunStatus).
        db.update_graph_run_result(
            "run-m1",
            GraphRunStatus::Fail,
            None,
            Some(chrono::Utc::now()),
        )
        .unwrap();

        // Member 2 starts; its cold-start prompt must carry the ensemble's note.
        db.insert_graph_run(&running_run("run-m2", "spec-1", "m2"))
            .unwrap();
        let lp = db.get_graph("g").unwrap().unwrap();
        let spec = db.get_graph_spec("spec-1").unwrap().unwrap();
        let node = db.get_graph_node("m2").unwrap().unwrap();

        let section = build_spec_notes_section(&db, &lp, &spec, &node);
        assert!(
            section.contains("## Implementer"),
            "section must carry the ensemble's note heading, got: {section}"
        );
        assert!(
            section.contains("halfway: schema done"),
            "section must carry the ensemble's note body, got: {section}"
        );
    }
}
