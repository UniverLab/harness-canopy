use super::*;

/// Above this many bytes, `{{previous_feedback}}` is elided to head+tail with
/// a marker instead of interpolated in full. This is a defensive bound that
/// applies regardless of prompt transport (argv or stdin): a prior node can
/// emit an arbitrarily large output (e.g. a full `cargo test` log), and
/// nothing about interpolating it whole into the next prompt is actually
/// useful past a point. The full output is never lost — it stays in
/// `graph_runs.output` for humans to inspect.
pub(crate) const PREVIOUS_FEEDBACK_ELISION_THRESHOLD: usize = 16 * 1024;

/// Maximum prompt size (in bytes) that is safe to pass via argv. Linux's
/// `MAX_ARG_STRLEN` is 128KiB; we leave headroom for other argv elements
/// (headless flags, model flag, working dir flag) by using 100KiB. When
/// the composed prompt exceeds this, the graph engine forces stdin delivery
/// regardless of the CLI's `prompt_via_stdin` registry setting.
pub(crate) const ARGV_SAFETY_THRESHOLD: usize = 100 * 1024;

/// Elide the middle of `text` with a marker once it exceeds
/// `PREVIOUS_FEEDBACK_ELISION_THRESHOLD`, keeping head and tail (each half
/// the threshold) intact. Slices on char boundaries so it never panics on
/// multi-byte UTF-8 content.
pub(crate) fn bound_previous_feedback(text: String) -> String {
    if text.len() <= PREVIOUS_FEEDBACK_ELISION_THRESHOLD {
        return text;
    }

    let half = PREVIOUS_FEEDBACK_ELISION_THRESHOLD / 2;
    let head_end = floor_char_boundary(&text, half);
    let tail_start = ceil_char_boundary(&text, text.len() - half);
    let elided_bytes = tail_start - head_end;

    format!(
        "{}\n[...{} bytes elided...]\n{}",
        &text[..head_end],
        elided_bytes,
        &text[tail_start..]
    )
}

pub(crate) fn floor_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

pub(crate) fn ceil_char_boundary(s: &str, index: usize) -> usize {
    let mut i = index.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Resolve an agent node's prompt template at SPAWN time (P1), not at node
/// creation. Precedence: an explicit `prompt_template` in the node config
/// always wins; otherwise a `prompt_preset` name is resolved against
/// `<prompts_dir>/<name>.md` (falling back to the hardcoded seed constant,
/// with a WARN, when the file is missing or unreadable — see
/// `domain::prompts::resolve_prompt_preset`); otherwise the engine's own
/// default template. Resolving at spawn time (rather than baking the prompt
/// into the node's config once) is what lets a user's edit to a preset file
/// take effect on the very next run without touching the node itself.
pub(crate) fn resolve_node_prompt_template(
    node: &GraphNode,
    prompts_dir: &std::path::Path,
) -> String {
    if let Some(template) = node
        .config
        .get("prompt_template")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return template.to_string();
    }
    if let Some(preset_name) = node
        .config
        .get("prompt_preset")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return crate::domain::prompts::resolve_prompt_preset(prompts_dir, preset_name);
    }
    "{{spec_content}}\n\n{{previous_feedback}}".to_string()
}

/// Classifies which branch of [`resolve_node_prompt_template`]'s precedence
/// an agent node's config will actually take — `"explicit"` (`prompt_template`
/// set), `"preset"` (`prompt_preset` set), or `"default_fallback"` (neither,
/// so the node silently runs on the bare fallback template nobody chose).
/// Read-only mirror of that function's own precedence check — never the
/// other way around, so the two can't drift. Surfaced in `graph_get`'s node
/// JSON (`daemon::handler::graph_node_json`) and recorded on every agent run's
/// output, so a node running on the default is distinguishable from one
/// running its author's prompt without having to inspect its raw config.
pub(crate) fn agent_prompt_source(config: &Value) -> &'static str {
    let has_non_empty_str = |field: &str| {
        config
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    if has_non_empty_str("prompt_template") {
        "explicit"
    } else if has_non_empty_str("prompt_preset") {
        "preset"
    } else {
        "default_fallback"
    }
}

