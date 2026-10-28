//! The sandbox operations shared by `canopy sandbox` (`sandbox_cli`) and the
//! MCP `sandbox` tool (`handler_sandbox`): row lookup, the `SandboxInfo`
//! enrichment, the land/discard calls with their `sandbox_runs` status
//! update, and the refusal wording.
//!
//! Nothing here does git work of its own: every mutating call goes to the
//! same `domain::sandbox` function the CLI has always used (`land_sandbox`,
//! `discard_sandbox`), so both surfaces refuse identically and neither
//! force-resolves a conflict.

use anyhow::{bail, Result};

use crate::db::sandbox::SandboxRun;
use crate::db::Database;
use crate::domain::sandbox::{self, DiscardOutcome, LandOutcome, SandboxInfo};

/// The stored row plus the live git facts, as every sandbox listing shows it.
pub(crate) fn enrich(row: &SandboxRun) -> SandboxInfo {
    SandboxInfo {
        id: row.id.clone(),
        path: std::path::PathBuf::from(&row.worktree_path),
        graph_id: if row.owner_type == "graph" {
            row.owner_id.clone()
        } else {
            format!("{}:{}", row.owner_type, row.owner_id)
        },
        branch: row.sandbox_branch.clone(),
        base_branch: row.base_branch.clone(),
        original_workdir: row.original_workdir.clone(),
        status: row.status.clone(),
        has_unique_commits: sandbox::has_unique_commits(row),
        behind_count: sandbox::behind_count(row),
        worktree_registered: sandbox::worktree_is_registered(row),
    }
}

/// Look a sandbox up by exact id or unambiguous prefix.
pub(crate) fn resolve_row(db: &Database, sandbox_id: &str) -> Result<SandboxRun> {
    let trimmed = sandbox_id.trim();
    match db.resolve_sandbox_id_by_prefix(trimmed)? {
        Some(id) => db.get_sandbox_run(&id)?.ok_or_else(|| {
            anyhow::anyhow!("sandbox '{trimmed}' resolved to '{id}' but the row is gone")
        }),
        None => bail!("no sandbox found for '{trimmed}'"),
    }
}

/// Land one sandbox and record the outcome on its row (`merged`), exactly as
/// `canopy sandbox land` does. The row is left untouched on every refusal.
pub(crate) async fn land_and_record(db: &Database, row: &SandboxRun) -> Result<LandOutcome> {
    let outcome = sandbox::land_sandbox(row).await?;
    if outcome == LandOutcome::Landed {
        db.update_sandbox_run_status(&row.id, "merged")?;
    }
    Ok(outcome)
}

/// Discard one sandbox and record the outcome on its row (`discarded`).
/// `force` is the caller's explicit consent to delete unique commits.
pub(crate) async fn discard_and_record(
    db: &Database,
    row: &SandboxRun,
    force: bool,
) -> Result<DiscardOutcome> {
    let outcome = sandbox::discard_sandbox(row, force).await?;
    if outcome == DiscardOutcome::Discarded {
        db.update_sandbox_run_status(&row.id, "discarded")?;
    }
    Ok(outcome)
}

/// The refusal text for a land that did not land, or `None` when it did.
/// Names the sandbox and, for a conflict, every conflicting path — one per
/// line, never resolved automatically.
pub(crate) fn land_refusal(row: &SandboxRun, outcome: &LandOutcome) -> Option<String> {
    match outcome {
        LandOutcome::Landed => None,
        LandOutcome::RefusedWrongBranch(reason) | LandOutcome::RefusedNonClean(reason) => {
            Some(format!("refusing to land sandbox {}: {reason}", row.id))
        }
        LandOutcome::RefusedConflict(paths) if paths.is_empty() => Some(format!(
            "refusing to land sandbox {}: merge conflict (no paths reported)",
            row.id
        )),
        LandOutcome::RefusedConflict(paths) => Some(format!(
            "refusing to land sandbox {}: merge conflict in:\n  {}",
            row.id,
            paths.join("\n  ")
        )),
    }
}

/// The refusal text for a discard that was refused because the branch holds
/// commits existing nowhere else. `hint` is the caller's own consent flag or
/// parameter (`--discard-unique-commits`, `force: true`).
pub(crate) fn discard_refusal(row: &SandboxRun, hint: &str) -> String {
    format!(
        "refusing to discard sandbox {}: branch '{}' holds commits that exist nowhere else; \
         re-run with {hint} to discard them",
        row.id, row.sandbox_branch
    )
}
