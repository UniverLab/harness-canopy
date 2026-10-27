//! Ensemble row field updates: single-column `UPDATE ensembles ...`
//! setters plus the `mutex` column codec. Split out of `mod.rs` to keep
//! every production file within the file-size gate.
use anyhow::{anyhow, Result};
use rusqlite::params;

use crate::db::Database;
use crate::domain::graphs::EnsembleKind;

impl Database {
    /// Update the shared prompt on the `ensembles` row itself. Propagating it
    /// onto every member node's `config` is the caller's job (via
    /// `update_graph_node_details`) — this only updates the source-of-truth
    /// row `graph_get` reads back.
    pub fn update_ensemble_prompt(&self, ensemble_id: &str, prompt_template: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensembles SET prompt_template = ?1 WHERE id = ?2",
            params![prompt_template, ensemble_id],
        )?;
        Ok(rows > 0)
    }

    /// Update the join's own config: pass threshold, straggler timeout, and
    /// shared member timeout. `None` means "leave unchanged" for each field —
    /// same convention as [`Self::update_graph_node_details`].
    #[allow(clippy::too_many_arguments)]
    pub fn update_ensemble_join_config(
        &self,
        ensemble_id: &str,
        min_pass: Option<i64>,
        straggler_timeout_minutes: Option<Option<i64>>,
        quorum_grace_minutes: Option<Option<i64>>,
        timeout_minutes: Option<i64>,
        infra_retry_limit: Option<Option<i64>>,
        infra_crash_max_seconds: Option<Option<i64>>,
        infra_backoff_seconds: Option<Option<i64>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensembles
             SET min_pass = COALESCE(?1, min_pass),
                 straggler_timeout_minutes = CASE WHEN ?2 IS NULL THEN straggler_timeout_minutes ELSE ?3 END,
                 quorum_grace_minutes = CASE WHEN ?4 IS NULL THEN quorum_grace_minutes ELSE ?5 END,
                 timeout_minutes = COALESCE(?6, timeout_minutes),
                 infra_retry_limit = CASE WHEN ?7 IS NULL THEN infra_retry_limit ELSE ?8 END,
                 infra_crash_max_seconds = CASE WHEN ?9 IS NULL THEN infra_crash_max_seconds ELSE ?10 END,
                 infra_backoff_seconds = CASE WHEN ?11 IS NULL THEN infra_backoff_seconds ELSE ?12 END
             WHERE id = ?13",
            params![
                min_pass,
                straggler_timeout_minutes.map(|_| 1),
                straggler_timeout_minutes.flatten(),
                quorum_grace_minutes.map(|_| 1),
                quorum_grace_minutes.flatten(),
                timeout_minutes,
                infra_retry_limit.map(|_| 1),
                infra_retry_limit.flatten(),
                infra_crash_max_seconds.map(|_| 1),
                infra_crash_max_seconds.flatten(),
                infra_backoff_seconds.map(|_| 1),
                infra_backoff_seconds.flatten(),
                ensemble_id,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn update_ensemble_kind(
        &self,
        ensemble_id: &str,
        kind: Option<EnsembleKind>,
        round_robin_index: Option<Option<i64>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensembles
             SET kind = COALESCE(?1, kind),
                 round_robin_index = CASE WHEN ?2 IS NULL THEN round_robin_index ELSE ?3 END
             WHERE id = ?4",
            params![
                kind.map(|k| k.as_str()),
                round_robin_index.map(|_| 1),
                round_robin_index.flatten(),
                ensemble_id,
            ],
        )?;
        Ok(rows > 0)
    }

    pub fn update_ensemble_commit_rights(
        &self,
        ensemble_id: &str,
        commit_rights: bool,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensembles SET commit_rights = ?1 WHERE id = ?2",
            params![commit_rights, ensemble_id],
        )?;
        Ok(rows > 0)
    }

    /// Replace the ensemble's named mutex list. An empty list clears it (back
    /// to NULL). The caller decides keep-vs-write (`None` means keep, i.e.
    /// don't call this).
    pub fn update_ensemble_mutex(&self, ensemble_id: &str, mutex: &[String]) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensembles SET mutex = ?1 WHERE id = ?2",
            params![encode_ensemble_mutex(mutex)?, ensemble_id],
        )?;
        Ok(rows > 0)
    }
}

/// Encode an ensemble mutex list for the `ensembles.mutex` column: NULL for
/// none, else a JSON array.
pub(super) fn encode_ensemble_mutex(mutex: &[String]) -> Result<Option<String>> {
    if mutex.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(mutex).map(Some).map_err(Into::into)
}

/// Decode `ensembles.mutex`: NULL or unparsable reads as no mutex (a missing
/// column on a pre-migration row can never fail the read).
pub(super) fn decode_ensemble_mutex(raw: Option<String>) -> Vec<String> {
    raw.and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}