/// Refuse to render `template` when it carries a `{{name}}` marker not in
/// `supported` — the CP1 contract: a prompt builder must never emit a
/// template presented as an instruction. Mirrors
/// [`crate::domain::prompts::render_preset`]'s refusal, but for the engine's
/// own `.replace()`-based renderers, and checks the *template* (not the
/// rendered output) so a `{{...}}` sequence inside a bound value — a spec
/// body, prior feedback — is treated as data, not a leftover marker.
pub(crate) fn refuse_unbindable_template(
    node_name: &str,
    template: &str,
    supported: &[&str],
    available_node_names: Option<&[String]>,
) -> Result<()> {
    let mut extended: Vec<&str> = supported.to_vec();
    let mut owned: Vec<String> = Vec::new();
    if let Some(node_names) = available_node_names {
        for name in node_names {
            owned.push(format!("output:{name}"));
        }
        for s in &owned {
            extended.push(s.as_str());
        }
    }
    let unbindable = crate::domain::prompts::unbindable_placeholders(template, &extended);
    if unbindable.is_empty() {
        return Ok(());
    }
    tracing::error!(
        node = %node_name,
        placeholders = ?unbindable,
        "prompt template carries markers no binding covers; refusing to emit"
    );
    Err(anyhow!(
        "Node '{}' prompt template carries {} no binding covers ({}). Refusing to spawn \
         rather than send an agent a template it would read as an instruction — fix the \
         template or preset so every {{{{...}}}} marker is one the engine substitutes, \
         or write a marker meant as text escaped as \\{{{{...}}}}.",
        node_name,
        if unbindable.len() == 1 {
            "a placeholder"
        } else {
            "placeholders"
        },
        unbindable.join(", "),
    ))
}

/// Substitute `{{output:Name}}` markers via the shared escape-aware renderer
/// ([`crate::domain::prompts::render_template`]): escaped markers stay literal,
/// every other `{{...}}` marker passes through untouched. Kept as the focused
/// unit for output substitution; the prompt renderers below inline the same
/// helper with their full binding set so bound values are never rescanned.
#[allow(dead_code)]
pub(crate) fn substitute_named_outputs(
    template: &str,
    node_outputs: &HashMap<String, Value>,
    all_node_names: &[String],
) -> String {
    crate::domain::prompts::render_template(template, |raw| {
        raw.strip_prefix("output:").map(|node_name| {
            if let Some(output) = node_outputs.get(node_name) {
                serde_json::to_string_pretty(output).unwrap_or_else(|_| "(none)".to_string())
            } else if all_node_names.iter().any(|n| n == node_name) {
                "(not yet executed)".to_string()
            } else {
                "(unknown node)".to_string()
            }
        })
    })
}

