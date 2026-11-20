//! Legacy agent prompt rendering for migrated backing graphs.
//!
//! A migrated background agent keeps its prompt verbatim on its backing
//! node's `prompt_template`, plus a `legacy_agent_id` marker in the node
//! config. At run time the node renders in two passes, in this order:
//!
//! 1. the legacy `substitute_variables` (`TIMESTAMP`/`TASK_ID`/`LOG_PATH`
//!    always; `FILE_PATH`/`EVENT_TYPE` only on a watch fire with context);
//! 2. the router render binding exactly three markers — `spec_name` to the
//!    agent id, `spec_content` to the (already substituted) agent prompt,
//!    `previous_output` to `"(none)"`.
//!
//! For a migrated watch agent on a non-manual fire, the deliver node
//! appends the identical block `render_send_prompt` appends today.

use super::directed::FireCtx;

/// Node-config key carrying the migrated agent's id. Its presence selects
/// the two-pass legacy render instead of the graph's normal render.
pub(crate) const LEGACY_AGENT_ID_KEY: &str = "legacy_agent_id";

/// Node-config key: the migrated agent was a watch agent, so a non-manual
/// fire appends the watch block.
pub(crate) const LEGACY_WATCH_APPEND_KEY: &str = "legacy_watch_append";

/// The only `{{marker}}` spellings a migrated agent prompt may contain:
/// the five legacy variables plus the three router bindings the second
/// pass binds. Anything else (including `{{previous_feedback}}`) fails the
/// agent's migration naming the marker.
pub(crate) const LEGACY_ALLOWED_MARKERS: &[&str] = &[
    "TIMESTAMP",
    "TASK_ID",
    "LOG_PATH",
    "FILE_PATH",
    "EVENT_TYPE",
    "spec_name",
    "spec_content",
    "previous_output",
];

