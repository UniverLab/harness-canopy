use anyhow::Result;
use chrono::Utc;

use super::format::{
    commit_rights_note, current_running_run, distinct_spec_ids_in_order, format_dt, format_elapsed,
    format_relative_duration, interrupted_note, no_verdict_note, run_status_icon, spec_status_icon,
    status_icon,
};
use super::resolve::resolve_graph;
use crate::daemon::handler::{active_graph_spec_context, ActiveGraphSpec};
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphNodeRun, GraphSpec, GraphStatus};

/// One spec row for `graph info`'s Specs section. Callers guarantee `spec`
/// is never the blank-name placeholder — it is hidden, not relabelled.
pub(super) fn print_graph_info_spec_line(spec: &GraphSpec) {
    let admin_tag = if spec.completed_via.as_deref() == Some("admin") {
        " (admin)"
    } else {
        ""
    };
    println!(
        " {} {}{}",
        spec_status_icon(spec.status),
        spec.name,
        admin_tag
    );
}

/// Header for the run-history reconstruction of a graph with nothing bound
/// and nothing in flight (e.g. a drained queue-driven graph). Names the
/// queue when the graph still links to one, so the reader knows where the
/// sequence came from.
pub(super) fn print_graph_info_queue_history_header(db: &Database, lp: &Graph) -> Result<()> {
    if let Some(queue_id) = lp.active_run_queue_id.as_deref() {
        if let Some(queue) = db.get_queue(queue_id)? {
            println!(
                " (queue-driven: \"{}\" — showing specs worked so far, not the full queue)",
                queue.name
            );
            return Ok(());
        }
    }
    println!(" (queue-driven — showing specs worked so far, not the full queue)");
    Ok(())
}

pub(super) fn handle_graph_info(db: &Database, id_or_name: &str) -> Result<()> {
    // Resolves by id/name (not a browsing list), so an archived graph must
    // still be found here.
    let graphs = db.list_graphs(None, true)?;
    let lp = resolve_graph(&graphs, id_or_name)?;

    print_graph_info_header(lp);
    print_graph_info_legacy_completion_hook(lp);
    print_graph_info_event_hooks(lp);

    let all_runs = db.list_graph_runs_for_graph(&lp.id)?;
    let hook_runs = db.list_graph_completion_hook_runs(&lp.id)?;

    print_graph_info_specs(db, lp, &all_runs)?;
    print_graph_info_current_node(db, lp, &all_runs)?;
    print_graph_info_recent_runs(db, lp, &all_runs)?;
    print_graph_info_hook_runs(&hook_runs);
    println!();
    Ok(())
}

/// Section: identity, status, workdir, trigger (schedule/watch) and autorun.
pub(super) fn print_graph_info_header(lp: &Graph) {
    println!("\n\x1b[1m── Graph: {} ──\x1b[0m", lp.name);
    println!(" id:      {}", lp.id);
    println!(
        " status:  {} {}",
        status_icon(lp.status),
        lp.status.as_str()
    );
    println!(" workdir: {}", lp.workdir);
    print!(" trigger: {}", lp.trigger_type_label());
    if let Some(expr) = lp.schedule_expr() {
        print!(" ({expr})");
    }
    if let Some(path) = lp.watch_path() {
        print!(" ({path})");
    }
    println!();
    if let Some(at) = lp.autorun_at {
        println!(
            " autorun: {} ({})",
            at.to_rfc3339(),
            format_relative_duration(at - Utc::now())
        );
    }
}

/// Section: the legacy singular `on_completed:` line. Event-keyed hooks are
/// the authoritative detail, but the legacy display stays for old users.
pub(super) fn print_graph_info_legacy_completion_hook(lp: &Graph) {
    if let Some(hooks) = lp
        .hooks
        .get(&crate::domain::graphs::GraphHookEvent::OnCompleted)
    {
        if let Some(hook) = hooks.first() {
            if hook.is_command() {
                println!(
                    " on_completed: command: {}",
                    hook.command.as_deref().unwrap_or("")
                );
            } else {
                println!(
                    " on_completed: {} ({})",
                    hook.platform.as_deref().unwrap_or(""),
                    hook.model.as_deref().unwrap_or("default model")
                );
            }
        }
    }
}