/// The only `{{...}}` markers [`render_agent_prompt`] can bind. A resolved
/// template — an explicit `prompt_template`, a `prompt_preset` body, or the
/// default fallback — that carries any other marker is refused (CP1): the
/// engine will not spawn an agent on a prompt still holding a literal
/// `{{name}}` the agent would read as an instruction.
pub(crate) const AGENT_PROMPT_BINDINGS: &[&str] = &[
    "graph_name",
    "workdir",
    "spec_id",
    "spec_name",
    "spec_content",
    "node_id",
    "previous_feedback",
];

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_agent_prompt(
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    prompt_template: &str,
    previous_output: Option<&Value>,
    workdir: &str,
    run_id: &str,
    node_outputs: &HashMap<String, Value>,
    all_node_names: &[String],
) -> Result<String> {
    refuse_unbindable_template(
        &node.name,
        prompt_template,
        AGENT_PROMPT_BINDINGS,
        Some(all_node_names),
    )?;

    let previous_feedback = previous_output
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(none)".to_string());
    let previous_feedback = bound_previous_feedback(previous_feedback);
    let spec_content = spec.description.as_deref().unwrap_or(&spec.name);
    let prompt = crate::domain::prompts::render_template(prompt_template, |raw| match raw {
        "graph_name" => Some(lp.name.clone()),
        "workdir" => Some(workdir.to_string()),
        "spec_id" => Some(spec.id.clone()),
        "spec_name" => Some(spec.name.clone()),
        "spec_content" => Some(spec_content.to_string()),
        "node_id" => Some(node.id.clone()),
        "previous_feedback" => Some(previous_feedback.clone()),
        _ => raw.strip_prefix("output:").map(|node_name| {
            if let Some(output) = node_outputs.get(node_name) {
                serde_json::to_string_pretty(output).unwrap_or_else(|_| "(none)".to_string())
            } else if all_node_names.iter().any(|n| n == node_name) {
                "(not yet executed)".to_string()
            } else {
                "(unknown node)".to_string()
            }
        }),
    });

    // A spec picked up in `Interrupted` status has a previous attempt's
    // partial work sitting in `workdir` — the engine no longer `git stash`es
    // it away, so it's exactly where that attempt left it. This agent has no
    // memory of that attempt (it's a cold start, a fresh process/session),
    // so the prompt has to say so explicitly: the work was cut short by
    // something external (a daemon restart, a crash), not set aside for
    // being wrong, and the right move is to inspect what's there and
    // continue it rather than redo it from scratch.
    let continuation_notice = if spec.status == GraphSpecStatus::Interrupted {
        format!(
            "\n# [CONTINUATION]\nA previous attempt at this spec was interrupted by something external — a daemon restart, a machine crash, or an unrelated process — not by any problem with the work itself. The working tree at {workdir} may already hold that attempt's partial progress, left exactly as it was. Before doing anything else, run `git status` and `git diff` there to see what already exists, and continue from it rather than starting over. (If {workdir} is not a git repository, inspect it directly instead — the same partial work may still be present.)\n"
        )
    } else {
        String::new()
    };

    // `run_id` (not just `node_id`) must round-trip through the report tools
    // (B12): a node can be retried, so more than one run can exist for the
    // same `node_id` over a spec's lifetime. Without the exact run_id, a
    // report arriving late from a killed/superseded attempt (e.g. a timed-out
    // agent that ignores its own termination and calls the tool anyway) would
    // otherwise be matched to "whatever's currently active for this node_id"
    // and silently corrupt a newer, unrelated run.
    Ok(format!(
        "# [GRAPH CONTEXT]\n<graph>\n  <name>{}</name>\n  <spec>{}</spec>\n  <node>{}</node>\n  <workdir>{}</workdir>\n</graph>\n{}\n# [SPEC]\n{}\n\n# [PREVIOUS FEEDBACK]\n{}\n\n# [REPORTING]\nWhen you finish this node, call graph_complete_node with run_id=\"{}\", node_id=\"{}\", status=\"pass\"|\"fail\", a concise summary, and your output.\nIf you are blocked and need human intervention, call graph_report_blocker with run_id=\"{}\", node_id=\"{}\" and the blocker description.\n",
        lp.name,
        spec.name,
        node.name,
        workdir,
        continuation_notice,
        prompt,
        previous_feedback,
        run_id,
        node.id,
        run_id,
        node.id
    ))
}

/// Default incremental prompt for a SAME-SPEC resumed agent run (RS2): a
/// fail-edge bounce or B19 infra retry, where the resumed session was
/// captured by THIS spec earlier in this same dispatch. Deliberately omits
/// the full `[GRAPH CONTEXT]`/`[SPEC]` block that a cold start renders: the
/// resumed session already holds all of that in its own history, so
/// re-sending it wastes tokens and can confuse the model into re-reading the
/// whole task. Only the new feedback and a one-line reminder of the reporting
/// contract are sent. Overridable per node via the `resume_prompt` config
/// key — but only for this same-spec case; see
/// [`RESUME_PROMPT_CROSS_SPEC_DEFAULT`] for the other one.
pub(crate) const RESUME_PROMPT_DEFAULT: &str = "# [CONTINUE]\nYou are resuming your existing session for this task. The full task context is already in your session history — only the new feedback is included below. Address it, then report.\n\n# [PREVIOUS FEEDBACK]\n{{previous_feedback}}\n\n# [REPORTING]\nWhen you finish, call graph_complete_node with run_id=\"{{run_id}}\", node_id=\"{{node_id}}\", status=\"pass\"|\"fail\", a concise summary, and your output.\nIf you are blocked and need human intervention, call graph_report_blocker with run_id=\"{{run_id}}\", node_id=\"{{node_id}}\" and the blocker description.\n";

