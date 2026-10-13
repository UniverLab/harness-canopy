//! CLI handler for `canopy clean` (soft cleanup, C1) and
//! `canopy clean --hard` (orphan-project cascade, C2).
//!
//! Gathers facts from the DB and filesystem, hands them to the pure
//! `domain::clean` decision functions to build a [`CleanPlan`], then either
//! prints it (`--dry-run`) or executes it and prints what happened. Soft
//! mode never touches `active`/`resumed` sessions, never deletes projects,
//! and only reports orphaned projects (missing workdir). `--hard` runs the
//! full soft cleanup first, then prints the orphan-project cascade plan
//! and prompts before deleting each orphan (and every row that references
//! it) unless `--yes` or `--dry-run` is set.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::daemon::process;
use crate::db::Database;
use crate::domain::canopy_config::CanopyConfig;
use crate::domain::clean::{self, CleanPlan, ProjectCandidate, SandboxCandidate};
use crate::domain::db_paths::database_path;
use crate::domain::graphs::GraphStatus;

mod hard_cascade;
mod reclaim;
mod scans;

use hard_cascade::{build_hard_cascade_plan, projected_hard_cascade_rows, run_hard_cascade};
use reclaim::{
    confirm_stop_daemon, print_dry_run_reclaim_window, reclaim_if_warranted,
    run_reclaim_window_and_report, ReclaimOptions,
};
use scans::{format_bytes, scan_log_files, scan_rag_residue, scan_terminal_dirs};

#[cfg(test)]
use reclaim::{
    restoration_feasible_from_facts, run_reclaim_window, DaemonOps, DaemonOwnerKind,
    ReclaimWindowOutcome,
};
#[cfg(test)]
use std::sync::Mutex;

pub async fn handle_clean_action(
    dry_run: bool,
    older_than: Option<u64>,
    hard: bool,
    yes: bool,
    no_reclaim: bool,
    stop_daemon: bool,
) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db_path = database_path(&data_dir);
    let db = Database::new_safe(&db_path, &data_dir)?;
    let config = CanopyConfig::load(&data_dir);
    let retention_days = older_than.unwrap_or(config.clean.retention_days);
    let graph_event_days = older_than.unwrap_or(config.graph_event_retention_days);
    let now_ts = chrono::Utc::now().timestamp();

    // Project (read-only, no execution) what a real run would delete, so we
    // only ever consider the stop-daemon reclaim window when it's actually
    // warranted — stopping the daemon to reclaim a handful of rows would
    // just be downtime for nothing.
    let projected_plan =
        build_clean_plan(&data_dir, &db, retention_days, graph_event_days, now_ts)?;
    let mut projected_rows = projected_plan.deleted_row_count() as u64;
    if hard {
        projected_rows += projected_hard_cascade_rows(&build_hard_cascade_plan(&db)?);
    }
    let reclaim_warranted = !no_reclaim && clean::should_reclaim(projected_rows as usize);

    if !reclaim_warranted {
        return run_clean_without_window(
            &db,
            &data_dir,
            &db_path,
            dry_run,
            hard,
            yes,
            retention_days,
            graph_event_days,
            now_ts,
            no_reclaim,
        );
    }

    if dry_run {
        print_summary(&projected_plan, retention_days, true);
        let mut rows_deleted = projected_plan.deleted_row_count() as u64;
        if hard {
            rows_deleted += run_hard_cascade(&db, true, yes)?;
        }
        print_dry_run_reclaim_window(rows_deleted, &db_path);
        return Ok(());
    }

    let daemon_pid = process::read_pid(&data_dir).filter(|&p| process::is_process_running(p));

    if daemon_pid.is_some() {
        let consent = stop_daemon || confirm_stop_daemon()?;
        if !consent {
            return run_clean_without_window(
                &db,
                &data_dir,
                &db_path,
                false,
                hard,
                yes,
                retention_days,
                graph_event_days,
                now_ts,
                no_reclaim,
            );
        }
    }

    run_reclaim_window_and_report(
        &data_dir,
        &db_path,
        ReclaimOptions {
            retention_days,
            graph_event_retention_days: graph_event_days,
            now_ts,
            hard,
            yes,
        },
        daemon_pid,
    )
    .await
}

/// The pre-B2 flow: build (and unless `dry_run`, execute) the plan, run
/// `--hard` if requested, then let [`reclaim_if_warranted`] decide whether a
/// same-process reclaim is possible (only when the daemon happens to already
/// be down) — used whenever the stop-daemon reclaim window isn't in play:
/// reclaiming wasn't warranted, or it was but the operator didn't consent to
/// stopping the daemon.
#[allow(clippy::too_many_arguments)]
fn run_clean_without_window(
    db: &Database,
    data_dir: &Path,
    db_path: &Path,
    dry_run: bool,
    hard: bool,
    yes: bool,
    retention_days: u64,
    graph_event_days: u64,
    now_ts: i64,
    no_reclaim: bool,
) -> Result<()> {
    let plan = run_clean(
        data_dir,
        db,
        dry_run,
        retention_days,
        graph_event_days,
        now_ts,
    )?;
    print_summary(&plan, retention_days, dry_run);

    let mut rows_deleted = plan.deleted_row_count() as u64;
    if hard {
        rows_deleted += run_hard_cascade(db, dry_run, yes)?;
    }

    reclaim_if_warranted(db, data_dir, db_path, dry_run, no_reclaim, rows_deleted);

    Ok(())
}

/// Gather inputs and build the plan, and (unless `dry_run`) execute it.
/// Takes `data_dir`/`db`/`now_ts` as parameters (rather than resolving them
/// itself) so tests can point it at a scratch directory and an injected
/// clock instead of the real `~/.canopy`.
fn run_clean(
    data_dir: &Path,
    db: &Database,
    dry_run: bool,
    retention_days: u64,
    graph_event_days: u64,
    now_ts: i64,
) -> Result<CleanPlan> {
    let plan = build_clean_plan(data_dir, db, retention_days, graph_event_days, now_ts)?;
    if !dry_run {
        execute_plan(db, &plan)?;
    }
    Ok(plan)
}

