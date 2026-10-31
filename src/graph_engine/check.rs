use super::*;
use crate::db::connection::with_busy_retry;
use crate::db::Database;
use crate::domain::graphs::GraphRunStatus;
use check_command::{execute_shell_command, ShellCommandResult};
use check_render::{
    check_command_env_vars, render_check_command, write_check_notes_file, write_check_spec_file,
    CheckCommandCtx,
};
#[cfg(test)]
use check_render::{check_command_marker_error, CHECK_COMMAND_RAW_MARKERS};

#[expect(clippy::too_many_arguments)]
pub(crate) async fn execute_check_node(
    db: &Database,
    run_id: &str,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    spec_start_head: Option<&str>,
    spec_committed_head: Option<&str>,
    workdir: &str,
) -> Result<NodeExecution> {
    let raw_command = node
        .config
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Check node '{}' is missing a command.", node.name))?;
    // SPECCODE: type/scope/code/commit message come straight off the spec
    // row; an untyped spec yields all-empty (the legacy convention).
    let spec_commit_message = spec
        .spec_type
        .map(|spec_type| commit_message(spec_type, spec.spec_scope.as_deref(), &spec.name));
    let ctx = CheckCommandCtx {
        graph_id: &lp.id,
        graph_name: &lp.name,
        workdir,
        spec_id: &spec.id,
        spec_name: &spec.name,
        spec_type: spec.spec_type,
        spec_scope: spec.spec_scope.as_deref(),
        spec_code: spec.spec_code.as_deref(),
        spec_commit_message: spec_commit_message.as_deref(),
        spec_start_head,
        spec_start_dirty: spec.spec_start_dirty,
        spec_committed_head,
    };
    // CM38 FR3: a command carrying a marker the engine cannot bind must
    // fail the node before anything reaches the shell.
    let command = match render_check_command(raw_command, &ctx) {
        Ok(command) => command,
        Err(error) => {
            return Ok(check_marker_violation(
                db,
                run_id,
                lp,
                spec,
                node,
                raw_command,
                &error.to_string(),
            ));
        }
    };

    let success_condition = node
        .config
        .get("success_condition")
        .and_then(Value::as_str)
        .unwrap_or("exit_code_0");
    let timeout_seconds = node
        .config
        .get("timeout_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(120);

    // CM38 FR6: the spec text rides in a scratch file — never a marker —
    // so there is no quoting problem and no argv size limit. The TempDir is
    // held until this function returns, which removes the file once the
    // node has finished (pass, fail and timeout paths alike).
    let (_spec_scratch, spec_file) = write_check_spec_file(node, spec)?;
    // CM47: notes ride in a sibling scratch file — never an env var's text —
    // so several 2500-char notes can never hit an env size limit.
    let (_notes_scratch, notes_file) = write_check_notes_file(db, node, spec)?;
    let env_vars = check_command_env_vars(&ctx, &spec_file, &notes_file);
    let result =
        execute_shell_command(db, run_id, &command, workdir, timeout_seconds, &env_vars).await?;

    // B28: a timeout is a check fail, not a hard error and not something to
    // run the success condition against — it must route through the fail
    // edge like any other check failure, never abort the whole spec and
    // never turn into a pass because a non-exit-code condition happened to
    // match the partial output.
    if result.timed_out {
        return Ok(check_timeout_execution(
            lp,
            spec,
            node,
            timeout_seconds,
            result.output,
        ));
    }

    finalize_check_result(lp, spec, node, success_condition, result)
}

/// CM38 FR3: settle a check node whose command carried an unbindable
/// `{{...}}` marker — a Fail verdict routed through the node's `fail` edge,
/// with the refusal message recorded on the run row. Nothing ever spawns.
fn check_marker_violation(
    db: &Database,
    run_id: &str,
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    raw_command: &str,
    message: &str,
) -> NodeExecution {
    let output_json = serde_json::json!({
        "kind": "check",
        "command": raw_command,
        "error": message,
        "graph_id": lp.id,
        "spec_id": spec.id,
        "node_id": node.id,
    });
    let _ = with_busy_retry("update_graph_run_result", || {
        db.update_graph_run_result(
            run_id,
            GraphRunStatus::Fail,
            Some(&output_json),
            Some(chrono::Utc::now()),
        )
    });
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: output_json,
        summary: format!("Check node '{}': {message}.", node.name),
    }
}