/// Default incremental prompt for a CROSS-SPEC resumed agent run (RS3): a
/// context-group handoff, where the session being resumed was captured by a
/// DIFFERENT spec — the previous grouped sibling on this node. Unlike
/// [`RESUME_PROMPT_DEFAULT`], the claim "the full task context is already in
/// your session history" is false here (the session has never seen THIS
/// spec), so it is never sent: this template renders `{{spec_content}}`
/// instead, plus a short boundary notice that the previous spec is finished
/// and already committed, so its conclusions are not to be restated as this
/// spec's own work. Selected by the engine itself, from its own state (which
/// spec captured the session being resumed) — never overridable via the
/// node's `resume_prompt` config key, since that choice depends on runtime
/// state a static per-node template cannot know.
pub(crate) const RESUME_PROMPT_CROSS_SPEC_DEFAULT: &str = "# [CONTINUE: NEW SPEC]\nYou are resuming your existing session, but for a NEW spec. The previous spec you were working on is finished and already committed — do not restate its conclusions or describe its prior work as this spec's output. Only the spec below is outstanding; address it, then report.\n\n# [SPEC]\n{{spec_content}}\n\n# [PREVIOUS FEEDBACK]\n{{previous_feedback}}\n\n# [REPORTING]\nWhen you finish, call graph_complete_node with run_id=\"{{run_id}}\", node_id=\"{{node_id}}\", status=\"pass\"|\"fail\", a concise summary, and your output.\nIf you are blocked and need human intervention, call graph_report_blocker with run_id=\"{{run_id}}\", node_id=\"{{node_id}}\" and the blocker description.\n";

/// Render a resumed run's incremental prompt (RS2/RS3) from `template` — the
/// node's `resume_prompt` override or [`RESUME_PROMPT_DEFAULT`] for a
/// same-spec bounce, [`RESUME_PROMPT_CROSS_SPEC_DEFAULT`] for a cross-spec
/// (RS3) handoff. The caller picks which (see `execute_agent_node`); this
/// function only renders whichever it's given. Same `{{previous_feedback}}`
/// bounding as [`render_agent_prompt`], plus the run/node/spec placeholders
/// the reporting contract needs. `{{spec_content}}` is substituted ONLY when
/// `template` asks for it — the same-spec default never does (the session
/// already has that spec in its history; re-rendering it wastes tokens and
/// invites regurgitation), but the cross-spec default does (that session has
/// never seen this spec).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_resume_prompt(
    lp: &crate::domain::graphs::Graph,
    spec: &GraphSpec,
    node: &GraphNode,
    template: &str,
    previous_output: Option<&Value>,
    workdir: &str,
    run_id: &str,
    node_outputs: &HashMap<String, Value>,
    all_node_names: &[String],
) -> Result<String> {
    refuse_unbindable_template(
        &node.name,
        template,
        RESUME_PROMPT_BINDINGS,
        Some(all_node_names),
    )?;

    let previous_feedback = previous_output
        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
        .unwrap_or_else(|| "(none)".to_string());
    let previous_feedback = bound_previous_feedback(previous_feedback);
    let spec_content = spec.description.as_deref().unwrap_or(&spec.name);
    Ok(crate::domain::prompts::render_template(
        template,
        |raw| match raw {
            "graph_name" => Some(lp.name.clone()),
            "workdir" => Some(workdir.to_string()),
            "spec_id" => Some(spec.id.clone()),
            "spec_name" => Some(spec.name.clone()),
            "spec_content" => Some(spec_content.to_string()),
            "node" => Some(node.name.clone()),
            "node_id" => Some(node.id.clone()),
            "run_id" => Some(run_id.to_string()),
            "previous_feedback" => Some(previous_feedback.clone()),
            _ => raw.strip_prefix("output:").map(|node_name| {
                if let Some(output) = node_outputs.get(node_name) {
                    serde_json::to_string_pretty(output).unwrap_or_else(|_| "(none)".to_string())
                } else if all_node_names.iter().any(|n| n == node_name) {
                    "(not yet executed)".to_string()
                } else {
                    "(unknown node)".to_string()
                }
            }),
        },
    ))
}

/// The only `{{...}}` markers [`render_resume_prompt`] can bind — the
/// same-spec ([`RESUME_PROMPT_DEFAULT`]) and cross-spec
/// ([`RESUME_PROMPT_CROSS_SPEC_DEFAULT`]) defaults, and any per-node
/// `resume_prompt` override, are all refused (CP1) if they carry anything
/// else.
pub(crate) const RESUME_PROMPT_BINDINGS: &[&str] = &[
    "graph_name",
    "workdir",
    "spec_id",
    "spec_name",
    "spec_content",
    "node",
    "node_id",
    "run_id",
    "previous_feedback",
];