/// Section: the full event-keyed hook listing — every event's hooks, so
/// non-completion events are visible. The legacy line above already showed
/// the first `on_completed` hook, so that one is skipped here.
pub(super) fn print_graph_info_event_hooks(lp: &Graph) {
    for (event, hooks) in &lp.hooks {
        if *event == crate::domain::graphs::GraphHookEvent::OnCompleted {
            for (idx, hook) in hooks.iter().enumerate().skip(1) {
                if hook.is_command() {
                    println!(
                        " {}[{}]: command: {}",
                        event.as_str(),
                        idx,
                        hook.command.as_deref().unwrap_or("")
                    );
                } else {
                    println!(
                        " {}[{}]: {} ({})",
                        event.as_str(),
                        idx,
                        hook.platform.as_deref().unwrap_or(""),
                        hook.model.as_deref().unwrap_or("default model")
                    );
                }
            }
        } else {
            for (idx, hook) in hooks.iter().enumerate() {
                if hook.is_command() {
                    println!(
                        " {}[{}]: command: {}",
                        event.as_str(),
                        idx,
                        hook.command.as_deref().unwrap_or("")
                    );
                } else {
                    println!(
                        " {}[{}]: {} ({})",
                        event.as_str(),
                        idx,
                        hook.platform.as_deref().unwrap_or(""),
                        hook.model.as_deref().unwrap_or("default model")
                    );
                }
            }
        }
    }
}

/// Section: which spec the graph is on. CB30: one helper decides what the
/// graph is on — the raw bound list reads as a nameless placeholder for
/// queue/idea runs.
pub(super) fn print_graph_info_specs(
    db: &Database,
    lp: &Graph,
    all_runs: &[GraphNodeRun],
) -> Result<()> {
    println!("\n\x1b[1m── Specs ──────────────────────────────────────────────────────\x1b[0m");
    match active_graph_spec_context(db, lp)? {
        ActiveGraphSpec::Bound(_) | ActiveGraphSpec::None => {
            print_graph_info_bound_specs(db, lp, all_runs)?;
        }
        ActiveGraphSpec::QueueMember {
            spec, queue_name, ..
        } => {
            print_graph_info_queue_member_specs(db, &spec, &queue_name, all_runs)?;
        }
        ActiveGraphSpec::Idea { .. } => {
            println!(" (idea-driven — no bound specs)");
        }
    }
    Ok(())
}

/// The Specs section for a graph with nothing in flight: the bound specs
/// when it has any, otherwise the queue history reconstructed from
/// `graph_runs`, otherwise a bare "nothing queued" line.
pub(super) fn print_graph_info_bound_specs(
    db: &Database,
    lp: &Graph,
    all_runs: &[GraphNodeRun],
) -> Result<()> {
    let visible: Vec<GraphSpec> = db
        .list_graph_specs(&lp.id)?
        .into_iter()
        .filter(|spec| !spec.name.trim().is_empty())
        .collect();
    if !visible.is_empty() {
        for spec in &visible {
            print_graph_info_spec_line(spec);
        }
        return Ok(());
    }
    if all_runs.is_empty() {
        println!(" (no specs queued)");
        return Ok(());
    }
    // Nothing bound and nothing in flight — e.g. a drained queue-driven
    // graph. Reconstruct what ran so far from `graph_runs`, which always
    // records the real `graph_id` regardless of queue membership.
    // Placeholders are engine bookkeeping, never work: skip them.
    print_graph_info_queue_history_header(db, lp)?;
    for spec_id in distinct_spec_ids_in_order(all_runs) {
        let Some(spec) = db.get_graph_spec(spec_id)? else {
            continue;
        };
        if spec.name.trim().is_empty() {
            continue;
        }
        print_graph_info_spec_line(&spec);
    }
    Ok(())
}

/// The Specs section while a queue member is in flight: name the current
/// spec first, then every prior spec of the run in order (skipping the
/// current one and any placeholder).
pub(super) fn print_graph_info_queue_member_specs(
    db: &Database,
    spec: &GraphSpec,
    queue_name: &str,
    all_runs: &[GraphNodeRun],
) -> Result<()> {
    println!(" (queue-driven: \"{queue_name}\" — showing current spec)");
    print_graph_info_spec_line(spec);
    for spec_id in distinct_spec_ids_in_order(all_runs) {
        if *spec_id == spec.id {
            continue;
        }
        let Some(prior) = db.get_graph_spec(spec_id)? else {
            continue;
        };
        if prior.name.trim().is_empty() {
            continue;
        }
        print_graph_info_spec_line(&prior);
    }
    Ok(())
}