/// The timed-out shape of a check node (B28): Fail verdict plus the graph/
/// spec/node metadata, with no success condition evaluated.
fn check_timeout_execution(
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    timeout_seconds: u64,
    output: Value,
) -> NodeExecution {
    let mut output_json = output;
    output_json["graph_id"] = serde_json::json!(lp.id);
    output_json["spec_id"] = serde_json::json!(spec.id);
    output_json["node_id"] = serde_json::json!(node.id);
    NodeExecution {
        status: GraphRunStatus::Fail,
        output: output_json,
        summary: format!(
            "Check node '{}' timed out after {timeout_seconds}s.",
            node.name
        ),
    }
}

/// Evaluate a finished check command's `success_condition` against the
/// *full* captured output — the `output_contains` / `output_not_contains`
/// conditions must see everything the command emitted, not just the tail
/// kept for storage — and build the check node's result with the graph/
/// spec/node metadata.
fn finalize_check_result(
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    success_condition: &str,
    result: ShellCommandResult,
) -> Result<NodeExecution> {
    let exit_code = result
        .output
        .get("exit_code")
        .and_then(Value::as_i64)
        .unwrap_or(-1) as i32;
    let combined = if result.full_stderr.is_empty() {
        result.full_stdout
    } else if result.full_stdout.is_empty() {
        result.full_stderr
    } else {
        format!("{}\n{}", result.full_stdout, result.full_stderr)
    };
    let passed = evaluate_success_condition(success_condition, exit_code, &combined)?;

    let mut output_json = result.output;
    output_json["graph_id"] = serde_json::json!(lp.id);
    output_json["spec_id"] = serde_json::json!(spec.id);
    output_json["node_id"] = serde_json::json!(node.id);
    output_json["success_condition"] = serde_json::json!(success_condition);
    output_json["passed"] = serde_json::json!(passed);

    let status = if passed {
        GraphRunStatus::Pass
    } else {
        GraphRunStatus::Fail
    };

    Ok(NodeExecution {
        status,
        output: output_json,
        summary: format!(
            "Check node '{}' {}.",
            node.name,
            if passed { "passed" } else { "failed" }
        ),
    })
}

/// Force stdin prompt delivery for an oversized prompt. Node outputs are
/// arbitrarily large (e.g. a full `cargo test` log), and the composed prompt
/// embeds previous_output via `{{previous_feedback}}`. Even after elision the
/// total can exceed Linux's MAX_ARG_STRLEN (128KiB), crashing the spawn with
/// E2BIG — the temp-file + stdin transport has no size cliff.
pub(crate) fn sized_strategy(
    base: &crate::domain::cli_strategy::CliStrategy,
    prompt: &str,
) -> crate::domain::cli_strategy::CliStrategy {
    if prompt.len() > ARGV_SAFETY_THRESHOLD && !base.prompt_via_stdin {
        base.with_stdin_forced()
    } else {
        base.clone()
    }
}

