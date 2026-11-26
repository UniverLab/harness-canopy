//! Storage-retention settings: what `canopy clean` removes on demand
//! (`[clean]`) and what the daemon health routine's prune pass ages out in
//! the background (`[retention]`). Kept together so the two overlapping
//! lifetimes — on-demand session/file cleanup vs. background `graph_runs`
//! payload ageing — stay comparable in one place.

use serde::{Deserialize, Serialize};

use crate::domain::settings;

/// Settings for `canopy clean` (soft cleanup). Read from the `[clean]` table
/// in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanConfig {
    /// Days of retention before orphaned/error/completed
    /// `interactive_sessions` rows and orphaned log/terminal/RAG-residue
    /// artifacts become eligible for removal. Overridable per-run with
    /// `canopy clean --older-than <days>`.
    #[serde(default = "default_retention_days")]
    pub retention_days: u64,
    /// Minimum deleted rows to trigger VACUUM during clean.
    #[serde(default = "default_reclaim_min_deleted_rows")]
    pub reclaim_min_deleted_rows: usize,
}

impl Default for CleanConfig {
    fn default() -> Self {
        Self {
            retention_days: default_retention_days(),
            reclaim_min_deleted_rows: default_reclaim_min_deleted_rows(),
        }
    }
}

fn default_retention_days() -> u64 {
    settings::default_duration_units("clean.retention_days")
}

fn default_reclaim_min_deleted_rows() -> usize {
    settings::default_int("clean.reclaim_min_deleted_rows") as usize
}

/// Settings for the daemon health routine's retention prune, read from the
/// `[retention]` table in `config.toml`. Separate from `[clean]` (which
/// governs what `canopy clean` removes on demand) so the background prune
/// ages `graph_runs` payloads independently of session/file cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Days a `graph_runs` row's `input`/`output` payload is kept. Rows
    /// older than this are stripped to metadata; rows older than twice this
    /// are deleted. `0` disables `graph_runs` pruning entirely.
    #[serde(default = "default_retention_graph_runs_days")]
    pub graph_runs_days: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            graph_runs_days: default_retention_graph_runs_days(),
        }
    }
}

fn default_retention_graph_runs_days() -> u64 {
    settings::default_int("retention.graph_runs_days") as u64
}
