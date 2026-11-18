use super::*;

impl Database {
    /// Mark a spec `unreviewed` (review on) and clear tier/findings.
    pub fn mark_spec_unreviewed(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET review_state = 'unreviewed', review_tier = NULL, review_findings = NULL, updated_at = ?2 WHERE id = ?1",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    /// Mark a spec `approved` with no tier (review off / placeholders).
    pub fn mark_spec_review_approved_no_tier(&self, spec_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET review_state = 'approved', review_tier = NULL, review_findings = NULL, updated_at = ?2 WHERE id = ?1",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        Ok(rows > 0)
    }

    /// Approve every `unreviewed`/`in_review` spec with no tier (toggle off).
    pub fn approve_pending_spec_reviews(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET review_state = 'approved', review_tier = NULL, review_findings = NULL, updated_at = ?1 WHERE review_state IN ('unreviewed', 'in_review')",
            params![Utc::now().timestamp_millis()],
        )?;
        Ok(rows)
    }

    /// Requeue `in_review` specs on daemon restart.
    pub fn requeue_in_review_specs(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE graph_specs SET review_state = 'unreviewed', updated_at = ?1 WHERE review_state = 'in_review'",
            params![Utc::now().timestamp_millis()],
        )?;
        Ok(rows)
    }

    /// Claim the next unreviewed spec (oldest first) for review.
    pub fn claim_next_unreviewed_spec(&self) -> Result<Option<GraphSpec>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let spec_id: Option<String> = conn
            .query_row(
                "SELECT id FROM graph_specs WHERE review_state = 'unreviewed' ORDER BY updated_at ASC, id ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(spec_id) = spec_id else {
            return Ok(None);
        };
        let updated = conn.execute(
            "UPDATE graph_specs SET review_state = 'in_review', updated_at = ?2 WHERE id = ?1 AND review_state = 'unreviewed'",
            params![spec_id, Utc::now().timestamp_millis()],
        )?;
        if updated == 0 {
            return Ok(None);
        }
        drop(conn);
        self.get_graph_spec(&spec_id)
    }

    pub fn count_in_review_specs(&self) -> Result<usize> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM graph_specs WHERE review_state = 'in_review'",
            [],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as usize)
    }

    pub fn set_spec_review_result(
        &self,
        spec_id: &str,
        state: crate::domain::graphs::SpecReviewState,
        tier: Option<crate::domain::graphs::SpecReviewTier>,
        findings: Option<&[crate::domain::graphs::SpecReviewFinding]>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let findings_json = findings.and_then(|f| serde_json::to_string(f).ok());
        let rows = conn.execute(
            "UPDATE graph_specs SET review_state = ?1, review_tier = ?2, review_findings = ?3, updated_at = ?5 WHERE id = ?4",
            params![
                state.as_str(),
                tier.map(|t| t.as_str()),
                findings_json,
                spec_id,
                Utc::now().timestamp_millis(),
            ],
        )?;
        Ok(rows > 0)
    }
}