/// Pure planning half of [`run_clean`] — gathers facts and builds the
/// [`CleanPlan`] without executing it. Split out so the reclaim window can
/// project the row count a real run would delete (to decide whether
/// reclaiming is even warranted) without a redundant `execute_plan` call,
/// and so it can later execute that same, already-built plan once inside
/// the window instead of re-scanning.
fn build_clean_plan(
    data_dir: &Path,
    db: &Database,
    retention_days: u64,
    graph_event_days: u64,
    now_ts: i64,
) -> Result<CleanPlan> {
    let cutoff_ts = clean::cutoff_timestamp(now_ts, retention_days);

    let sessions = db.list_cleanable_interactive_sessions()?;
    let session_ids = clean::plan_session_cleanup(&sessions, cutoff_ts);

    let agent_ids = db.list_agent_ids()?;
    let log_files = scan_log_files(data_dir)?;
    let orphan_logs = clean::plan_orphan_file_cleanup(&log_files, &agent_ids, cutoff_ts);

    let terminal_names = db.list_terminal_session_names()?;
    let terminal_dirs = scan_terminal_dirs(data_dir)?;
    let orphan_terminals =
        clean::plan_orphan_file_cleanup(&terminal_dirs, &terminal_names, cutoff_ts);

    let rag_files = scan_rag_residue(data_dir)?;
    let rag_residue = clean::plan_rag_residue_cleanup(&rag_files, cutoff_ts);

    let mut project_candidates = Vec::new();
    for p in db.list_projects()? {
        let workdir_exists = Path::new(&p.path).exists();
        let dependents = db.project_dependent_counts(&p.path)?;
        project_candidates.push(ProjectCandidate {
            hash: p.hash,
            name: p.name,
            path: p.path,
            workdir_exists,
            dependents,
        });
    }
    let orphaned_projects = clean::plan_orphaned_projects(&project_candidates);

    // CB42: bulk sandbox path — req 3's rule with force=false. Every
    // finished sandbox row becomes a candidate; `run_over` is true when the
    // owning graph is completed/failed (or its row is gone — nothing is left
    // that could still need the sandbox) or the sandbox row itself is no
    // longer active. `plan_sandbox_cleanup` then keeps only provably-empty
    // branches; uncertain ones stay.
    let mut sandbox_candidates = Vec::new();
    for row in db.list_finished_sandbox_runs().unwrap_or_default() {
        let graph_over = match db.get_graph(&row.owner_id) {
            Ok(Some(lp)) => matches!(lp.status, GraphStatus::Completed | GraphStatus::Failed),
            Ok(None) | Err(_) => true,
        };
        sandbox_candidates.push(SandboxCandidate {
            id: row.id.clone(),
            path: std::path::PathBuf::from(&row.worktree_path),
            graph_id: row.owner_id.clone(),
            branch: row.sandbox_branch.clone(),
            run_over: graph_over || row.status != "active",
            has_unique_commits: crate::domain::sandbox::has_unique_commits(&row),
        });
    }
    let sandbox_removals = clean::plan_sandbox_cleanup(&sandbox_candidates);
    // CB92: derive the worktrees root from the injected `data_dir` (equal to
    // `canopy_dir()` in production) — never the real home — and split the
    // untracked leaves into proven leaks (dangling gitdir + older than 2 h,
    // deleted) and the rest (still reported, never touched).
    let worktrees_base = data_dir.join("worktrees");
    let untracked_paths = super::sandbox_cli::scan_untracked_sandbox_dirs_in(db, &worktrees_base);
    let leak_facts = scans::scan_untracked_leak_facts(&untracked_paths);
    let untracked_leaks = clean::plan_leaked_untracked(&leak_facts, now_ts);
    let leaked_set: std::collections::HashSet<&PathBuf> = untracked_leaks.iter().collect();
    let sandbox_untracked: Vec<PathBuf> = untracked_paths
        .into_iter()
        .filter(|p| !leaked_set.contains(p))
        .collect();

    // CM46: aged-out durable graph events, for graphs that are not running.
    let graph_event_cutoff = clean::cutoff_timestamp(now_ts, graph_event_days);
    let graph_event_cutoff = chrono::DateTime::from_timestamp(graph_event_cutoff, 0)
        .map(|at| at.to_rfc3339())
        .unwrap_or_default();
    let running_graph_ids = db.list_running_graph_ids()?;
    let graph_event_deletions =
        db.count_pruneable_graph_events(&graph_event_cutoff, &running_graph_ids)?;

    Ok(CleanPlan {
        session_ids,
        log_files: orphan_logs,
        terminal_dirs: orphan_terminals,
        rag_residue_files: rag_residue,
        orphaned_projects,
        sandbox_removals,
        sandbox_untracked,
        untracked_leaks,
        graph_event_deletions,
        graph_event_cutoff,
    })
}

fn execute_plan(db: &Database, plan: &CleanPlan) -> Result<()> {
    if !plan.session_ids.is_empty() {
        db.delete_interactive_sessions(&plan.session_ids)?;
    }
    if plan.graph_event_deletions > 0 {
        let running_graph_ids = db.list_running_graph_ids()?;
        db.prune_graph_events(&plan.graph_event_cutoff, &running_graph_ids)?;
    }
    for f in &plan.log_files {
        let _ = std::fs::remove_file(&f.path);
    }
    for d in &plan.terminal_dirs {
        let _ = std::fs::remove_file(d.path.join("history.toml"));
        let _ = std::fs::remove_dir(&d.path);
    }
    for f in &plan.rag_residue_files {
        let _ = std::fs::remove_file(&f.path);
    }
    // CB42 bulk path: each candidate is named BEFORE removal (req 7), and a
    // failure never aborts the rest of clean. Always force=false in bulk —
    // unique work is never deleted here.
    for target in &plan.sandbox_removals {
        println!(
            "Will remove sandbox {} ({}, branch {})",
            target.id,
            target.path.display(),
            target.branch
        );
        let row = match db.get_sandbox_run(&target.id) {
            Ok(Some(row)) => row,
            Ok(None) => {
                eprintln!(" Failed to remove sandbox {}: run row is gone", target.id);
                continue;
            }
            Err(e) => {
                eprintln!(" Failed to remove sandbox {}: {e:#}", target.id);
                continue;
            }
        };
        match crate::domain::sandbox::discard_sandbox_blocking(&row, false) {
            Ok(crate::domain::sandbox::DiscardOutcome::Discarded) => {
                let _ = db.update_sandbox_run_status(&row.id, "discarded");
            }
            Ok(crate::domain::sandbox::DiscardOutcome::RefusedUniqueCommits) => {
                eprintln!(
                    " Skipped sandbox {}: branch holds commits that exist nowhere else",
                    target.id
                );
            }
            Err(e) => {
                eprintln!(" Failed to remove sandbox {}: {e:#}", target.id);
            }
        }
    }
    // CB92: delete only the leaf of each proven leak — the parent `<hash>/`
    // dir is deliberately left alone (pruning it would race with a
    // concurrent run creating the same project dir).
    for p in &plan.untracked_leaks {
        println!("Will remove leaked untracked sandbox dir {}", p.display());
        if let Err(e) = std::fs::remove_dir_all(p) {
            eprintln!(" Failed to remove leaked dir {}: {e:#}", p.display());
        }
    }
    Ok(())
}

