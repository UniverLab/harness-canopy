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
    // SPECCODE: change type, scope, code and commit message ride alongside
    // the spec name so deterministic check nodes can commit with
    // `$CANOPY_SPEC_COMMIT_MESSAGE` and templates can branch on type/code.
    "spec_code",
    "spec_type",
    "spec_scope",
    "spec_commit_message",
    "node_id",
    "previous_feedback",
];

#[expect(clippy::too_many_arguments)]
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
    spec_notes_section: &str,
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
    let spec_commit_message = crate::domain::spec_type::commit_message_or_default(
        spec.spec_type,
        spec.spec_scope.as_deref(),
        &spec.name,
    );
    let prompt = crate::domain::prompts::render_template(prompt_template, |raw| match raw {
        "graph_name" => Some(lp.name.clone()),
        "workdir" => Some(workdir.to_string()),
        "spec_id" => Some(spec.id.clone()),
        "spec_name" => Some(spec.name.clone()),
        "spec_content" => Some(spec_content.to_string()),
        "spec_code" => Some(spec.spec_code.clone().unwrap_or_default()),
        "spec_type" => Some(crate::domain::spec_type::type_str(spec.spec_type).to_string()),
        "spec_scope" => {
            Some(crate::domain::spec_type::scope_str(spec.spec_scope.as_deref()).to_string())
        }
        "spec_commit_message" => Some(spec_commit_message.clone()),
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
    // CM47: engine-owned `[SPEC NOTES]` travels with every cold-start prompt,
    // like `[REPORTING]` — no template placeholder needed. Pre-built by the
    // caller (which has DB access); `""` for previews that skip notes.
    let spec_notes_block = if spec_notes_section.trim().is_empty() {
        String::new()
    } else {
        format!("{}\n\n", spec_notes_section.trim_end())
    };
    Ok(format!(
        "# [GRAPH CONTEXT]\n<graph>\n  <name>{}</name>\n  <spec>{}</spec>\n  <node>{}</node>\n  <workdir>{}</workdir>\n</graph>\n{}\n# [SPEC]\n{}\n\n# [PREVIOUS FEEDBACK]\n{}\n\n{}# [REPORTING]\nWhen you finish this node, call graph_complete_node with run_id=\"{}\", node_id=\"{}\", status=\"pass\"|\"fail\", a concise summary, and your output.\nIf you are blocked and need human intervention, call graph_report_blocker with run_id=\"{}\", node_id=\"{}\" and the blocker description.\n",
        lp.name,
        spec.name,
        node.name,
        workdir,
        continuation_notice,
        prompt,
        previous_feedback,
        spec_notes_block,
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
#[expect(clippy::too_many_arguments)]
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

/// Tag heading the reporting-nudge section: an agent attempt that exited
/// without filing a verdict is re-dispatched once with this appended, so a
/// test (or a human reading the prompt) can tell a nudged retry apart from
/// an ordinary attempt.
pub(crate) const REPORT_NUDGE_TAG: &str = "# [REPORT REQUIRED]";

/// The reporting-nudge section: the previous attempt exited without a
/// verdict, printed text is never a verdict, and the agent must call
/// `graph_complete_node` exactly once now. Appended by
/// [`with_report_nudge`] to the retry's prompt (cold or resumed).
pub(crate) fn report_nudge_section(run_id: &str, node_id: &str) -> String {
    format!(
        "{REPORT_NUDGE_TAG}\nYour previous attempt exited without reporting a verdict: no verdict was recorded for that run. Printed text is never a verdict — only a graph_complete_node call counts. Call graph_complete_node exactly once now with run_id=\"{run_id}\", node_id=\"{node_id}\", your verdict (status=\"pass\"|\"fail\"), a concise summary, and your output.\n"
    )
}

/// Append the [`report_nudge_section`] to `prompt` when `on` (a spent
/// reporting nudge), else return the prompt unchanged.
pub(crate) fn with_report_nudge(prompt: String, on: bool, run_id: &str, node_id: &str) -> String {
    if !on {
        return prompt;
    }
    format!("{prompt}\n\n{}", report_nudge_section(run_id, node_id))
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

/// CM47: budget for the `[SPEC NOTES]` prompt section, in characters.
pub(crate) const SPEC_NOTES_CHAR_BUDGET: usize = 5_000;

/// CM47: fixed instructions closing every `[SPEC NOTES]` section.
pub(crate) const SPEC_NOTES_FOOTER: &str = "Write your note with spec_note_write(run_id, body); it replaces your previous note; keep it at most 2 500 characters; write it as you go (what you did, what is left, decisions a successor must not redo), not only at the end.";

/// CM47: one note's already-fetched fields for [`compose_spec_notes_section`].
/// A view (not the DB row) so the composer never touches the DB and the
/// ordering/budget logic is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SpecNoteView<'a> {
    pub owner_kind: &'a str,
    pub owner_id: &'a str,
    pub owner_name: &'a str,
    pub body: &'a str,
    pub updated_at: &'a str,
}

fn spec_note_stub(note: &SpecNoteView<'_>) -> String {
    format!(
        "{}: {} chars, updated {} — read with spec_notes_get\n",
        note.owner_name,
        note.body.chars().count(),
        note.updated_at
    )
}

/// CM47: build the `[SPEC NOTES]` section body (header included). `caller` is
/// the owner of the node being prompted; `designer` is the owner of the
/// graph's first agent node or ensemble after Start, in DFS order (see
/// [`resolve_designer_owner`]). `notes` is the spec's notes, newest-first.
///
/// Order: the caller's own note first, then the Designer's, then the rest
/// newest-first. Each note is included whole or as a stub — never truncated —
/// so the same notes always give the same section bytes (prompt caching
/// holds). With no notes, the section is the header plus the instructions.
pub(crate) fn compose_spec_notes_section(
    notes: &[SpecNoteView<'_>],
    caller: Option<(&str, &str)>,
    designer: Option<(&str, &str)>,
) -> String {
    let mut out = String::from("# [SPEC NOTES]\n\n");
    // The 5 000-character budget covers note bodies only — the fixed footer
    // is instructions, not notes, so bodies of 2400 + 2400 fit it whole.
    let mut budget = SPEC_NOTES_CHAR_BUDGET;

    let is_caller = |note: &SpecNoteView<'_>| match caller {
        Some((kind, id)) => note.owner_kind == kind && note.owner_id == id,
        None => false,
    };
    let is_designer = |note: &SpecNoteView<'_>| match designer {
        Some((kind, id)) => note.owner_kind == kind && note.owner_id == id,
        None => false,
    };
    let caller_note = notes.iter().find(|note| is_caller(note));
    let designer_note = notes
        .iter()
        .find(|note| is_designer(note) && Some(**note) != caller_note.copied());
    let mut sequence: Vec<&SpecNoteView<'_>> = Vec::new();
    if let Some(note) = caller_note {
        sequence.push(note);
    }
    if let Some(note) = designer_note {
        sequence.push(note);
    }
    for note in notes {
        if !sequence.contains(&note) {
            sequence.push(note);
        }
    }

    for note in sequence {
        // The budget counts note bodies only: `## <name>` framing is a few
        // fixed bytes the composer itself emits, so bodies of 2400 + 2400 fit
        // a 5000 budget and the 900-char third note becomes the stub.
        let len = note.body.chars().count();
        if len <= budget {
            out.push_str(&format!("## {}\n{}\n", note.owner_name, note.body));
            budget -= len;
        } else {
            let stub = spec_note_stub(note);
            let stub_len = stub.chars().count();
            if stub_len > budget {
                break;
            }
            out.push_str(&stub);
            budget -= stub_len;
        }
    }
    out.push_str(SPEC_NOTES_FOOTER);
    out.push('\n');
    out
}

/// CM47: the Designer is the owner of the graph's first agent node or
/// ensemble after Start, in DFS order (following `pass`/`always` edges).
/// Works over already-fetched `nodes`/`edges` plus a member→ensemble map, so
/// the call site never pays a DB round trip per prompt. Returns the
/// `(kind, id)` owner pair for [`compose_spec_notes_section`].
pub(crate) fn resolve_designer_owner(
    nodes: &[GraphNode],
    edges: &[GraphEdge],
    member_to_ensemble: &HashMap<String, String>,
) -> Option<(String, String)> {
    let entry = routing::find_entry_node(nodes, edges, "designer").ok()?;
    let mut outgoing: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if matches!(
            edge.condition,
            GraphEdgeCondition::Pass | GraphEdgeCondition::Always
        ) {
            outgoing
                .entry(edge.from_node.as_str())
                .or_default()
                .push(edge.to_node.as_str());
        }
    }
    let by_id: HashMap<&str, &GraphNode> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut stack = vec![entry.as_str()];
    while let Some(node_id) = stack.pop() {
        if !visited.insert(node_id) {
            continue;
        }
        if node_id != entry.as_str() {
            if let Some(ensemble_id) = member_to_ensemble.get(node_id) {
                return Some(("ensemble".to_string(), ensemble_id.clone()));
            }
            if let Some(node) = by_id.get(node_id) {
                if node.kind == GraphNodeKind::Agent {
                    return Some(("node".to_string(), node.id.clone()));
                }
            }
        }
        if let Some(next) = outgoing.get(node_id) {
            for target in next.iter().rev() {
                stack.push(target);
            }
        }
    }
    None
}

#[cfg(test)]
mod spec_notes_tests {
    use super::*;

    fn view<'a>(kind: &'a str, id: &'a str, name: &'a str, body: &'a str) -> SpecNoteView<'a> {
        SpecNoteView {
            owner_kind: kind,
            owner_id: id,
            owner_name: name,
            body,
            updated_at: "2026-10-08T00:00:00Z",
        }
    }

    #[test]
    fn empty_notes_give_instructions_only() {
        let section = compose_spec_notes_section(&[], None, None);
        assert!(section.starts_with("# [SPEC NOTES]"));
        assert!(section.contains(SPEC_NOTES_FOOTER));
        assert!(!section.contains("## "));
    }

    #[test]
    fn caller_first_then_designer_then_newest() {
        let impl_body = "implementer progress";
        let designer_body = "designer plan";
        let other_body = "other context";
        let notes = vec![
            view("node", "other", "Other", other_body),
            view("node", "designer", "Designer", designer_body),
            view("node", "impl", "Implementer", impl_body),
        ];
        let section =
            compose_spec_notes_section(&notes, Some(("node", "impl")), Some(("node", "designer")));
        let impl_pos = section.find("## Implementer").unwrap();
        let designer_pos = section.find("## Designer").unwrap();
        let other_pos = section.find("## Other").unwrap();
        assert!(
            impl_pos < designer_pos && designer_pos < other_pos,
            "caller, then Designer, then the rest newest-first"
        );
        assert!(section.contains(impl_body));
        assert!(section.contains(designer_body));
        assert!(section.contains(other_body));
    }

    #[test]
    fn oversized_third_note_becomes_stub() {
        let caller_body = "x".repeat(2400);
        let designer_body = "y".repeat(2400);
        let other_body = "z".repeat(900);
        let caller = view("node", "impl", "Implementer", &caller_body);
        let designer = view("node", "designer", "Designer", &designer_body);
        let other = view("node", "other", "Other", &other_body);
        let notes = vec![other, designer, caller];
        let section =
            compose_spec_notes_section(&notes, Some(("node", "impl")), Some(("node", "designer")));
        assert!(
            section.contains(&caller_body),
            "caller's 2400-char note must be whole"
        );
        assert!(
            section.contains(&designer_body),
            "Designer's 2400-char note must be whole"
        );
        assert!(
            !section.contains(&other_body),
            "the 900-char note must not fit whole after caller + Designer"
        );
        assert!(
            section.contains("Other: 900 chars, updated"),
            "overflow becomes a stub naming owner, size and time"
        );
        assert!(section.contains("read with spec_notes_get"));
    }

    fn topo_node(id: &str, name: &str, kind: GraphNodeKind) -> GraphNode {
        GraphNode {
            id: id.to_string(),
            spec_id: Some("spec".to_string()),
            graph_id: None,
            name: name.to_string(),
            kind,
            config: serde_json::json!({}),
            position: 0,
            created_at: chrono::Utc::now(),
        }
    }

    fn topo_edge(id: &str, from: &str, to: &str) -> GraphEdge {
        GraphEdge {
            id: id.to_string(),
            spec_id: Some("spec".to_string()),
            graph_id: None,
            from_node: from.to_string(),
            to_node: to.to_string(),
            condition: GraphEdgeCondition::Always,
        }
    }

    #[test]
    fn designer_is_first_agent_after_start() {
        let nodes = vec![
            topo_node("start", "Start", GraphNodeKind::Check),
            topo_node("designer", "Designer", GraphNodeKind::Agent),
            topo_node("impl", "Implementer", GraphNodeKind::Agent),
        ];
        let edges = vec![
            topo_edge("e1", "start", "designer"),
            topo_edge("e2", "designer", "impl"),
        ];
        let map = HashMap::new();
        assert_eq!(
            resolve_designer_owner(&nodes, &edges, &map),
            Some(("node".to_string(), "designer".to_string()))
        );
    }

    #[test]
    fn designer_is_ensemble_when_member_comes_first() {
        let nodes = vec![
            topo_node("start", "Start", GraphNodeKind::Check),
            topo_node("m1", "Member", GraphNodeKind::Agent),
        ];
        let edges = vec![topo_edge("e1", "start", "m1")];
        let mut map = HashMap::new();
        map.insert("m1".to_string(), "ens-1".to_string());
        assert_eq!(
            resolve_designer_owner(&nodes, &edges, &map),
            Some(("ensemble".to_string(), "ens-1".to_string()))
        );
    }
}