/// A node's pinned `skills` (S2): an optional array of skill names in
/// `node.config["skills"]`, resolved through the dynamic skill store (S1) at
/// spawn time and appended to the composed prompt in listed order. Anything
/// other than an array of strings (key absent, wrong type, non-string
/// element) is treated as "no pins" — malformed config must never fail a
/// spawn, exactly like every other loosely-typed node config key.
pub(crate) fn node_pinned_skills(node: &GraphNode) -> Vec<String> {
    node.config
        .get("skills")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Stable delimiter a pinned skill's instructions are appended under (S2).
/// Keep this format stable — downstream prompts may reference it.
pub(crate) fn render_pinned_skill_section(name: &str, instructions: &str) -> String {
    format!("\n\n## Skill: {name}\n{instructions}")
}

/// One-line stand-in for a pinned skill that couldn't be resolved, under the
/// same `## Skill: <name>` delimiter so the section is still easy to spot.
pub(crate) fn render_unresolved_skill_note(name: &str) -> String {
    format!(
        "\n\n## Skill: {name}\n_Could not resolve pinned skill '{name}' — continuing without it._"
    )
}

/// Resolve every skill pinned on `node` (S2) through the dynamic skill store
/// and append each one's instructions to `prompt`, in listed order, under a
/// `## Skill: <name>` section (see [`render_pinned_skill_section`]). Applied
/// identically to cold-start and resumed prompts so every agent-spawn
/// flavor carries the same pins.
///
/// A skill that can't be resolved — unknown name, no store configured, the
/// store's blocking fetch task panicking, or (via [`crate::dynamic_skills`]'s
/// own TTL/network handling) a source that's unreachable — degrades to
/// [`render_unresolved_skill_note`] plus a WARN naming the node and skill.
/// Pinned skills are a convenience the agent gets automatically, never a
/// hard dependency for the spawn to proceed at all.
pub(crate) async fn append_pinned_skills(
    mut prompt: String,
    node: &GraphNode,
    dynamic_skills: Option<&Arc<crate::dynamic_skills::SkillStore>>,
) -> String {
    let names = node_pinned_skills(node);
    if names.is_empty() {
        return prompt;
    }

    for name in names {
        let resolved = resolve_pinned_skill(node, &name, dynamic_skills).await;
        append_skill_result(&mut prompt, &name, resolved);
    }

    prompt
}

pub(crate) async fn resolve_pinned_skill(
    node: &GraphNode,
    name: &str,
    dynamic_skills: Option<&Arc<crate::dynamic_skills::SkillStore>>,
) -> Option<String> {
    let Some(store) = dynamic_skills else {
        tracing::warn!(
            node = %node.name,
            skill = %name,
            "no dynamic skill store configured; pinned skill not injected"
        );
        return None;
    };

    let store = Arc::clone(store);
    let fetch_name = name.to_string();
    match tokio::task::spawn_blocking(move || store.get(&fetch_name)).await {
        Ok(Ok(content)) => Some(content.instructions),
        Ok(Err(e)) => {
            tracing::warn!(
                node = %node.name,
                skill = %name,
                error = %e,
                "could not resolve pinned skill; continuing without it"
            );
            None
        }
        Err(join_err) => {
            tracing::warn!(
                node = %node.name,
                skill = %name,
                error = %join_err,
                "pinned skill resolution task failed; continuing without it"
            );
            None
        }
    }
}

pub(crate) fn append_skill_result(prompt: &mut String, name: &str, resolved: Option<String>) {
    match resolved {
        Some(instructions) => {
            prompt.push_str(&render_pinned_skill_section(name, &instructions));
        }
        None => prompt.push_str(&render_unresolved_skill_note(name)),
    }
}

/// CM13: the single fact CM13 classifies on — did this run finalize its own
/// row by calling `graph_complete_node` / `graph_report_blocker`? The run row's
/// status leaves `Running` only when a report call wrote to it. Routing
/// (`is_infra_crash_shape`), the ensemble fallthrough (same function), and the
/// self-reported-result path (`self_reported_execution`) all read this, so the
/// three can never disagree.
pub(crate) fn run_self_reported(run: &GraphNodeRun) -> bool {
    run.status != GraphRunStatus::Running
}

/// If the agent finalized its own run row (called `graph_complete_node` /
/// `graph_report_blocker`), turn that self-reported status into the node's
/// result; otherwise `None` so the caller uses the process-derived execution.
pub(crate) fn self_reported_execution(
    run: Option<&GraphNodeRun>,
    node: &GraphNode,
) -> Option<NodeExecution> {
    let run = run?;
    if !run_self_reported(run) {
        return None;
    }
    Some(NodeExecution {
        status: run.status,
        output: run.output.clone().unwrap_or_else(|| serde_json::json!({})),
        summary: format!("Agent node '{}' reported its own result.", node.name),
    })
}

#[cfg(test)]
mod check_command_tests {
    use super::*;

    fn ctx<'a>(spec_name: &'a str, head: Option<&'a str>) -> CheckCommandCtx<'a> {
        CheckCommandCtx {
            graph_id: "wf-test",
            graph_name: "Graph",
            workdir: "/work/dir",
            spec_id: "spec-test",
            spec_name,
            spec_start_head: head,
            spec_start_dirty: Some(2),
            spec_committed_head: None,
            spec_code: None,
            spec_commit_message: None,
            spec_scope: None,
            spec_type: None,
        }
    }

    #[test]
    fn check_command_quotes_spec_name_with_spaces() {
        let rendered =
            render_check_command("test {{spec_name}} = 'a b'", &ctx("a b", None)).unwrap();
        assert_eq!(rendered, "test 'a b' = 'a b'");
    }

    #[test]
    fn check_command_round_trips_an_injection_shaped_name() {
        let name = "x'; touch /tmp/p; '";
        let rendered = render_check_command("printf %s {{spec_name}}", &ctx(name, None)).unwrap();
        assert_eq!(rendered, format!("printf %s {}", shell_quote(name)));
    }

    #[test]
    fn check_command_strips_prequoted_quoted_markers() {
        let rendered =
            render_check_command("n='{{spec_name}}'; echo $n", &ctx("a b", None)).unwrap();
        assert_eq!(rendered, "n='a b'; echo $n");
    }

    #[test]
    fn check_command_head_markers_stay_raw() {
        let rendered = render_check_command(
            "{{spec_start_head}}|{{spec_start_dirty}}|{{spec_committed_head}}",
            &ctx("s", Some("abc")),
        )
        .unwrap();
        assert_eq!(rendered, "abc|2|");
    }

    #[test]
    fn check_command_refuses_unknown_marker_with_exact_message() {
        let err = render_check_command("echo {{nope}}", &ctx("s", None)).unwrap_err();
        assert_eq!(err.to_string(), "unknown marker {{nope}} in check command");
        assert_eq!(
            CHECK_COMMAND_RAW_MARKERS,
            &["spec_start_head", "spec_start_dirty", "spec_committed_head"]
        );
    }

    #[test]
    fn check_command_env_vars_lists_every_variable() {
        let vars = check_command_env_vars(
            &ctx("a b", Some("abc")),
            "/scratch/spec.md",
            "/scratch/spec-notes.md",
        );
        let pairs: Vec<(String, String)> = [
            ("CANOPY_SPEC_ID", "spec-test"),
            ("CANOPY_SPEC_NAME", "a b"),
            ("CANOPY_GRAPH_ID", "wf-test"),
            ("CANOPY_GRAPH_NAME", "Graph"),
            ("CANOPY_SPEC_CODE", ""),
            ("CANOPY_SPEC_TYPE", ""),
            ("CANOPY_SPEC_SCOPE", ""),
            ("CANOPY_SPEC_COMMIT_MESSAGE", ""),
            ("CANOPY_SPEC_START_HEAD", "abc"),
            ("CANOPY_SPEC_FILE", "/scratch/spec.md"),
            ("CANOPY_SPEC_NOTES_FILE", "/scratch/spec-notes.md"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
        assert_eq!(vars, pairs);
    }

    /// A typed spec exposes its code, type, scope, and prebuilt commit
    /// message as CANOPY_SPEC_* env vars — e.g. `FIX-1` and
    /// `fix(tui): open F1 from inside a session`.
    #[test]
    fn check_command_env_vars_with_spec_type_scope_code() {
        let ctx = CheckCommandCtx {
            graph_id: "wf-test",
            graph_name: "Graph",
            workdir: "/work/dir",
            spec_id: "spec-f1",
            spec_name: "open F1 from inside a session",
            spec_start_head: None,
            spec_start_dirty: None,
            spec_committed_head: None,
            spec_code: Some("FIX-1"),
            spec_commit_message: Some("fix(tui): open F1 from inside a session"),
            spec_scope: Some("tui"),
            spec_type: Some(SpecType::Fix),
        };
        let vars = check_command_env_vars(&ctx, "/scratch/spec.md", "/scratch/spec-notes.md");
        let map: std::collections::HashMap<String, String> = vars.into_iter().collect();
        assert_eq!(map["CANOPY_SPEC_CODE"], "FIX-1");
        assert_eq!(map["CANOPY_SPEC_TYPE"], "fix");
        assert_eq!(map["CANOPY_SPEC_SCOPE"], "tui");
        assert_eq!(
            map["CANOPY_SPEC_COMMIT_MESSAGE"],
            "fix(tui): open F1 from inside a session"
        );
    }

    #[test]
    fn check_notes_file_names_designer_section() {
        use crate::db::graphs::notes::NewSpecNote;
        let dir = tempfile::tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.insert_graph_spec(&GraphSpec {
            id: "s".to_string(),
            graph_id: None,
            name: "Spec".to_string(),
            description: None,
            position: 1,
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
        })
        .unwrap();
        let node = GraphNode {
            id: "n".to_string(),
            spec_id: None,
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        let spec = db.get_graph_spec("s").unwrap().unwrap();
        db.spec_note_write(NewSpecNote {
            spec_id: "s",
            owner_kind: "node",
            owner_id: "designer",
            owner_name: "Designer",
            body: "the plan",
            author_platform: None,
            author_model: None,
        })
        .unwrap();
        let (_scratch, path) = write_check_notes_file(&db, &node, &spec).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("## Designer\nthe plan"),
            "check file must carry the Designer section, got: {text}"
        );

        // A real check-command read of the file sees the Designer section —
        // `PLAN_PRESENT` becomes `grep -q '^## Designer' "$CANOPY_SPEC_NOTES_FILE"`.
        #[cfg(unix)]
        {
            let status = std::process::Command::new("grep")
                .arg("-q")
                .arg("^## Designer")
                .arg(&path)
                .status()
                .unwrap();
            assert!(
                status.success(),
                "a check command reading $CANOPY_SPEC_NOTES_FILE must see the Designer section"
            );
        }
    }

    #[test]
    fn check_command_spec_file_holds_description_and_is_removed_on_drop() {
        let node = GraphNode {
            id: "n".to_string(),
            spec_id: None,
            graph_id: None,
            name: "check".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({}),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        let spec = GraphSpec {
            id: "s".to_string(),
            graph_id: None,
            name: "Spec".to_string(),
            description: Some("Objective:\n- quoted".to_string()),
            position: 1,
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
        };
        let (scratch, path) = write_check_spec_file(&node, &spec).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Objective:\n- quoted"
        );
        assert!(!path.is_empty());
        drop(scratch);
        assert!(!std::path::Path::new(&path).exists());
    }

    #[test]
    fn check_command_marker_error_names_node_and_marker() {
        let node = |command: &str, name: &str| GraphNode {
            id: "n".to_string(),
            spec_id: None,
            graph_id: Some("g".to_string()),
            name: name.to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({ "command": command }),
            position: 1,
            created_at: chrono::Utc::now(),
        };
        let details = crate::domain::graphs::GraphDetails {
            lp: crate::domain::graphs::Graph {
                archived: false,
                paused_by_reconciliation: false,
                allow_dirty_start: false,
                infra_node_id: None,
                id: "g".to_string(),
                name: "Graph".to_string(),
                description: None,
                workdir: "/w".to_string(),
                status: crate::domain::graphs::GraphStatus::Draft,
                trigger: None,
                created_at: chrono::Utc::now(),
                started_at: None,
                completed_at: None,
                autorun_at: None,
                auto_continue_at: None,
                auto_continue_action: None,
                active_run_queue_id: None,
                hooks: std::collections::BTreeMap::new(),
                require_spec_type: false,
            },
            graph_nodes: vec![node("echo {{nope}}", "gatekeeper")],
            graph_edges: vec![],
            specs: vec![],
            completion_hook_runs: vec![],
        };
        let message = check_command_marker_error(&details).unwrap();
        assert_eq!(
            message,
            "unknown marker {{nope}} in check command (check node 'gatekeeper')."
        );

        // FR4 must also reach check nodes bound to a spec, not just the
        // graph's own nodes — drop the graph-level one and keep the defect
        // inside `specs[].nodes` only.
        let mut spec_only = details;
        spec_only.graph_nodes.clear();
        spec_only.specs = vec![crate::domain::graphs::GraphSpecDetails {
            spec: GraphSpec {
                id: "s".to_string(),
                graph_id: Some("g".to_string()),
                name: "Spec".to_string(),
                description: None,
                position: 1,
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
            },
            nodes: vec![node("echo {{specnope}}", "spec-gate")],
            edges: vec![],
        }];
        assert_eq!(
            check_command_marker_error(&spec_only).unwrap(),
            "unknown marker {{specnope}} in check command (check node 'spec-gate')."
        );
    }
}