/// Section: the run the engine is on right now.
///
/// A `running`-status run row can outlive its graph (e.g. a run left over
/// from before `reconcile_orphaned_graphs` existed to clean these up), so
/// only trust it as "current" while the graph itself is actually running —
/// otherwise a completed/failed graph could misreport an old node as still
/// in flight.
pub(super) fn print_graph_info_current_node(
    db: &Database,
    lp: &Graph,
    all_runs: &[GraphNodeRun],
) -> Result<()> {
    if lp.status != GraphStatus::Running {
        return Ok(());
    }
    let Some(run) = current_running_run(all_runs) else {
        return Ok(());
    };
    let node = db.get_graph_node(&run.node_id)?;
    let node_name = node.as_ref().map_or(run.node_id.as_str(), |n| &n.name);
    let node_kind = node.as_ref().map_or("?", |n| n.kind.display_str());
    let elapsed = format_elapsed(Utc::now() - run.started_at);
    println!("\n\x1b[1m── Current Node ───────────────────────────────────────────────\x1b[0m");
    println!(
        " {} ({})  running {}  iteration {}",
        node_name, node_kind, elapsed, run.iteration
    );
    // Surfaces the exact baseline `{{spec_start_head}}` resolved to
    // for this spec's current attempt (B10) — the one number every
    // "why did this check pass/fail" debugging session needs.
    if let Some(spec) = db.get_graph_spec(&run.spec_id)? {
        match spec.spec_start_head {
            Some(head) => println!(" spec_start_head: {head}"),
            None => println!(" spec_start_head: (not a git workdir)"),
        }
        // C15: the HEAD this attempt's own committer last left
        // behind, if any — `{{spec_committed_head}}` in a check node.
        match spec.spec_committed_head {
            Some(head) => println!(" spec_committed_head: {head}"),
            None => println!(" spec_committed_head: (nothing committed by this run yet)"),
        }
    }
    Ok(())
}

/// Section: the five most recent node runs, newest first.
pub(super) fn print_graph_info_recent_runs(
    db: &Database,
    lp: &Graph,
    all_runs: &[GraphNodeRun],
) -> Result<()> {
    println!("\n\x1b[1m── Recent Node Runs ───────────────────────────────────────────\x1b[0m");
    if all_runs.is_empty() {
        println!(" (no runs yet)");
    }
    for run in all_runs.iter().rev().take(5) {
        let node_name = db
            .get_graph_node(&run.node_id)?
            .map_or_else(|| run.node_id.clone(), |n| n.name);
        let sid = run
            .session_id
            .as_deref()
            .map(|sid| format!("  sid {}", sid.chars().take(8).collect::<String>()))
            .unwrap_or_default();
        // RS3: flag a run that continued a context group's warm session (its
        // session id was first captured by a grouped sibling on this node).
        let group_note = match (lp.active_run_queue_id.as_deref(), run.session_id.as_deref()) {
            (Some(queue_id), Some(session_id)) => db
                .group_resume_source(queue_id, &run.spec_id, &run.node_id, session_id)?
                .map(|group| format!("  \x1b[36m(resumed group {group})\x1b[0m"))
                .unwrap_or_default(),
            _ => String::new(),
        };
        // B37: a node that moved git HEAD without `commit_rights` was failed
        // by the engine, not by its own verdict — say so, otherwise the run
        // reads as an ordinary fail and the operator hunts the wrong cause.
        let commit_note = commit_rights_note(run.output.as_ref());
        // B36: a run the daemon marked `fail` only because it was cut short
        // by a restart/kill must never read as "the node failed" — flag it
        // with its own icon and note, including how to recover any
        // quarantined worktree changes.
        let interrupted_note = interrupted_note(run.output.as_ref());
        let no_verdict = no_verdict_note(run.output.as_ref());
        println!(
            " {} {}  {}{}{}{}{}{}",
            run_status_icon(run.status, run.output.as_ref()),
            node_name,
            format_dt(run.started_at),
            sid,
            group_note,
            commit_note,
            interrupted_note,
            no_verdict
        );
    }
    Ok(())
}

/// Section: the five most recent hook firings, newest first, only when the
/// graph has fired at least one.
pub(super) fn print_graph_info_hook_runs(
    hook_runs: &[crate::domain::graphs::GraphCompletionHookRun],
) {
    if hook_runs.is_empty() {
        return;
    }
    println!("\n\x1b[1m── Hook Runs ────────────────────────────────────────────────\x1b[0m");
    for run in hook_runs.iter().rev().take(5) {
        println!(
            " {} {}[{}] {}",
            run_status_icon(run.status, run.output.as_ref()),
            run.event.as_str(),
            run.hook_index,
            format_dt(run.started_at)
        );
    }
}