/// Render the `on_completed` hook's prompt template (N2). The hook has no
/// spec/node graph context to template against (it fires once per whole run,
/// not per spec), so it supports a smaller, hook-specific placeholder set
/// rather than [`render_agent_prompt`]'s full one:
///
/// - `{{graph_name}}` / `{{workdir}}` — same meaning as the node-prompt
///   placeholders of the same name.
/// - `{{completed_specs}}` — name + one-line summary of each spec completed
///   *in this run* (the final node's own summary text), one per line;
///   `(none)` if this run completed zero specs (e.g. every spec was already
///   `completed`/`skipped` before this run started).
#[allow(dead_code)]
pub(crate) fn render_completion_hook_prompt(
    lp: &crate::domain::graphs::Graph,
    workdir: &str,
    completed_specs: &[(String, String)],
    prompt_template: &str,
) -> Result<String> {
    refuse_unbindable_template(
        "on_completed hook",
        prompt_template,
        COMPLETION_HOOK_BINDINGS,
        None,
    )?;

    let completed_specs_text = if completed_specs.is_empty() {
        "(none)".to_string()
    } else {
        completed_specs
            .iter()
            .map(|(name, summary)| format!("- {name}: {summary}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(crate::domain::prompts::render_template(
        prompt_template,
        |raw| match raw {
            "graph_name" => Some(lp.name.clone()),
            "workdir" => Some(workdir.to_string()),
            "completed_specs" => Some(completed_specs_text.clone()),
            _ => None,
        },
    ))
}

/// The only `{{...}}` markers [`render_completion_hook_prompt`] can bind; a
/// hook prompt carrying anything else is refused (CP1) and the firing is
/// recorded as failed rather than sent.
#[allow(dead_code)]
pub(crate) const COMPLETION_HOOK_BINDINGS: &[&str] = &["graph_name", "workdir", "completed_specs"];

/// Context passed to [`GraphEngine::fire_hooks`] so the renderer can bind
/// event-specific placeholders into the hook prompt. All events share
/// `graph_name` and `workdir`; the remaining fields are event-specific.
pub(crate) struct HookContext<'a> {
    pub(crate) graph_name: &'a str,
    pub(crate) workdir: &'a str,
    /// `on_completed`: name + summary of each spec completed this dispatch.
    pub(crate) completed_specs: &'a [(String, String)],
    /// `on_spec_completed`: the spec that just completed.
    pub(crate) spec_name: Option<&'a str>,
    pub(crate) spec_id: Option<&'a str>,
    /// `on_failed` / `on_blocked`: blocker description and the node that
    /// ended the run.
    pub(crate) blocker: Option<&'a str>,
    pub(crate) node_name: Option<&'a str>,
}

pub(crate) const GRAPH_HOOK_BINDINGS: &[&str] = &[
    "graph_name",
    "workdir",
    "completed_specs",
    "spec_name",
    "spec_id",
    "blocker",
    "node",
];

