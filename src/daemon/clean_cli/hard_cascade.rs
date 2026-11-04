use std::path::Path;

use anyhow::Result;

use crate::db::Database;
use crate::domain::clean::{self, HardCascadeCandidate, HardCascadePlan};

/// `--hard` mode: gather the per-project cascade facts, build the
/// [`HardCascadePlan`], print it, and (unless `dry_run`) prompt and
/// execute. Splits the orphan work from `run_clean` so the soft-mode
/// tests don't pay for the extra DB roundtrips when `--hard` isn't set.
/// Returns the number of database rows the cascade removed (real run) or
/// would remove (`--dry-run`'s projection), so the caller can fold it into
/// the total that decides whether reclaiming space is warranted.
/// Pure fact-gathering half of `--hard`'s cascade: builds the
/// [`HardCascadePlan`] without printing or executing anything. Split out so
/// the reclaim window can project the cascade's row count (to decide
/// whether reclaiming is warranted) without the double-printed plan a
/// second `run_hard_cascade` call would otherwise produce.
pub(super) fn build_hard_cascade_plan(db: &Database) -> Result<HardCascadePlan> {
    let candidates: Vec<HardCascadeCandidate> = db
        .list_projects()?
        .into_iter()
        .map(|p| {
            let workdir_exists = Path::new(&p.path).exists();
            let counts = db.project_hard_cascade_counts(&p.hash, &p.path)?;
            let skip_reason = db.project_hard_cascade_skip_reason(&p.path)?;
            Ok::<_, anyhow::Error>(HardCascadeCandidate {
                hash: p.hash,
                name: p.name,
                path: p.path,
                workdir_exists,
                counts,
                skip_reason,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(clean::plan_hard_cascade(&candidates))
}

/// Projected row count (direct + cascade) the plan's targets would remove,
/// for the reclaim-warranted projection — never executes anything.
pub(super) fn projected_hard_cascade_rows(plan: &HardCascadePlan) -> u64 {
    plan.targets
        .iter()
        .map(|t| (direct_count(&t.counts) + cascade_count(&t.counts)) as u64)
        .sum()
}

pub(super) fn run_hard_cascade(db: &Database, dry_run: bool, yes: bool) -> Result<u64> {
    let plan = build_hard_cascade_plan(db)?;
    print_hard_cascade_plan(&plan, dry_run);

    if plan.targets.is_empty() {
        return Ok(0);
    }

    if dry_run {
        // Plan-only: no prompt, no deletes (spec: "deletes nothing, no
        // confirmation needed"). Project the row count so `--dry-run`
        // reports the same reclaim figures a real run would.
        return Ok(projected_hard_cascade_rows(&plan));
    }

    if !yes {
        // Interactive confirmation; refuse on no/eof/non-tty.
        let proceed = match prompt_hard_cascade_confirmation(&plan) {
            Ok(value) => value,
            Err(err) => {
                eprintln!("  {err}\n  Aborting --hard: refusing to run without an explicit yes.");
                return Ok(0);
            }
        };
        if !proceed {
            println!("  Aborted by user — nothing deleted.");
            return Ok(0);
        }
    }

    let mut rows_deleted = 0u64;
    for target in &plan.targets {
        match db.cascade_delete_orphan_project(&target.hash, &target.missing_path) {
            Ok(actual) => {
                rows_deleted += (direct_count(&actual) + cascade_count(&actual)) as u64;
                println!(
                    " \x1b[32m✓\x1b[0m  Removed {} ({}): {} direct + {} cascade rows across the project.",
                    target.name,
                    target.hash,
                    direct_count(&actual),
                    cascade_count(&actual),
                );
            }
            Err(err) => {
                eprintln!(
                    " \x1b[31m✗\x1b[0m  Failed to remove {} ({}): {err}",
                    target.name, target.hash
                );
            }
        }
    }
    Ok(rows_deleted)
}

fn direct_count(c: &clean::HardCascadeCounts) -> i64 {
    c.graphs
        + c.interactive_sessions
        + c.terminal_sessions
        + c.last_prompts
        + c.scheduled_prompts
        + c.failed_scheduled_prompts
        + c.sync_messages
        + c.sync_locks
        + c.intelligence_nodes
        + c.operational_sessions
}

fn cascade_count(c: &clean::HardCascadeCounts) -> i64 {
    c.graph_specs
        + c.graph_nodes
        + c.graph_edges
        + c.graph_runs
        + c.graph_completion_hook_runs
        + c.ensembles
        + c.ensemble_members
        + c.queue_members
        + c.seed_sessions
        + c.intelligence_edges
}

fn prompt_hard_cascade_confirmation(plan: &HardCascadePlan) -> Result<bool> {
    use inquire::Confirm;
    // Default to no so a stray Enter (or a non-tty env) can't accidentally
    // confirm a destructive cascade. Scripts that want unattended deletes
    // must pass `--yes`.
    let prompt = format!(
        "Delete these {} orphaned project(s) and every row that references them?",
        plan.targets.len()
    );
    Confirm::new(&prompt)
        .with_default(false)
        .with_help_message("y: delete, n/Esc: abort")
        .prompt()
        .map_err(|err| anyhow::anyhow!("{err}"))
}

fn print_hard_cascade_plan(plan: &HardCascadePlan, dry_run: bool) {
    if plan.is_empty() && plan.skips.is_empty() {
        println!("\nNo orphaned projects to clean.");
        return;
    }
    let verb = if dry_run {
        "Would remove"
    } else {
        "Will remove"
    };
    if !plan.targets.is_empty() {
        println!("\n\x1b[1m── canopy clean --hard (orphan-project cascade) ──\x1b[0m");
        println!(" {verb} {} orphaned project(s):", plan.targets.len());
        for t in &plan.targets {
            let c = &t.counts;
            println!(
                "   {} ({})  missing: {}\n     [{} graph(s), {} interactive session(s), {} terminal session(s),\n      {} last prompt(s), {} scheduled send(s), {} failed send(s),\n      {} sync message(s), {} sync lock(s), {} intelligence node(s), {} operational session(s)]\n     + cascade: [{} graph_spec(s), {} graph_node(s), {} graph_edge(s),\n                 {} graph_control(s), {} completion_hook_run(s),\n                 {} ensemble(s), {} ensemble_member(s), {} queue_member(s),\n                 {} seed_session(s), {} intelligence_edge(s)]",
                t.name,
                t.hash,
                t.missing_path,
                c.graphs,
                c.interactive_sessions,
                c.terminal_sessions,
                c.last_prompts,
                c.scheduled_prompts,
                c.failed_scheduled_prompts,
                c.sync_messages,
                c.sync_locks,
                c.intelligence_nodes,
                c.operational_sessions,
                c.graph_specs,
                c.graph_nodes,
                c.graph_edges,
                c.graph_runs,
                c.graph_completion_hook_runs,
                c.ensembles,
                c.ensemble_members,
                c.queue_members,
                c.seed_sessions,
                c.intelligence_edges,
            );
        }
    }
    if !plan.skips.is_empty() {
        println!(
            "\n\x1b[33m⚠\x1b[0m  Skipped {} project(s) with in-flight state:",
            plan.skips.len()
        );
        for s in &plan.skips {
            println!(
                "   {} ({})  missing: {}  — {}",
                s.name,
                s.hash,
                s.missing_path,
                s.reason.describe()
            );
        }
    }
}
