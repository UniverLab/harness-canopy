//! CM49: binding a crew to an ensemble, unbinding it, and resolving a crew's
//! member pair at dispatch. Storage for the `crews`/`crew_members` tables
//! lives in `crate::db::crews`; this module is the write edge the tools call
//! and the read edge the engine and preflight share.
//!
//! The one row writer remains [`Database::sync_ensemble_with_crew`]: bind
//! stamps the binding columns and funnels through it, so bind and later crew
//! edits share the same clamp/rewrite logic.

use anyhow::{anyhow, Result};
use chrono::Utc;
use rusqlite::params;

use crate::db::Database;
use crate::domain::crews::Crew;
use crate::domain::graphs::{member_bounds, EnsembleDetails};
use crate::domain::roster::effective_state;

impl Database {
    /// Bind an ensemble to a crew by name. Refused when the ensemble's current
    /// members carry a `prompt_override` (a per-member angle cannot come from
    /// a crew) or when the crew's size is outside the ensemble kind's bounds.
    /// Materialises the crew's members through the one sync path.
    pub fn bind_ensemble_to_crew(&self, ensemble_id: &str, crew_name: &str) -> Result<Crew> {
        let (crew, crew_members) = self.crew_get(crew_name)?.ok_or_else(|| {
            anyhow!("crew '{crew_name}' not found. Create it with crew_create first.")
        })?;
        let details = self
            .get_ensemble_details(ensemble_id)?
            .ok_or_else(|| anyhow!("ensemble '{ensemble_id}' not found."))?;
        check_bindable(&details, crew_members.len())?;
        self.stamp_crew_binding(ensemble_id, &crew.id, &crew.name)?;
        self.sync_ensemble_with_crew(ensemble_id)?;
        Ok(crew)
    }

    /// Unbind an ensemble from its crew. The current member rows stay exactly
    /// as they are (they become literal members, spec req 4); only the three
    /// binding columns are cleared.
    pub fn unbind_ensemble_crew(&self, ensemble_id: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE ensembles
             SET crew_id = NULL, crew_name = NULL, crew_version_applied = NULL
             WHERE id = ?1",
            params![ensemble_id],
        )?;
        Ok(())
    }

    fn stamp_crew_binding(&self, ensemble_id: &str, crew_id: &str, crew_name: &str) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE ensembles
             SET crew_id = ?1, crew_name = ?2, crew_version_applied = NULL
             WHERE id = ?3",
            params![crew_id, crew_name, ensemble_id],
        )?;
        Ok(())
    }

    /// Resolve the `(platform, model, index)` a crew-bound node or hook should
    /// run: the first member, in crew order, that CM48's `effective_state`
    /// reports available. `after` is the index to resume *after* (wrapping) —
    /// `None` starts at the first member; an infra retry passes the failed
    /// member's index. Returns `None` when the crew is missing or no member is
    /// available; callers turn both into a block naming the crew.
    pub fn resolve_crew_pair(
        &self,
        crew_name: &str,
        after: Option<usize>,
    ) -> Result<Option<(String, Option<String>, usize)>> {
        let Some((_crew, members)) = self.crew_get(crew_name)? else {
            return Ok(None);
        };
        if members.is_empty() {
            return Ok(None);
        }
        let now = Utc::now();
        let start = match after {
            Some(index) => (index + 1) % members.len(),
            None => 0,
        };
        for offset in 0..members.len() {
            let index = (start + offset) % members.len();
            let member = &members[index];
            let entries = self.roster_list_for_platform(&member.platform)?;
            let state = effective_state(&entries, &member.platform, member.model.as_deref(), &now);
            if state.available {
                return Ok(Some((member.platform.clone(), member.model.clone(), index)));
            }
        }
        Ok(None)
    }
}

/// The kind-bounds and prompt-override rules a bind must satisfy.
fn check_bindable(details: &EnsembleDetails, crew_len: usize) -> Result<()> {
    if details
        .members
        .iter()
        .any(|member| member.prompt_override.is_some())
    {
        anyhow::bail!(
            "ensemble '{}' has per-member prompt_override and cannot bind a crew; \
             parallel review panels stay literal.",
            details.ensemble.name
        );
    }
    let (min, max) = member_bounds(details.ensemble.kind);
    if crew_len < min || crew_len > max {
        anyhow::bail!(
            "A {} ensemble must have {min}-{max} members, got {crew_len}.",
            details.ensemble.kind.as_str()
        );
    }
    Ok(())
}
