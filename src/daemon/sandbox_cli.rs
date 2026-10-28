//! CLI handler for `canopy sandbox list|land|discard` — consolidation for
//! the worktrees sandboxed graph runs leave behind (CB42).
//!
//! - `list` is read-only: it never checks out, fetches, or modifies
//!   anything. It also shows untracked directories under canopy's worktrees
//!   root (no `sandbox_runs` row) as `untracked` so anonymous leftovers stay
//!   visible — and untouched.
//! - `land` merges one sandbox's branch onto the graph's base branch,
//!   refusing (with the conflicting paths) rather than forcing when the
//!   merge is not clean.
//! - `discard` removes one sandbox's worktree (via git's own worktree
//!   machinery) and deletes its branch, refusing unique commits unless the
//!   caller passes `--discard-unique-commits` in that same call.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use clap::Subcommand;

use crate::daemon::sandbox_ops;
use crate::db::Database;
use crate::domain::db_paths::database_path;
use crate::domain::sandbox::{self, SandboxInfo};

#[derive(Subcommand)]
pub(crate) enum SandboxAction {
    /// List every sandbox worktree canopy created: its path, the graph and
    /// run that made it, its branch, whether the branch holds commits that
    /// exist nowhere else, and how far behind the base ref it is.
    List {
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
    },
    /// Land one sandbox's work onto the graph's base branch. Refuses rather
    /// than forcing when the merge is not clean, and names the conflicting
    /// paths.
    Land {
        /// Full sandbox id or an unambiguous id prefix.
        sandbox_id: String,
    },
    /// Discard one sandbox: worktree removed, branch deleted. Refuses when
    /// the branch holds commits that exist nowhere else, unless
    /// `--discard-unique-commits` is passed in the same call.
    Discard {
        /// Full sandbox id or an unambiguous id prefix.
        sandbox_id: String,
        /// Explicit consent to delete commits that exist only in this
        /// sandbox. Without it, unique work is never deleted.
        #[arg(long = "discard-unique-commits")]
        discard_unique: bool,
    },
}

pub(crate) async fn handle_sandbox_action(action: SandboxAction) -> Result<()> {
    let data_dir = crate::ensure_data_dir()?;
    let db = Database::new_safe(&database_path(&data_dir), &data_dir)?;
    match action {
        SandboxAction::List { json } => handle_list(&db, json),
        SandboxAction::Land { sandbox_id } => handle_land(&db, &sandbox_id).await,
        SandboxAction::Discard {
            sandbox_id,
            discard_unique,
        } => handle_discard(&db, &sandbox_id, discard_unique).await,
    }
}

/// Directories under a worktrees root with no `sandbox_runs` row:
/// anonymous leftovers from before runs were recorded. Listed so they stay
/// findable — never touched here.
fn untracked_sandbox_dirs_in(known_ids: &HashSet<String>, base: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let top = entry.path();
        if !top.is_dir() {
            continue;
        }
        // Layout is `<project-hash>/<sandbox-id>`; a top-level dir with no
        // subdirectories (a stale empty project dir) is itself the leaf.
        let mut leaves: Vec<PathBuf> = std::fs::read_dir(&top)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        if leaves.is_empty() {
            leaves.push(top);
        }
        for leaf in leaves {
            let id = leaf
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            if !known_ids.contains(id) {
                out.push(leaf);
            }
        }
    }
    out.sort();
    out
}

fn untracked_sandbox_dirs(known_ids: &HashSet<String>) -> Vec<PathBuf> {
    untracked_sandbox_dirs_in(known_ids, &sandbox::worktrees_base_dir())
}

/// Shared by `sandbox list` and `canopy clean`: the untracked-directory
/// scan is one function so both commands agree on what "untracked" means.
/// The `_in` form takes the worktrees root explicitly so `canopy clean`
/// derives it from its injected `data_dir` instead of the real home.
pub(crate) fn scan_untracked_sandbox_dirs_in(db: &Database, base: &Path) -> Vec<PathBuf> {
    let known: HashSet<String> = db
        .list_all_sandbox_runs()
        .map(|rows| rows.into_iter().map(|r| r.id).collect())
        .unwrap_or_default();
    untracked_sandbox_dirs_in(&known, base)
}

fn untracked_info(path: &Path) -> SandboxInfo {
    let id = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    SandboxInfo {
        id,
        path: path.to_path_buf(),
        graph_id: String::new(),
        branch: "unknown".to_string(),
        base_branch: "unknown".to_string(),
        original_workdir: "unknown".to_string(),
        status: "untracked".to_string(),
        has_unique_commits: None,
        behind_count: None,
        worktree_registered: path.join(".git").exists(),
    }
}

fn handle_list(db: &Database, json: bool) -> Result<()> {
    let rows = db.list_all_sandbox_runs()?;
    let mut infos: Vec<SandboxInfo> = rows.iter().map(sandbox_ops::enrich).collect();
    let known: HashSet<String> = rows.into_iter().map(|r| r.id).collect();
    infos.extend(
        untracked_sandbox_dirs(&known)
            .iter()
            .map(|p| untracked_info(p)),
    );

    if json {
        println!("{}", serde_json::to_string_pretty(&infos)?);
        return Ok(());
    }

    println!(
        "{:<38} {:<52} {:<18} {:<26} {:<14} {:<8} STATUS",
        "ID", "PATH", "GRAPH/RUN", "BRANCH", "UNIQUE-COMMITS", "BEHIND"
    );
    for info in &infos {
        let unique = match info.has_unique_commits {
            Some(true) => "yes",
            Some(false) => "no",
            None => "unknown",
        };
        let behind = match info.behind_count {
            Some(n) => n.to_string(),
            None => "unknown".to_string(),
        };
        let owner = if info.graph_id.is_empty() {
            "—".to_string()
        } else {
            info.graph_id.clone()
        };
        println!(
            "{:<38} {:<52} {:<18} {:<26} {:<14} {:<8} {}",
            info.id,
            info.path.display(),
            owner,
            info.branch,
            unique,
            behind,
            info.status
        );
    }
    if infos.is_empty() {
        println!("No sandboxes.");
    }
    Ok(())
}

async fn handle_land(db: &Database, sandbox_id: &str) -> Result<()> {
    let row = sandbox_ops::resolve_row(db, sandbox_id)?;
    let outcome = sandbox_ops::land_and_record(db, &row).await?;
    if let Some(refusal) = sandbox_ops::land_refusal(&row, &outcome) {
        bail!("{refusal}");
    }
    println!(
        "Landed sandbox {} (branch {}) onto {}.",
        row.id, row.sandbox_branch, row.base_branch
    );
    Ok(())
}

async fn handle_discard(db: &Database, sandbox_id: &str, discard_unique: bool) -> Result<()> {
    let row = sandbox_ops::resolve_row(db, sandbox_id)?;
    match sandbox_ops::discard_and_record(db, &row, discard_unique).await? {
        sandbox::DiscardOutcome::Discarded => {
            println!(
                "Discarded sandbox {} ({}, branch {}).",
                row.id, row.worktree_path, row.sandbox_branch
            );
            Ok(())
        }
        sandbox::DiscardOutcome::RefusedUniqueCommits => bail!(
            "{}",
            sandbox_ops::discard_refusal(&row, "--discard-unique-commits")
        ),
    }
}