/// The migrated agent id for a legacy backing node, if it is one.
pub(crate) fn legacy_agent_id(node: &crate::domain::graphs::GraphNode) -> Option<String> {
    node.config
        .get(LEGACY_AGENT_ID_KEY)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// Whether a legacy backing node appends the watch block on non-manual fires.
pub(crate) fn legacy_watch_append(node: &crate::domain::graphs::GraphNode) -> bool {
    node.config
        .get(LEGACY_WATCH_APPEND_KEY)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Every unescaped `{{marker}}` in `prompt`, first-seen order, deduplicated.
pub(crate) fn template_markers(prompt: &str) -> Vec<String> {
    crate::domain::prompts::placeholder_names(prompt)
}

/// The first marker in `prompt` outside [`LEGACY_ALLOWED_MARKERS`], if any.
pub(crate) fn first_disallowed_marker(prompt: &str) -> Option<String> {
    template_markers(prompt)
        .into_iter()
        .find(|marker| !LEGACY_ALLOWED_MARKERS.contains(&marker.as_str()))
}

/// Render a migrated agent's prompt: legacy substitution first, then the
/// router render with exactly the three legacy bindings. `watch_append`
/// adds the watch block on non-manual fires, byte-identical to the legacy
/// `render_send_prompt` block.
pub(crate) fn render_legacy_migrated_prompt(
    template: &str,
    agent_id: &str,
    log_path: &str,
    fire: &FireCtx,
    watch_append: bool,
) -> String {
    let pass1 = crate::scheduler::substitute_variables(
        template,
        agent_id,
        log_path,
        fire.file_path.as_deref(),
        fire.event_type.as_deref(),
    );
    let pass2 = crate::domain::prompts::render_template(&pass1, |raw| match raw {
        "previous_output" => Some("(none)".to_string()),
        "spec_content" => Some(pass1.clone()),
        "spec_name" => Some(agent_id.to_string()),
        _ => None,
    });
    if watch_append && !fire.is_manual() {
        format!(
            "{pass2}\n\n---\nTriggered by canopy watch agent '{agent_id}'.\nChanged path: {}\nEvent: {}",
            fire.file_path.as_deref().unwrap_or(""),
            fire.event_type.as_deref().unwrap_or(""),
        )
    } else {
        pass2
    }
}

/// Refuse a legacy template holding a marker the two passes cannot bind.
/// Legacy `{{FILE_PATH}}`/`{{EVENT_TYPE}}` may legitimately survive pass 1
/// (no fire context on cron/manual runs, exactly like the legacy path
/// leaving them literal), so the allowlist is the migration-time set, not
/// just the three second-pass bindings.
pub(crate) fn refuse_unbindable_legacy_template(
    node_name: &str,
    template: &str,
) -> Result<(), anyhow::Error> {
    super::prompts::refuse_unbindable_template(node_name, template, LEGACY_ALLOWED_MARKERS, None)
}

/// Render the cold-start prompt of a migrated headless agent node: the
/// node's verbatim prompt template through the two legacy passes. Fails
/// naming the marker when the template holds one the passes cannot bind
/// (migration validates this upfront; this is the runtime backstop).
pub(crate) fn render_legacy_agent_prompt(
    lp: &crate::domain::graphs::Graph,
    node: &crate::domain::graphs::GraphNode,
    agent_id: &str,
    template: &str,
    fire: &super::directed::FireCtx,
) -> Result<String, anyhow::Error> {
    refuse_unbindable_legacy_template(&node.name, template)?;
    Ok(render_legacy_migrated_prompt(
        template,
        agent_id,
        &lp.log_path(),
        fire,
        legacy_watch_append(node),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fire(trigger: bool, file: Option<&str>, event: Option<&str>) -> FireCtx {
        FireCtx {
            trigger: trigger.then_some(crate::graph_engine::graph_run_log::GraphRunTrigger::Watch),
            file_path: file.map(str::to_string),
            event_type: event.map(str::to_string),
        }
    }

    #[test]
    fn legacy_render_substitutes_task_and_log_markers() {
        let out = render_legacy_migrated_prompt(
            "run {{TASK_ID}} log {{LOG_PATH}}",
            "agent-1",
            "/tmp/agent-1.log",
            &fire(false, None, None),
            false,
        );
        assert_eq!(out, "run agent-1 log /tmp/agent-1.log");
    }

    #[test]
    fn legacy_render_binds_router_markers_to_agent() {
        let out = render_legacy_migrated_prompt(
            "name={{spec_name}} prev={{previous_output}}",
            "agent-9",
            "/tmp/x.log",
            &fire(false, None, None),
            false,
        );
        assert_eq!(out, "name=agent-9 prev=(none)");
    }

    #[test]
    fn legacy_render_rejects_previous_feedback_at_migration() {
        assert_eq!(
            first_disallowed_marker("do {{previous_feedback}} now"),
            Some("previous_feedback".to_string())
        );
        assert_eq!(
            first_disallowed_marker("run {{TASK_ID}} {{spec_content}}"),
            None
        );
    }

    #[test]
    fn legacy_render_appends_watch_block_on_watch_fire_only() {
        let template = "check {{FILE_PATH}} {{EVENT_TYPE}}";
        let watched = render_legacy_migrated_prompt(
            template,
            "w1",
            "/tmp/w1.log",
            &fire(true, Some("/repo/a.rs"), Some("modify")),
            true,
        );
        assert!(watched.contains("check /repo/a.rs modify"));
        assert!(watched.contains("Triggered by canopy watch agent 'w1'"));
        assert!(watched.contains("Changed path: /repo/a.rs"));
        assert!(watched.contains("Event: modify"));

        let manual = render_legacy_migrated_prompt(
            template,
            "w1",
            "/tmp/w1.log",
            &fire(false, None, None),
            true,
        );
        assert!(!manual.contains("Triggered by canopy"));
        assert!(manual.contains("{{FILE_PATH}}"));
    }
}
