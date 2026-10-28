//! `graph_control`'s design-reviewer helpers (CB25), split out of
//! `handler_preflight.rs` to keep every production file within the size gate.
//! Moved verbatim; behaviour unchanged.

use std::sync::Arc;

use crate::daemon::handler::TaskTriggerHandler;

/// (CB25) Build the design-review prompt for a `graph_control` reviewer.
/// The full audit checklist lives in the `graph-reviewer` skill — never
/// duplicated here. The fallback checklist below is used ONLY when the
/// skill cannot be resolved, and names the areas, not the checks.
pub(crate) fn build_preflight_reviewer_prompt(
    graph_id: &str,
    queue_id: Option<&str>,
    skill_section: &str,
) -> String {
    let mut prompt = String::from(
        "You are auditing a canopy graph's DESIGN, not its reachability. Audit ONLY: do not modify the graph, the specs, or any file (no graph_* mutation tools, no file writes).",
    );
    prompt.push_str(&format!(" Target: graph_id='{graph_id}'"));
    if let Some(qid) = queue_id {
        prompt.push_str(&format!(" queue_id='{qid}'."));
    } else {
        prompt.push('.');
    }
    prompt.push_str(
        " Read the graph with graph get (and `queue list` for the queue) through the MCP surface.",
    );
    prompt.push_str(
        " Load the 'graph-reviewer' skill with skill_get; if the skill text is appended below under '## Skill: graph-reviewer', that IS its content — do not fetch again.",
    );
    prompt.push_str(skill_section);
    prompt.push_str(
        " Fallback areas (only if the skill is missing): graph (single entry, no unreachable node, pass+fail on every agent node, paired ops balanced); specs (decided, one repo each, seven sections, one language, no unsourced factual claims); model economy (expensive model placement, cost of one rejection, resume, commit rights, context groups, pinned skills); prompts (finish the turn, continue from work on disk).",
    );
    prompt.push_str(
        " Order findings by severity, name the SINGLE item to fix first, and close with an advisory verdict. Your verdict does not block the run — the caller decides.",
    );
    prompt
}

/// (CB25) Resolve the `graph-reviewer` skill for the preflight reviewer
/// prompt. Success inlines the instructions under the pinned-skill
/// delimiter; anything else degrades to the unresolved note and the
/// reviewer still spawns. Blocking (git/network), so the caller runs it
/// via `spawn_blocking`.
pub(crate) fn resolve_reviewer_skill_section(store: &crate::dynamic_skills::SkillStore) -> String {
    match store.get("graph-reviewer") {
        Ok(content) => format!("\n\n## Skill: graph-reviewer\n{}", content.instructions),
        Err(_) => "\n\n## Skill: graph-reviewer\n_Could not resolve pinned skill 'graph-reviewer' — continuing without it._"
            .to_string(),
    }
}

/// (CB25) Spawn the `graph_control` design reviewer. NEVER returns Err:
/// every failure degrades to a `not_spawned` note so the reviewer can
/// never fail the preflight itself.
pub(crate) async fn spawn_preflight_reviewer(
    handler: &TaskTriggerHandler,
    graph_id: &str,
    queue_id: Option<&str>,
    workdir: &str,
    platform: &str,
    model: Option<&str>,
) -> serde_json::Value {
    let pair = format!("{platform}/{}", model.unwrap_or("default"));
    // `Cli::strategy()` panics for a platform missing from the canopy
    // config (and `find_platform` errors for names outside the registry),
    // so pre-validate here: an unconfigured reviewer platform degrades to
    // `not_spawned` instead of panicking the preflight itself. Mirrors
    // `Cli::strategy`'s home resolution (`CANOPY_HOME_OVERRIDE` first).
    let home = std::env::var_os("CANOPY_HOME_OVERRIDE")
        .map(std::path::PathBuf::from)
        .or_else(dirs::home_dir);
    let configured =
        home.map(|h| crate::domain::canopy_config::CanopyConfig::load(&h.join(".canopy")));
    if configured
        .as_ref()
        .and_then(|c| c.get_cli(platform))
        .is_none()
    {
        return serde_json::json!({
            "status": "not_spawned",
            "platform": platform,
            "model": model,
            "pair": pair,
            "note": format!("Reviewer platform '{platform}' is not configured in canopy; probes above are unaffected."),
        });
    }
    let store = Arc::clone(&handler.dynamic_skills);
    let skill_section =
        match tokio::task::spawn_blocking(move || resolve_reviewer_skill_section(&store)).await {
            Ok(section) => section,
            Err(_) => resolve_reviewer_skill_section(&handler.dynamic_skills),
        };
    let prompt = build_preflight_reviewer_prompt(graph_id, queue_id, &skill_section);
    // Run the spawn in its own task so a panic inside `spawn_subagent`
    // (e.g. `fetch_registry`'s `reqwest::blocking` client, which cannot
    // drop its runtime in an async context under `debug_assertions`)
    // is captured as a `JoinError` and degrades to `not_spawned`
    // instead of taking down the preflight. Owned clones keep the
    // future `'static`; the monitor task it leaves behind still joins
    // the shared runtime, so a successful spawn is unaffected.
    let db = Arc::clone(&handler.db);
    let platform_owned = platform.to_string();
    let model_owned = model.map(str::to_string);
    let workdir_owned = workdir.to_string();
    let spawned = tokio::spawn(async move {
        crate::daemon::subagent::spawn_subagent(
            &db,
            &platform_owned,
            &prompt,
            model_owned.as_deref(),
            &workdir_owned,
            &["canopy".to_string()],
            15,
            60,
            None,
        )
        .await
    })
    .await;
    match spawned {
        Ok(Ok((id, _))) => serde_json::json!({
            "status": "running",
            "id": id,
            "platform": platform,
            "model": model,
            "pair": pair,
            "collect_with": "subagent_collect",
            "note": format!("Cost: ran on '{pair}'. Advisory only — does not affect probe results."),
        }),
        Ok(Err(e)) => serde_json::json!({
            "status": "not_spawned",
            "platform": platform,
            "model": model,
            "pair": pair,
            "note": format!("Reviewer could not start ({e}); probes above are unaffected."),
        }),
        Err(join_err) => serde_json::json!({
            "status": "not_spawned",
            "platform": platform,
            "model": model,
            "pair": pair,
            "note": format!("Reviewer could not start ({join_err}); probes above are unaffected."),
        }),
    }
}