pub(crate) fn hook_env_vars(
    event: &GraphHookEvent,
    ctx: &HookContext<'_>,
) -> Vec<(String, String)> {
    let completed_specs = if ctx.completed_specs.is_empty() {
        "(none)".to_string()
    } else {
        ctx.completed_specs
            .iter()
            .map(|(name, summary)| format!("- {name}: {summary}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let vars = vec![
        (
            "CANOPY_HOOK_GRAPH_NAME".to_string(),
            ctx.graph_name.to_string(),
        ),
        (
            "CANOPY_HOOK_LOOP_NAME".to_string(),
            ctx.graph_name.to_string(),
        ),
        ("CANOPY_HOOK_WORKDIR".to_string(), ctx.workdir.to_string()),
        ("CANOPY_HOOK_COMPLETED_SPECS".to_string(), completed_specs),
        (
            "CANOPY_HOOK_SPEC_NAME".to_string(),
            ctx.spec_name.unwrap_or("(none)").to_string(),
        ),
        (
            "CANOPY_HOOK_SPEC_ID".to_string(),
            ctx.spec_id.unwrap_or("(none)").to_string(),
        ),
        (
            "CANOPY_HOOK_BLOCKER".to_string(),
            ctx.blocker.unwrap_or("(none)").to_string(),
        ),
        (
            "CANOPY_HOOK_NODE".to_string(),
            ctx.node_name.unwrap_or("(none)").to_string(),
        ),
        ("CANOPY_HOOK_EVENT".to_string(), event.as_str().to_string()),
    ];
    vars
}

/// The supported `{{...}}` markers for graph hooks.
pub(crate) fn hook_bindings_for_event(event: &GraphHookEvent) -> &'static [&'static str] {
    let _ = event;
    GRAPH_HOOK_BINDINGS
}

/// Render a hook prompt for the given event, binding only the placeholders
/// that event supports. Returns `Err` if the template carries unbindable
/// markers — the caller records a failed hook run instead of spawning.
pub(crate) fn render_hook_prompt(
    event: &GraphHookEvent,
    ctx: &HookContext<'_>,
    prompt_template: &str,
) -> Result<String> {
    let bindings = hook_bindings_for_event(event);
    refuse_unbindable_template(
        &format!("{} hook", event.as_str()),
        prompt_template,
        bindings,
        None,
    )?;

    let completed_specs_text = if ctx.completed_specs.is_empty() {
        "(none)".to_string()
    } else {
        ctx.completed_specs
            .iter()
            .map(|(name, summary)| format!("- {name}: {summary}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(crate::domain::prompts::render_template(
        prompt_template,
        |raw| match raw {
            "graph_name" => Some(ctx.graph_name.to_string()),
            "workdir" => Some(ctx.workdir.to_string()),
            "completed_specs" => Some(completed_specs_text.clone()),
            "spec_name" => Some(ctx.spec_name.unwrap_or("(none)").to_string()),
            "spec_id" => Some(ctx.spec_id.unwrap_or("(none)").to_string()),
            "blocker" => Some(ctx.blocker.unwrap_or("(none)").to_string()),
            "node" => Some(ctx.node_name.unwrap_or("(none)").to_string()),
            _ => None,
        },
    ))
}

/// Render a hook command for the given event, binding only the placeholders
/// that event supports. Returns `Err` if the template carries unbindable
/// markers — the caller records a failed hook run instead of executing.
pub(crate) fn render_hook_command(
    event: &GraphHookEvent,
    ctx: &HookContext<'_>,
    command_template: &str,
) -> Result<String> {
    let bindings = hook_bindings_for_event(event);
    refuse_unbindable_template(
        &format!("{} hook command", event.as_str()),
        command_template,
        bindings,
        None,
    )?;

    let completed_specs_text = if ctx.completed_specs.is_empty() {
        "(none)".to_string()
    } else {
        ctx.completed_specs
            .iter()
            .map(|(name, summary)| format!("- {name}: {summary}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // A template may already single-quote a marker. Remove only that exact
    // wrapper so the replacement's shell quoting is not nested inside it.
    let normalized_template = hook_bindings_for_event(event).iter().fold(
        command_template.to_string(),
        |template, marker| {
            template.replace(&format!("'{{{{{marker}}}}}'"), &format!("{{{{{marker}}}}}"))
        },
    );

    Ok(crate::domain::prompts::render_template(
        &normalized_template,
        |raw| match raw {
            "graph_name" => Some(shell_quote(ctx.graph_name)),
            "workdir" => Some(shell_quote(ctx.workdir)),
            "completed_specs" => Some(shell_quote(&completed_specs_text)),
            "spec_name" => Some(shell_quote(ctx.spec_name.unwrap_or("(none)"))),
            "spec_id" => Some(shell_quote(ctx.spec_id.unwrap_or("(none)"))),
            "blocker" => Some(shell_quote(ctx.blocker.unwrap_or("(none)"))),
            "node" => Some(shell_quote(ctx.node_name.unwrap_or("(none)"))),
            _ => None,
        },
    ))
}

/// Render a graph hook's `idea` template (CH4), binding the same placeholders
/// as the hook's event. Returns `Err` if the template carries unbindable
/// markers.
pub(crate) fn render_hook_idea(
    event: &GraphHookEvent,
    ctx: &HookContext<'_>,
    idea_template: &str,
) -> Result<String> {
    let bindings = hook_bindings_for_event(event);
    refuse_unbindable_template(
        &format!("{} hook idea", event.as_str()),
        idea_template,
        bindings,
        None,
    )?;

    let completed_specs_text = if ctx.completed_specs.is_empty() {
        "(none)".to_string()
    } else {
        ctx.completed_specs
            .iter()
            .map(|(name, summary)| format!("- {name}: {summary}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(crate::domain::prompts::render_template(
        idea_template,
        |raw| match raw {
            "graph_name" => Some(ctx.graph_name.to_string()),
            "workdir" => Some(ctx.workdir.to_string()),
            "completed_specs" => Some(completed_specs_text.clone()),
            "spec_name" => Some(ctx.spec_name.unwrap_or("(none)").to_string()),
            "spec_id" => Some(ctx.spec_id.unwrap_or("(none)").to_string()),
            "blocker" => Some(ctx.blocker.unwrap_or("(none)").to_string()),
            "node" => Some(ctx.node_name.unwrap_or("(none)").to_string()),
            _ => None,
        },
    ))
}