fn print_summary(plan: &CleanPlan, retention_days: u64, dry_run: bool) {
    let verb = if dry_run { "Would remove" } else { "Removed" };
    println!(
        "\n\x1b[1m── canopy clean (retention: {retention_days}d{}) ──\x1b[0m",
        if dry_run { ", dry run" } else { "" }
    );
    println!(
        " {verb} {} stale interactive session(s) (database rows)",
        plan.session_ids.len()
    );
    if plan.graph_event_deletions > 0 {
        println!(
            " {verb} {} graph event(s) (database rows)",
            plan.graph_event_deletions
        );
    }
    println!(" {verb} {} orphaned log file(s)", plan.log_files.len());
    println!(
        " {verb} {} orphaned terminal history dir(s)",
        plan.terminal_dirs.len()
    );
    println!(
        " {verb} {} leftover RAG residue file(s)",
        plan.rag_residue_files.len()
    );
    println!(
        " {verb} {} sandbox worktree(s)",
        plan.sandbox_removals.len()
    );
    for t in &plan.sandbox_removals {
        println!("   {} ({}, branch {})", t.id, t.path.display(), t.branch);
    }
    println!(
        " {verb} {} leaked untracked sandbox dir(s) (dangling gitdir, older than 2 h)",
        plan.untracked_leaks.len()
    );
    if !plan.sandbox_untracked.is_empty() {
        println!(
            " Skipped {} untracked sandbox dir(s) (no sandbox run record; left untouched):",
            plan.sandbox_untracked.len()
        );
        for p in &plan.sandbox_untracked {
            println!("   {}", p.display());
        }
    }
    // Deliberately two separate lines: a row count is not a byte count, and
    // the row deletions above contribute nothing to this figure — it's
    // filesystem bytes from the log/terminal/RAG files only. Freed database
    // space is reported (if warranted) by `reclaim_if_warranted` below.
    println!(
        " Filesystem bytes {}: {}",
        if dry_run { "would be freed" } else { "freed" },
        format_bytes(plan.reclaimed_bytes())
    );

    if !plan.orphaned_projects.is_empty() {
        println!(
            "\n\x1b[33m⚠\x1b[0m  {} orphaned project(s) — workdir missing, reported only:",
            plan.orphaned_projects.len()
        );
        for p in &plan.orphaned_projects {
            println!(
                "   {} ({})  missing: {}  [{} graph(s), {} interactive session(s), {} terminal session(s)]",
                p.name,
                p.hash,
                p.missing_path,
                p.dependents.graphs,
                p.dependents.interactive_sessions,
                p.dependents.terminal_sessions
            );
            println!(
                "     Hint: if this directory was renamed or moved, `canopy project remap {} <new-path>` \
                 keeps its history instead of deleting it.",
                p.hash
            );
        }
        println!(
            "   Hint: `canopy clean --hard` removes orphaned projects (and their dependents) with confirmation — \
             only if the directory is truly gone, not just moved."
        );
    }

    if plan.is_empty() && plan.orphaned_projects.is_empty() {
        println!("\nNothing to clean.");
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::GraphStatus;
    use tempfile::tempdir;

    fn test_db(dir: &Path) -> Database {
        Database::new(&dir.join("test.db")).unwrap()
    }

    fn touch(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn dry_run_reports_but_deletes_nothing() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();

        touch(&data_dir.join("logs/agent-gone.log"), "log contents");

        // Push the clock far enough forward that everything looks old
        // without needing to backdate real file mtimes.
        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        let plan = run_clean(data_dir, &db, true, 7, 90, now_ts).unwrap();

        assert_eq!(plan.session_ids, vec!["s-old".to_string()]);
        assert_eq!(plan.log_files.len(), 1);

        // Nothing was actually touched.
        assert_eq!(db.count_interactive_sessions().unwrap(), 1);
        assert!(data_dir.join("logs/agent-gone.log").exists());
    }

    #[test]
    fn real_run_deletes_orphaned_session_and_log_file() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();

        touch(&data_dir.join("logs/agent-gone.log"), "log contents");

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        let plan = run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert_eq!(plan.session_ids.len(), 1);
        assert_eq!(db.count_interactive_sessions().unwrap(), 0);
        assert!(!data_dir.join("logs/agent-gone.log").exists());
    }

    #[test]
    fn active_session_survives_a_real_run_even_when_ancient() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        db.insert_interactive_session(
            "s-active",
            "s-active",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let now_ts = chrono::Utc::now().timestamp() + 3650 * 86_400;
        run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert_eq!(
            db.get_interactive_session_status("s-active").unwrap(),
            Some("active".to_string())
        );
    }

    #[test]
    fn known_agent_log_file_is_never_deleted() {
        use crate::application::ports::AgentRepository;

        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        db.upsert_agent(&crate::domain::models::Agent {
            id: "agent-keep".to_string(),
            prompt: "do stuff".to_string(),
            trigger: None,
            cli: crate::domain::models::Cli::new("opencode"),
            model: None,
            effort: None,
            working_dir: None,
            enabled: true,
            enable_at: None,
            created_at: chrono::Utc::now(),
            log_path: data_dir
                .join("logs/agent-keep.log")
                .to_string_lossy()
                .to_string(),
            timeout_minutes: 15,
            expires_at: None,
            last_run_at: None,
            last_run_ok: None,
            last_triggered_at: None,
            trigger_count: 0,
        })
        .unwrap();

        touch(&data_dir.join("logs/agent-keep.log"), "keep me");

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert!(data_dir.join("logs/agent-keep.log").exists());
    }

    #[test]
    fn orphaned_terminal_history_dir_detected_and_removed() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        touch(
            &data_dir.join("terminals/stray-term/history.toml"),
            "commands = []",
        );

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        let plan = run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert_eq!(plan.terminal_dirs.len(), 1);
        assert!(!data_dir.join("terminals/stray-term").exists());
    }

    #[test]
    fn known_terminal_session_dir_survives() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        db.insert_terminal_session("t1", "kept-term", "bash", "/tmp")
            .unwrap();
        touch(
            &data_dir.join("terminals/kept-term/history.toml"),
            "commands = []",
        );

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert!(data_dir.join("terminals/kept-term").exists());
    }

    #[test]
    fn orphaned_project_with_missing_workdir_is_reported_not_deleted() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        let existing_workdir = dir.path().join("still-here");
        std::fs::create_dir_all(&existing_workdir).unwrap();

        db.upsert_project(&crate::domain::project::Project {
            hash: "hash-exists".to_string(),
            path: existing_workdir.to_string_lossy().to_string(),
            name: "exists".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        })
        .unwrap();
        db.upsert_project(&crate::domain::project::Project {
            hash: "hash-missing".to_string(),
            path: "/definitely/does/not/exist/anywhere".to_string(),
            name: "missing".to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: chrono::Utc::now().timestamp(),
        })
        .unwrap();

        let now_ts = chrono::Utc::now().timestamp();
        let plan = run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert_eq!(plan.orphaned_projects.len(), 1);
        assert_eq!(plan.orphaned_projects[0].hash, "hash-missing");
        // Soft mode never deletes the project row itself.
        assert_eq!(db.list_projects().unwrap().len(), 2);
    }

    #[test]
    fn rag_residue_tmp_file_is_removed_and_lancedb_dir_untouched() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        touch(&data_dir.join("rag/leftover.tmp"), "partial ingest");
        touch(
            &data_dir.join("rag/vectors.lancedb/manifest.json"),
            "not a residue file",
        );

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        let plan = run_clean(data_dir, &db, false, 7, 90, now_ts).unwrap();

        assert_eq!(plan.rag_residue_files.len(), 1);
        assert!(!data_dir.join("rag/leftover.tmp").exists());
        assert!(data_dir.join("rag/vectors.lancedb/manifest.json").exists());
    }

    #[test]
    fn format_bytes_renders_human_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
    }

    fn make_project(hash: &str, path: &str) -> crate::domain::project::Project {
        crate::domain::project::Project {
            hash: hash.to_string(),
            path: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            description: None,
            tags: None,
            indexed_at: None,
            created_at: 1_700_000_000,
        }
    }

    fn make_graph(
        id: &str,
        workdir: &str,
        status: crate::domain::graphs::GraphStatus,
    ) -> crate::domain::graphs::Graph {
        crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: format!("graph-{id}"),
            description: None,
            workdir: workdir.to_string(),
            status,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn hard_cascade_with_yes_deletes_orphan_and_its_dependents() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);
        let workdir = "/definitely/does/not/exist";
        let hash = "hash-orphan";
        db.upsert_project(&make_project(hash, workdir)).unwrap();
        db.insert_graph(&make_graph("graph-1", workdir, GraphStatus::Completed))
            .unwrap();
        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            workdir,
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();
        db.insert_terminal_session("t-1", "t-1", "bash", workdir)
            .unwrap();

        // --hard --yes: confirm the cascade executes without prompting.
        run_hard_cascade(&db, false, true).unwrap();

        assert!(db.get_project(hash).unwrap().is_none());
        assert_eq!(db.project_dependent_counts(workdir).unwrap().graphs, 0);
        assert_eq!(
            db.project_dependent_counts(workdir)
                .unwrap()
                .interactive_sessions,
            0
        );
        assert_eq!(
            db.project_dependent_counts(workdir)
                .unwrap()
                .terminal_sessions,
            0
        );
    }

    #[test]
    fn hard_cascade_dry_run_deletes_nothing() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);
        let workdir = "/definitely/does/not/exist";
        let hash = "hash-orphan-dry";
        db.upsert_project(&make_project(hash, workdir)).unwrap();
        db.insert_graph(&make_graph("graph-1", workdir, GraphStatus::Completed))
            .unwrap();
        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            workdir,
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();

        // --hard --dry-run: no prompt, no deletes, plan still printed.
        run_hard_cascade(&db, true, false).unwrap();

        assert!(db.get_project(hash).unwrap().is_some());
        assert_eq!(db.project_dependent_counts(workdir).unwrap().graphs, 1);
        assert_eq!(
            db.project_dependent_counts(workdir)
                .unwrap()
                .interactive_sessions,
            1
        );
    }

    #[test]
    fn hard_cascade_keeps_project_with_existing_workdir_intact() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);
        let workdir = dir.path().join("real-workdir");
        std::fs::create_dir_all(&workdir).unwrap();
        let workdir_str = workdir.to_string_lossy().to_string();
        let hash = "hash-keep";
        db.upsert_project(&make_project(hash, &workdir_str))
            .unwrap();
        db.insert_graph(&make_graph("graph-1", &workdir_str, GraphStatus::Completed))
            .unwrap();
        db.insert_interactive_session(
            "s-1",
            "s-1",
            "opencode",
            &workdir_str,
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-1", 0).unwrap();

        run_hard_cascade(&db, false, true).unwrap();

        // Project with an existing workdir is NEVER a target of --hard.
        assert!(db.get_project(hash).unwrap().is_some());
        assert_eq!(db.project_dependent_counts(&workdir_str).unwrap().graphs, 1);
    }

    #[test]
    fn hard_cascade_skips_orphan_with_running_graph() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);
        let workdir = "/orphan/with/running/graph";
        let hash = "hash-running";
        db.upsert_project(&make_project(hash, workdir)).unwrap();
        db.insert_graph(&make_graph("graph-r", workdir, GraphStatus::Running))
            .unwrap();

        run_hard_cascade(&db, false, true).unwrap();

        // Project survives because its graph is still running.
        assert!(db.get_project(hash).unwrap().is_some());
        assert!(db.get_graph("graph-r").unwrap().is_some());
    }

    #[test]
    fn hard_cascade_skips_orphan_with_active_session() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);
        let workdir = "/orphan/with/active/session";
        let hash = "hash-active";
        db.upsert_project(&make_project(hash, workdir)).unwrap();
        db.insert_interactive_session(
            "s-live",
            "s-live",
            "opencode",
            workdir,
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        run_hard_cascade(&db, false, true).unwrap();

        assert!(db.get_project(hash).unwrap().is_some());
        assert_eq!(
            db.project_dependent_counts(workdir)
                .unwrap()
                .interactive_sessions,
            1
        );
    }

    #[test]
    fn hard_cascade_processes_targets_and_skips_in_one_call() {
        // Mixed: one deletable orphan, one skipped (running graph), one with
        // a real workdir. --hard should delete only the first.
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        let real_workdir = dir.path().join("real");
        std::fs::create_dir_all(&real_workdir).unwrap();
        let real_str = real_workdir.to_string_lossy().to_string();

        db.upsert_project(&make_project("hash-real", &real_str))
            .unwrap();
        db.upsert_project(&make_project("hash-doomed", "/orphan/doomed"))
            .unwrap();
        db.upsert_project(&make_project("hash-skipped", "/orphan/skipped"))
            .unwrap();

        db.insert_graph(&make_graph("graph-real", &real_str, GraphStatus::Completed))
            .unwrap();
        db.insert_graph(&make_graph(
            "graph-doomed",
            "/orphan/doomed",
            GraphStatus::Completed,
        ))
        .unwrap();
        db.insert_graph(&make_graph(
            "graph-skipped",
            "/orphan/skipped",
            GraphStatus::Running,
        ))
        .unwrap();

        run_hard_cascade(&db, false, true).unwrap();

        assert!(db.get_project("hash-real").unwrap().is_some());
        assert!(db.get_project("hash-doomed").unwrap().is_none());
        assert!(db.get_project("hash-skipped").unwrap().is_some());
    }

    // ── reclaim_if_warranted ────────────────────────────────────────────

    /// Combined on-disk footprint (main file + WAL) so shrinkage is
    /// detectable regardless of whether data happened to already be
    /// checkpointed out of the WAL at the moment of measurement.
    fn total_db_size(db_path: &Path) -> u64 {
        let main = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
        let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.to_string_lossy()));
        let wal = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
        main + wal
    }

    /// Inserts `count` padded, immediately-deleted sessions so the database
    /// has freed-but-unreturned pages worth reclaiming.
    fn bulk_insert_and_delete_sessions(db: &Database, count: usize) {
        let padding = "x".repeat(4096);
        let mut ids = Vec::new();
        for i in 0..count {
            let id = format!("s-{i}");
            db.insert_interactive_session(
                &id,
                &id,
                "opencode",
                "/tmp",
                Some(&padding),
                None,
                "interactive",
                None,
            )
            .unwrap();
            db.finish_interactive_session(&id, 0).unwrap();
            ids.push(id);
        }
        db.delete_interactive_sessions(&ids).unwrap();
    }

    #[test]
    fn reclaim_shrinks_file_when_threshold_met_and_daemon_not_running() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        // Above RECLAIM_ROW_THRESHOLD (50).
        bulk_insert_and_delete_sessions(&db, 60);

        let size_before = total_db_size(&db_path);
        reclaim_if_warranted(&db, data_dir, &db_path, false, false, 60);
        let size_after = total_db_size(&db_path);

        assert!(
            size_after < size_before,
            "expected reclaim to shrink the file: {size_before} -> {size_after}"
        );
    }

    #[test]
    fn reclaim_skipped_when_deletion_is_trivial() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        bulk_insert_and_delete_sessions(&db, 5);

        let size_before = total_db_size(&db_path);
        // Below RECLAIM_ROW_THRESHOLD (50): must not touch the file.
        reclaim_if_warranted(&db, data_dir, &db_path, false, false, 5);
        let size_after = total_db_size(&db_path);

        assert_eq!(size_before, size_after);
    }

    #[test]
    fn reclaim_skipped_when_no_reclaim_flag_set() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        bulk_insert_and_delete_sessions(&db, 60);

        let size_before = total_db_size(&db_path);
        // Above threshold, but --no-reclaim opts out.
        reclaim_if_warranted(&db, data_dir, &db_path, false, true, 60);
        let size_after = total_db_size(&db_path);

        assert_eq!(size_before, size_after);
    }

    #[test]
    fn reclaim_skipped_and_untouched_under_dry_run() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        bulk_insert_and_delete_sessions(&db, 60);

        let size_before = total_db_size(&db_path);
        // Above threshold, but --dry-run must project only, never touch.
        reclaim_if_warranted(&db, data_dir, &db_path, true, false, 60);
        let size_after = total_db_size(&db_path);

        assert_eq!(size_before, size_after);
    }

    #[test]
    fn reclaim_skipped_while_daemon_is_running() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        bulk_insert_and_delete_sessions(&db, 60);

        // Current test process is guaranteed alive, so `daemon.pid`
        // naming it makes `is_process_running` report true, exactly as it
        // would for a live `canopy serve`.
        std::fs::write(data_dir.join("daemon.pid"), std::process::id().to_string()).unwrap();

        let size_before = total_db_size(&db_path);
        reclaim_if_warranted(&db, data_dir, &db_path, false, false, 60);
        let size_after = total_db_size(&db_path);

        assert_eq!(
            size_before, size_after,
            "must not VACUUM while the daemon holds a write connection"
        );
    }

    // ── B2: the stop-daemon reclaim window ─────────────────────────────

    /// A PID that is never going to equal a real `systemd`/`launchd`-managed
    /// canopy PID on any machine this test runs on, so `detect_daemon_owner`
    /// deterministically resolves to `Detached` here — including on a dev
    /// box (like this one) that has a real `canopy.service` installed and
    /// running.
    const FAKE_DAEMON_PID: u32 = u32::MAX;

    struct FakeDaemonOps {
        calls: std::cell::RefCell<Vec<&'static str>>,
        stop_ok: bool,
        restart_ok: bool,
        stop_panics: bool,
    }

    impl FakeDaemonOps {
        fn new() -> Self {
            Self {
                calls: std::cell::RefCell::new(Vec::new()),
                stop_ok: true,
                restart_ok: true,
                stop_panics: false,
            }
        }
    }

    impl DaemonOps for FakeDaemonOps {
        fn stop(&self, _owner: DaemonOwnerKind) -> Result<()> {
            self.calls.borrow_mut().push("stop");
            if self.stop_panics {
                panic!("simulated stop failure");
            }
            if self.stop_ok {
                Ok(())
            } else {
                anyhow::bail!("simulated stop failure")
            }
        }

        fn restart(&self, _owner: DaemonOwnerKind) -> Result<()> {
            self.calls.borrow_mut().push("restart");
            if self.restart_ok {
                Ok(())
            } else {
                anyhow::bail!("simulated restart failure")
            }
        }
    }

    #[test]
    fn reclaim_window_refuses_when_a_graph_is_running() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();
        db.insert_graph(&make_graph("graph-1", "/tmp", GraphStatus::Running))
            .unwrap();

        let ops = FakeDaemonOps::new();
        let restored = Mutex::new(false);
        let outcome = run_reclaim_window(
            &db,
            data_dir,
            &db_path,
            ReclaimOptions {
                retention_days: 7,
                graph_event_retention_days: 90,
                now_ts: chrono::Utc::now().timestamp(),
                hard: false,
                yes: true,
            },
            Some(FAKE_DAEMON_PID),
            &ops,
            &restored,
        )
        .unwrap();

        match outcome {
            ReclaimWindowOutcome::Refused(reason) => {
                assert!(reason.contains("running"), "{reason}");
            }
            _ => panic!("expected Refused"),
        }
        assert!(
            ops.calls.borrow().is_empty(),
            "must not touch the daemon while busy"
        );
    }

    #[test]
    fn reclaim_window_refuses_when_a_tui_session_is_attached() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();
        db.insert_interactive_session(
            "s-live",
            "s-live",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();

        let ops = FakeDaemonOps::new();
        let restored = Mutex::new(false);
        let outcome = run_reclaim_window(
            &db,
            data_dir,
            &db_path,
            ReclaimOptions {
                retention_days: 7,
                graph_event_retention_days: 90,
                now_ts: chrono::Utc::now().timestamp(),
                hard: false,
                yes: true,
            },
            Some(FAKE_DAEMON_PID),
            &ops,
            &restored,
        )
        .unwrap();

        match outcome {
            ReclaimWindowOutcome::Refused(reason) => {
                assert!(reason.contains("TUI"), "{reason}");
            }
            _ => panic!("expected Refused"),
        }
        assert!(ops.calls.borrow().is_empty());
    }

    #[test]
    fn reclaim_window_happy_path_stops_cleans_vacuums_and_restarts() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();
        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();

        let ops = FakeDaemonOps::new();
        let restored = Mutex::new(false);
        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        let outcome = run_reclaim_window(
            &db,
            data_dir,
            &db_path,
            ReclaimOptions {
                retention_days: 7,
                graph_event_retention_days: 90,
                now_ts,
                hard: false,
                yes: true,
            },
            Some(FAKE_DAEMON_PID),
            &ops,
            &restored,
        )
        .unwrap();

        match outcome {
            ReclaimWindowOutcome::Completed(report) => {
                assert!(report.daemon_was_running);
                assert_eq!(report.daemon_owner, Some(DaemonOwnerKind::Detached));
                assert_eq!(report.quick_check, "ok");
                assert_eq!(report.plan.session_ids, vec!["s-old".to_string()]);
                assert!(report.daemon_restored);
            }
            _ => panic!("expected Completed"),
        }
        assert_eq!(db.count_interactive_sessions().unwrap(), 0);
        assert_eq!(*ops.calls.borrow(), vec!["stop", "restart"]);
        assert!(*restored.lock().unwrap());
    }

    #[test]
    fn reclaim_window_skips_stop_and_restart_when_daemon_was_not_running() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        let ops = FakeDaemonOps::new();
        let restored = Mutex::new(false);
        let outcome = run_reclaim_window(
            &db,
            data_dir,
            &db_path,
            ReclaimOptions {
                retention_days: 7,
                graph_event_retention_days: 90,
                now_ts: chrono::Utc::now().timestamp(),
                hard: false,
                yes: true,
            },
            None,
            &ops,
            &restored,
        )
        .unwrap();

        match outcome {
            ReclaimWindowOutcome::Completed(report) => {
                assert!(!report.daemon_was_running);
                assert_eq!(report.daemon_owner, None);
                assert!(report.daemon_restored);
            }
            _ => panic!("expected Completed"),
        }
        assert!(
            ops.calls.borrow().is_empty(),
            "nothing to stop or restart when the daemon wasn't running"
        );
    }

    #[test]
    fn reclaim_window_aborts_on_quick_check_failure_and_still_restores() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        {
            let db = Database::new(&db_path).unwrap();
            db.insert_interactive_session(
                "s-old",
                "s-old",
                "opencode",
                "/tmp",
                None,
                None,
                "interactive",
                None,
            )
            .unwrap();
            db.finish_interactive_session("s-old", 0).unwrap();
        }
        // Same corruption technique as `db::clean`'s `quick_check` test:
        // stomp on page data after every handle is dropped so the file has
        // a valid SQLite header but fails `quick_check`.
        let mut bytes = std::fs::read(&db_path).unwrap();
        // CM5: a fixed offset in page 3 rather than bytes.len()/2 — the
        // subagent_runs table shifted the file so the midpoint no longer
        // lands in a page quick_check validates.
        let start = 8192;
        let end = (start + 100).min(bytes.len());
        for b in &mut bytes[start..end] {
            *b ^= 0xFF;
        }
        std::fs::write(&db_path, &bytes).unwrap();

        let db = Database::new(&db_path).unwrap();
        let ops = FakeDaemonOps::new();
        let restored = Mutex::new(false);
        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        let outcome = run_reclaim_window(
            &db,
            data_dir,
            &db_path,
            ReclaimOptions {
                retention_days: 7,
                graph_event_retention_days: 90,
                now_ts,
                hard: false,
                yes: true,
            },
            Some(FAKE_DAEMON_PID),
            &ops,
            &restored,
        )
        .unwrap();

        match outcome {
            ReclaimWindowOutcome::QuickCheckFailed {
                verdict,
                daemon_restored,
            } => {
                assert_ne!(verdict, "ok");
                assert!(daemon_restored);
            }
            _ => panic!("expected QuickCheckFailed"),
        }
        // Nothing was modified: the existing cleanup never ran.
        assert_eq!(db.count_interactive_sessions().unwrap(), 1);
        assert_eq!(*ops.calls.borrow(), vec!["stop", "restart"]);
    }

    #[test]
    fn reclaim_window_restores_the_daemon_even_when_stop_panics() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        let ops = FakeDaemonOps {
            stop_panics: true,
            ..FakeDaemonOps::new()
        };
        let restored = Mutex::new(false);
        let now_ts = chrono::Utc::now().timestamp();

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_reclaim_window(
                &db,
                data_dir,
                &db_path,
                ReclaimOptions {
                    retention_days: 7,
                    graph_event_retention_days: 90,
                    now_ts,
                    hard: false,
                    yes: true,
                },
                Some(FAKE_DAEMON_PID),
                &ops,
                &restored,
            )
        }));

        assert!(
            result.is_err(),
            "expected the simulated stop failure to panic"
        );
        assert!(
            *restored.lock().unwrap(),
            "the panic-safety guard must still restore the daemon"
        );
        assert_eq!(*ops.calls.borrow(), vec!["stop", "restart"]);
    }

    #[test]
    fn reclaim_window_reports_restore_failure_without_losing_the_quick_check_verdict() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let mut bytes;
        {
            let db = Database::new(&db_path).unwrap();
            db.insert_terminal_session("t1", "t1", "bash", "/tmp")
                .unwrap();
            drop(db);
            bytes = std::fs::read(&db_path).unwrap();
        }
        // CM5: a fixed offset in page 3 rather than bytes.len()/2 — the
        // subagent_runs table shifted the file so the midpoint no longer
        // lands in a page quick_check validates.
        let start = 8192;
        let end = (start + 100).min(bytes.len());
        for b in &mut bytes[start..end] {
            *b ^= 0xFF;
        }
        std::fs::write(&db_path, &bytes).unwrap();

        let db = Database::new(&db_path).unwrap();
        let ops = FakeDaemonOps {
            restart_ok: false,
            ..FakeDaemonOps::new()
        };
        let restored = Mutex::new(false);
        let outcome = run_reclaim_window(
            &db,
            data_dir,
            &db_path,
            ReclaimOptions {
                retention_days: 7,
                graph_event_retention_days: 90,
                now_ts: chrono::Utc::now().timestamp(),
                hard: false,
                yes: true,
            },
            Some(FAKE_DAEMON_PID),
            &ops,
            &restored,
        )
        .unwrap();

        match outcome {
            ReclaimWindowOutcome::QuickCheckFailed {
                daemon_restored, ..
            } => {
                assert!(
                    !daemon_restored,
                    "a failed restart must be reported, not papered over"
                );
            }
            _ => panic!("expected QuickCheckFailed"),
        }
        assert!(
            !*restored.lock().unwrap(),
            "a failed restart leaves the shared flag false so a later attempt can retry"
        );
    }

    // ── restoration_feasible_from_facts (B-decision 5) ──────────────────
    //
    // Fed booleans directly rather than exercised through the real
    // `systemctl`/unit-file checks: those are absent on some CI containers
    // and present (and enabled) on this very dev box, so a test asserting a
    // specific refusal must not depend on which machine it runs on.

    #[test]
    fn restoration_feasible_ok_for_a_detached_owner() {
        assert!(
            restoration_feasible_from_facts(DaemonOwnerKind::Detached, false, false, false).is_ok()
        );
    }

    #[test]
    fn restoration_feasible_refuses_when_the_unit_is_missing() {
        let err =
            restoration_feasible_from_facts(DaemonOwnerKind::Managed("systemd"), false, true, true)
                .unwrap_err();
        assert!(err.contains("not installed"), "{err}");
    }

    #[test]
    fn restoration_feasible_refuses_when_the_systemd_session_is_unavailable() {
        let err =
            restoration_feasible_from_facts(DaemonOwnerKind::Managed("systemd"), true, false, true)
                .unwrap_err();
        assert!(err.contains("unavailable"), "{err}");
    }

    #[test]
    fn restoration_feasible_refuses_when_the_unit_is_disabled() {
        let err =
            restoration_feasible_from_facts(DaemonOwnerKind::Managed("systemd"), true, true, false)
                .unwrap_err();
        assert!(err.contains("disabled"), "{err}");
    }

    #[test]
    fn restoration_feasible_ok_when_the_unit_is_installed_available_and_enabled() {
        assert!(restoration_feasible_from_facts(
            DaemonOwnerKind::Managed("systemd"),
            true,
            true,
            true
        )
        .is_ok());
    }

    // ── no-consent / dry-run: the legacy path stays intact ──────────────

    #[test]
    fn no_consent_path_still_cleans_but_leaves_reclaim_to_the_legacy_gate() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db_path = data_dir.join("test.db");
        let db = Database::new(&db_path).unwrap();

        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();
        // Same trick as `reclaim_skipped_while_daemon_is_running`: name the
        // live test process so the daemon reads as running.
        std::fs::write(data_dir.join("daemon.pid"), std::process::id().to_string()).unwrap();

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;
        run_clean_without_window(
            &db, data_dir, &db_path, false, false, true, 7, 90, now_ts, false,
        )
        .unwrap();

        // The existing cleanup still ran even though the (simulated) daemon
        // is up and no stop-daemon window was ever entered.
        assert_eq!(db.count_interactive_sessions().unwrap(), 0);
    }

    #[test]
    fn dry_run_projection_leaves_soft_and_hard_candidates_untouched() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        db.insert_interactive_session(
            "s-old",
            "s-old",
            "opencode",
            "/tmp",
            None,
            None,
            "interactive",
            None,
        )
        .unwrap();
        db.finish_interactive_session("s-old", 0).unwrap();

        let workdir = "/definitely/does/not/exist/for/projection";
        db.upsert_project(&make_project("hash-x", workdir)).unwrap();
        db.insert_graph(&make_graph("graph-x", workdir, GraphStatus::Completed))
            .unwrap();

        let now_ts = chrono::Utc::now().timestamp() + 30 * 86_400;

        let projected_plan = build_clean_plan(data_dir, &db, 7, 90, now_ts).unwrap();
        let projected_hard = projected_hard_cascade_rows(&build_hard_cascade_plan(&db).unwrap());
        assert_eq!(projected_plan.session_ids.len(), 1);
        assert!(projected_hard > 0);

        // A dry run of the soft plan must not touch anything, regardless of
        // whether the projected total would warrant a reclaim window.
        let dry_plan = run_clean(data_dir, &db, true, 7, 90, now_ts).unwrap();
        assert_eq!(dry_plan.session_ids, projected_plan.session_ids);
        assert_eq!(db.count_interactive_sessions().unwrap(), 1);
        assert!(
            db.get_project("hash-x").unwrap().is_some(),
            "projecting the hard cascade's row count must never execute it"
        );
    }

    // ── CB42: sandbox bulk path ────────────────────────────────────────

    fn init_git_repo(path: &Path) {
        let run = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(path)
                .env("GIT_AUTHOR_NAME", "Test")
                .env("GIT_AUTHOR_EMAIL", "test@test.com")
                .env("GIT_COMMITTER_NAME", "Test")
                .env("GIT_COMMITTER_EMAIL", "test@test.com")
                .output()
                .expect("git command");
            assert!(
                output.status.success(),
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init", "-b", "main"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(path.join("README.md"), "# Test\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);
    }

    fn git_branch_exists(repo: &Path, branch: &str) -> bool {
        let output = std::process::Command::new("git")
            .args(["branch", "--list", branch])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout).contains(branch)
    }

    #[test]
    fn sandbox_dry_run_names_before_removing() {
        let repo_dir = tempdir().unwrap();
        init_git_repo(repo_dir.path());
        // A sandbox branch with no unique commits (identical to base).
        std::process::Command::new("git")
            .args(["branch", "canopy/sandbox-cleantest"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();

        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        // No graph row for this owner: a missing graph means the run is over.
        let sandbox = crate::domain::sandbox::Sandbox {
            id: "sandbox-clean-fixture".to_string(),
            project_hash: "cleanhash".to_string(),
            base_branch: "main".to_string(),
            sandbox_branch: "canopy/sandbox-cleantest".to_string(),
            worktree_path: data_dir.join("wt"),
            cli_name: "opencode".to_string(),
            original_workdir: repo_dir.path().to_string_lossy().to_string(),
            created_at: chrono::Utc::now(),
        };
        db.insert_sandbox_run(&sandbox, "graph", "graph-gone")
            .unwrap();
        db.update_sandbox_run_status(&sandbox.id, "kept").unwrap();

        let now_ts = chrono::Utc::now().timestamp();
        let plan = build_clean_plan(data_dir, &db, 7, 90, now_ts).unwrap();
        // The finished, provably-empty sandbox is named as a removal target.
        assert!(
            plan.sandbox_removals
                .iter()
                .any(|t| t.id == sandbox.id && t.branch == "canopy/sandbox-cleantest"),
            "expected sandbox-clean-fixture in removals, got {:?}",
            plan.sandbox_removals
        );

        // A dry run removes nothing: branch, worktree path state, and row
        // are all exactly as before.
        let dry = run_clean(data_dir, &db, true, 7, 90, now_ts).unwrap();
        assert!(dry.sandbox_removals.iter().any(|t| t.id == sandbox.id));
        assert!(git_branch_exists(
            repo_dir.path(),
            "canopy/sandbox-cleantest"
        ));
        assert_eq!(
            db.get_sandbox_run(&sandbox.id).unwrap().unwrap().status,
            "kept"
        );

        std::process::Command::new("git")
            .args(["branch", "-D", "canopy/sandbox-cleantest"])
            .current_dir(repo_dir.path())
            .output()
            .unwrap();
    }

    // ── CB92: leaked untracked dirs ────────────────────────────────────

    #[test]
    fn clean_removes_dangling_old_untracked_and_keeps_live() {
        let dir = tempdir().unwrap();
        let data_dir = dir.path();
        let db = test_db(data_dir);

        // A leaked leaf: `.git` points at a gitdir that no longer exists
        // (the shape the 693 real leaks are in — tempdir repo is gone).
        let leak_path = data_dir.join("worktrees/hash-a/leak-id");
        std::fs::create_dir_all(&leak_path).unwrap();
        std::fs::write(
            leak_path.join(".git"),
            format!(
                "gitdir: /definitely/does/not/exist-{}/x\n",
                std::process::id()
            ),
        )
        .unwrap();

        // A live leaf: `.git` points at a real dir that stays alive for the
        // whole test, so its gitdir exists.
        let live_gitdir = tempdir().unwrap();
        let live_path = data_dir.join("worktrees/hash-b/live-id");
        std::fs::create_dir_all(&live_path).unwrap();
        std::fs::write(
            live_path.join(".git"),
            format!("gitdir: {}\n", live_gitdir.path().display()),
        )
        .unwrap();

        // Empty test DB → no sandbox rows → both leaves are untracked.
        // Capture "now" AFTER creating the leaves so their mtimes are never
        // in the future relative to the injected clock.
        let now = chrono::Utc::now().timestamp();

        // Too fresh (age ≈ 0 < 2 h): the age gate keeps both, dry run only
        // projects that decision.
        let plan = run_clean(data_dir, &db, true, 7, 90, now).unwrap();
        assert!(
            plan.untracked_leaks.is_empty(),
            "fresh dirs must not be leaks: {:?}",
            plan.untracked_leaks
        );
        assert!(leak_path.exists() && live_path.exists());

        // 5 h later: the dangling leaf is a proven leak — deleted in a real
        // run; the live-gitdir leaf is still reported, never touched.
        let plan = run_clean(data_dir, &db, false, 7, 90, now + 5 * 3600).unwrap();
        assert_eq!(plan.untracked_leaks, vec![leak_path.clone()]);
        assert!(!leak_path.exists(), "dangling old leak must be removed");
        assert!(live_path.exists(), "live-gitdir dir must be kept");
        assert!(plan.sandbox_untracked.contains(&live_path));
    }
}
