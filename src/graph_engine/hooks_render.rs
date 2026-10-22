use super::*;

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
    /// SPECCODE: the change type (`feat`, `fix`, ...) — `None` on a legacy
    /// spec, where the SPECCODE hooks/env vars are empty just like the
    /// legacy ones they stand beside.
    pub(crate) spec_type: Option<SpecType>,
    pub(crate) spec_scope: Option<&'a str>,
    /// SPECCODE: engine-generated quick reference (`FIX-7`) or `None`.
    pub(crate) spec_code: Option<&'a str>,
    /// SPECCODE: prebuilt `<type>(<scope>): <name>`, precomputed by the caller.
    pub(crate) spec_commit_message: Option<&'a str>,
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
    // SPECCODE: the change-type fields so hook command/prompt/idea templates
    // can reference `{{spec_code}}` etc. — empty when the spec has no type.
    "spec_code",
    "spec_type",
    "spec_scope",
    "spec_commit_message",
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
        // SPECCODE: empty when the spec has no type — same as CANOPY_SPEC_*
        // on check nodes, so hook command templates see the same value surface.
        (
            "CANOPY_HOOK_SPEC_CODE".to_string(),
            ctx.spec_code.unwrap_or("").to_string(),
        ),
        (
            "CANOPY_HOOK_SPEC_TYPE".to_string(),
            crate::domain::spec_type::type_str(ctx.spec_type).to_string(),
        ),
        (
            "CANOPY_HOOK_SPEC_SCOPE".to_string(),
            ctx.spec_scope
                .filter(|s| !s.is_empty())
                .unwrap_or("")
                .to_string(),
        ),
        (
            "CANOPY_HOOK_SPEC_COMMIT_MESSAGE".to_string(),
            ctx.spec_commit_message.unwrap_or("").to_string(),
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
            "spec_code" => Some(ctx.spec_code.unwrap_or("").to_string()),
            "spec_type" => Some(crate::domain::spec_type::type_str(ctx.spec_type).to_string()),
            "spec_scope" => Some(
                ctx.spec_scope
                    .filter(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string(),
            ),
            "spec_commit_message" => Some(ctx.spec_commit_message.unwrap_or("").to_string()),
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
            "spec_code" => Some(shell_quote(ctx.spec_code.unwrap_or(""))),
            "spec_type" => Some(shell_quote(crate::domain::spec_type::type_str(
                ctx.spec_type,
            ))),
            "spec_scope" => Some(shell_quote(crate::domain::spec_type::scope_str(
                ctx.spec_scope,
            ))),
            "spec_commit_message" => Some(shell_quote(ctx.spec_commit_message.unwrap_or(""))),
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
            "spec_code" => Some(ctx.spec_code.unwrap_or("").to_string()),
            "spec_type" => Some(crate::domain::spec_type::type_str(ctx.spec_type).to_string()),
            "spec_scope" => Some(
                ctx.spec_scope
                    .filter(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string(),
            ),
            "spec_commit_message" => Some(ctx.spec_commit_message.unwrap_or("").to_string()),
            "blocker" => Some(ctx.blocker.unwrap_or("(none)").to_string()),
            "node" => Some(ctx.node_name.unwrap_or("(none)").to_string()),
            _ => None,
        },
    ))
}
