//! CM49: DB layer for crews — named, ordered member lists that ensembles,
//! agent nodes and agent hooks bind to. All SQL for the `crews`/
//! `crew_members` tables and the `ensembles.crew_id` binding columns lives
//! here; the pure validation and types live in `crate::domain::crews`.
//!
//! The single sync path is [`Database::sync_ensemble_with_crew`]: it rewrites a
//! bound ensemble's member rows from its crew when the crew is newer, and it
//! is the ONLY path that rewrites a bound ensemble's members (spec req 5).

use anyhow::{anyhow, Result};
use rusqlite::{params, OptionalExtension};

use crate::db::Database;
use crate::domain::crews::{validate_crew_members, validate_crew_name, Crew, CrewMember};
use crate::domain::graphs::{EnsembleMember, GraphNode, GraphNodeKind};

use super::crew_member_config::member_node_config;

/// The crew member cap: `graphs.ensemble_max_members_sequential`, read at each
/// write. A crew is kind-agnostic, so it takes the sequential ensemble cap.
fn crew_member_cap() -> usize {
    crate::domain::canopy_config::CanopyConfig::load(&crate::domain::prompts::canopy_dir())
        .graphs
        .ensemble_max_members_sequential
}

/// One ensemble's crew binding columns: `(crew_id, crew_name,
/// crew_version_applied)`, each `None` when unset. `crew_name` alone (with a
/// NULL `crew_id`) is a pending import binding.
pub type EnsembleCrewBinding = (Option<String>, Option<String>, Option<i64>);

/// Outcome of [`Database::sync_ensemble_with_crew`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The ensemble is not bound to a crew — nothing to do.
    Unbound,
    /// A member of this ensemble is in flight — never touch its rows.
    SkippedInflight,
    /// The applied version is already current.
    UpToDate,
    /// The member rows were rewritten from the crew.
    Synced { from_version: i64, to_version: i64 },
    /// The ensemble is bound to a crew NAME that does not exist locally yet
    /// (a pending import). Nothing was rewritten; dispatch blocks naming the
    /// crew until a crew of that name is created.
    MissingCrew,
}

fn read_crew_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Crew> {
    Ok(Crew {
        id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        version: row.get(3)?,
    })
}

fn read_crew_member_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CrewMember> {
    Ok(CrewMember {
        position: row.get(0)?,
        platform: row.get(1)?,
        model: row.get(2)?,
        timeout_minutes: row.get(3)?,
        infra_retry_limit: row.get(4)?,
        infra_crash_max_seconds: row.get(5)?,
        infra_backoff_seconds: row.get(6)?,
    })
}

impl Database {
    /// Create a crew and its members in one transaction (version 1).
    pub fn crew_create(
        &self,
        name: &str,
        description: Option<&str>,
        members: &[CrewMember],
    ) -> Result<Crew> {
        validate_crew_name(name).map_err(|e| anyhow!(e))?;
        validate_crew_members(members, crew_member_cap()).map_err(|e| anyhow!(e))?;
        let name = name.trim();
        let id = uuid::Uuid::new_v4().to_string();
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO crews (id, name, description, version) VALUES (?1, ?2, ?3, 1)",
            params![&id, name, description],
        )
        .map_err(|e| anyhow!("could not create crew '{name}': {e}"))?;
        for (index, member) in members.iter().enumerate() {
            insert_crew_member(&tx, &id, index as i64, member)?;
        }
        tx.commit()?;
        Ok(Crew {
            id,
            name: name.to_string(),
            description: description.map(str::to_string),
            version: 1,
        })
    }

    /// Seed a crew from an ensemble's current members. Refuses when any member
    /// carries a `prompt_override` (a per-member angle cannot be a crew).
    pub fn crew_create_from_ensemble(
        &self,
        name: &str,
        description: Option<&str>,
        ensemble_id: &str,
    ) -> Result<Crew> {
        let details = self
            .get_ensemble_details(ensemble_id)?
            .ok_or_else(|| anyhow!("ensemble '{ensemble_id}' not found."))?;
        if details
            .members
            .iter()
            .any(|member| member.prompt_override.is_some())
        {
            anyhow::bail!(
                "ensemble '{}' has per-member prompt_override and cannot seed a crew; \
                 write the members literally instead.",
                details.ensemble.name
            );
        }
        let members: Vec<CrewMember> = details
            .members
            .iter()
            .enumerate()
            .map(|(index, member)| CrewMember {
                position: index as i64,
                platform: member.platform.clone(),
                model: member.model.clone(),
                timeout_minutes: member.timeout_minutes,
                infra_retry_limit: member.infra_retry_limit,
                infra_crash_max_seconds: member.infra_crash_max_seconds,
                infra_backoff_seconds: member.infra_backoff_seconds,
            })
            .collect();
        self.crew_create(name, description, &members)
    }

    pub fn crew_get(&self, name: &str) -> Result<Option<(Crew, Vec<CrewMember>)>> {
        let crew = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            conn.query_row(
                "SELECT id, name, description, version FROM crews WHERE name = ?1",
                params![name.trim()],
                read_crew_row,
            )
            .optional()?
        };
        let Some(crew) = crew else {
            return Ok(None);
        };
        let members = self.crew_members(&crew.id)?;
        Ok(Some((crew, members)))
    }

    fn crew_members(&self, crew_id: &str) -> Result<Vec<CrewMember>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT position, platform, model, timeout_minutes, infra_retry_limit, \
                    infra_crash_max_seconds, infra_backoff_seconds
             FROM crew_members WHERE crew_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![crew_id], read_crew_member_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn crew_list(&self) -> Result<Vec<(Crew, Vec<CrewMember>)>> {
        let crews: Vec<Crew> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt =
                conn.prepare("SELECT id, name, description, version FROM crews ORDER BY name ASC")?;
            let rows = stmt.query_map([], read_crew_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        crews
            .into_iter()
            .map(|crew| {
                let members = self.crew_members(&crew.id)?;
                Ok((crew, members))
            })
            .collect()
    }

    /// Update a crew. A member-list write bumps `version` in the same
    /// transaction; a description/rename-only write does not.
    pub fn crew_update(
        &self,
        name: &str,
        members: Option<&[CrewMember]>,
        description: Option<&str>,
        rename: Option<&str>,
    ) -> Result<Crew> {
        let name = name.trim();
        if let Some(rename) = rename {
            validate_crew_name(rename).map_err(|e| anyhow!(e))?;
        }
        if let Some(members) = members {
            validate_crew_members(members, crew_member_cap()).map_err(|e| anyhow!(e))?;
        }
        let (crew_id, current_version) = self
            .crew_id_and_version(name)?
            .ok_or_else(|| anyhow!("crew '{name}' not found."))?;
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let new_name = rename.map(str::trim).unwrap_or(name);
        if rename.is_some() {
            let taken: bool = tx
                .query_row(
                    "SELECT 1 FROM crews WHERE name = ?1 AND id != ?2",
                    params![new_name, &crew_id],
                    |_| Ok(true),
                )
                .optional()?
                .unwrap_or(false);
            if taken {
                anyhow::bail!("crew '{new_name}' already exists.");
            }
        }
        tx.execute(
            "UPDATE crews SET name = ?1, description = COALESCE(?2, description), updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?3",
            params![new_name, description, &crew_id],
        )?;
        // Keep the denormalised display name on bound ensembles current.
        // Bindings join on `crew_id`, so nothing else depends on it.
        tx.execute(
            "UPDATE ensembles SET crew_name = ?1 WHERE crew_id = ?2",
            params![new_name, &crew_id],
        )?;
        let new_version = if let Some(members) = members {
            tx.execute(
                "DELETE FROM crew_members WHERE crew_id = ?1",
                params![&crew_id],
            )?;
            for (index, member) in members.iter().enumerate() {
                insert_crew_member(&tx, &crew_id, index as i64, member)?;
            }
            tx.execute(
                "UPDATE crews SET version = version + 1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?1",
                params![&crew_id],
            )?;
            current_version + 1
        } else {
            current_version
        };
        let description: Option<String> = tx.query_row(
            "SELECT description FROM crews WHERE id = ?1",
            params![&crew_id],
            |row| row.get(0),
        )?;
        tx.commit()?;
        Ok(Crew {
            id: crew_id,
            name: new_name.to_string(),
            description,
            version: new_version,
        })
    }

    /// Delete a crew. Refused while any ensemble, node or hook binds it; the
    /// error names every binding.
    pub fn crew_delete(&self, name: &str) -> Result<()> {
        let name = name.trim();
        let bindings = self.crew_bindings(name)?;
        if !bindings.is_empty() {
            let list = bindings
                .iter()
                .map(|b| format!("{} {} '{}'", b.graph, b.kind.as_str(), b.name))
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("crew '{name}' is still bound by: {list}");
        }
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM crews WHERE name = ?1", params![name])?;
        if rows == 0 {
            anyhow::bail!("crew '{name}' not found.");
        }
        Ok(())
    }

    fn crew_id_and_version(&self, name: &str) -> Result<Option<(String, i64)>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Ok(conn
            .query_row(
                "SELECT id, version FROM crews WHERE name = ?1",
                params![name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Ensemble ids bound to a crew, each with its graph's status string.
    /// Used by `crew update`'s eager sync to skip running/pausing graphs.
    pub fn crew_bound_ensembles(&self, name: &str) -> Result<Vec<(String, String)>> {
        let rows: Vec<(String, Option<String>, Option<String>)> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT e.id, e.graph_id, e.spec_id
                 FROM ensembles e LEFT JOIN crews c ON c.id = e.crew_id
                 WHERE c.name = ?1 OR (e.crew_id IS NULL AND e.crew_name = ?1)",
            )?;
            let rows = stmt.query_map(params![name.trim()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let mut out = Vec::with_capacity(rows.len());
        for (ensemble_id, graph_id, spec_id) in rows {
            let status = self.graph_status_for_scope(graph_id.as_deref(), spec_id.as_deref())?;
            out.push((ensemble_id, status));
        }
        Ok(out)
    }

    fn graph_status_for_scope(
        &self,
        graph_id: Option<&str>,
        spec_id: Option<&str>,
    ) -> Result<String> {
        let graph_id = match graph_id {
            Some(id) => Some(id.to_string()),
            None => match spec_id {
                Some(id) => self.get_graph_spec(id)?.and_then(|spec| spec.graph_id),
                None => None,
            },
        };
        let Some(graph_id) = graph_id else {
            return Ok("unknown".to_string());
        };
        Ok(self
            .get_graph(&graph_id)?
            .map(|graph| graph.status.as_str().to_string())
            .unwrap_or_else(|| "unknown".to_string()))
    }

    /// Whether any member of `ensemble_id` has a run in flight.
    pub fn ensemble_has_inflight_runs(&self, ensemble_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM graph_runs
                 WHERE node_id IN (SELECT node_id FROM ensemble_members WHERE ensemble_id = ?1)
                   AND status IN ('running', 'waiting')
                 LIMIT 1",
                params![ensemble_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// `(crew_id, crew_name, crew_version_applied)` for an ensemble, each
    /// `None` when unset. `crew_name` alone (with a NULL `crew_id`) is a
    /// pending import binding.
    pub fn ensemble_crew_binding(&self, ensemble_id: &str) -> Result<Option<EnsembleCrewBinding>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Ok(conn
            .query_row(
                "SELECT crew_id, crew_name, crew_version_applied FROM ensembles WHERE id = ?1",
                params![ensemble_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?)
    }

    /// CM49 requirement 5, the one sync function the engine and `crew update`
    /// share. Rewrites a bound ensemble's member rows from its crew when the
    /// applied version is stale, then stores the applied version and clamps
    /// `min_pass`. NEVER runs while a member of the ensemble is in flight —
    /// replacing members under an in-flight dispatch killed a graph with a
    /// FOREIGN KEY failure on 2026-09-26. No other path may rewrite a bound
    /// ensemble's members.
    pub fn sync_ensemble_with_crew(&self, ensemble_id: &str) -> Result<SyncOutcome> {
        let Some((crew_id, crew_name, applied)) = self.ensemble_crew_binding(ensemble_id)? else {
            return Ok(SyncOutcome::Unbound);
        };
        let crew = match (crew_id.as_deref(), crew_name.as_deref()) {
            (Some(id), _) => self.crew_by_id(id)?,
            (None, Some(name)) => self.crew_get(name)?.map(|(crew, _)| crew),
            (None, None) => return Ok(SyncOutcome::Unbound),
        };
        let Some(crew) = crew else {
            // Pending import: bound to a name whose crew does not exist yet.
            return Ok(SyncOutcome::MissingCrew);
        };
        if self.ensemble_has_inflight_runs(ensemble_id)? {
            return Ok(SyncOutcome::SkippedInflight);
        }
        // A pending binding (NULL `crew_id`, name resolved) materialises on
        // its first sync: stamp the id so later reads join and rename hygiene
        // applies. `rewrite_ensemble_from_crew` below is still the only row
        // writer.
        if crew_id.as_deref() != Some(crew.id.as_str()) {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            conn.execute(
                "UPDATE ensembles SET crew_id = ?1, crew_name = ?2 WHERE id = ?3",
                params![&crew.id, &crew.name, ensemble_id],
            )?;
        }
        let crew_members = self.crew_members(&crew.id)?;
        let applied = applied.unwrap_or(0);
        if applied >= crew.version {
            return Ok(SyncOutcome::UpToDate);
        }
        self.rewrite_ensemble_from_crew(ensemble_id, &crew, &crew_members)?;
        Ok(SyncOutcome::Synced {
            from_version: applied,
            to_version: crew.version,
        })
    }

    fn crew_by_id(&self, crew_id: &str) -> Result<Option<Crew>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Ok(conn
            .query_row(
                "SELECT id, name, description, version FROM crews WHERE id = ?1",
                params![crew_id],
                read_crew_row,
            )
            .optional()?)
    }

    /// Rebuild the ensemble's member rows from `crew_members` (position order,
    /// the shared `prompt_template` for every member, no `prompt_override`),
    /// through the existing `replace_ensemble_members`, then store the applied
    /// version and clamp `min_pass`.
    fn rewrite_ensemble_from_crew(
        &self,
        ensemble_id: &str,
        crew: &Crew,
        crew_members: &[CrewMember],
    ) -> Result<()> {
        let details = self
            .get_ensemble_details(ensemble_id)?
            .ok_or_else(|| anyhow!("ensemble '{ensemble_id}' not found."))?;
        let ensemble = &details.ensemble;
        let old_members = &details.members;
        let mut new_members = Vec::with_capacity(crew_members.len());
        let mut new_nodes = Vec::with_capacity(crew_members.len());
        // New member nodes (a fresh or growing binding) continue the scope's
        // position sequence after every existing node, exactly as
        // `graph_ensemble`'s append convention does — never colliding with
        // a plain node already in the graph.
        let mut next_position = self
            .scope_max_node_position(ensemble.spec_id.as_deref(), ensemble.graph_id.as_deref())?
            + 1;
        for (index, member) in crew_members.iter().enumerate() {
            let node_id = old_members
                .get(index)
                .map(|m| m.node_id.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
            let position = match self.node_position(&node_id)? {
                Some(existing) => existing,
                None => {
                    let position = next_position;
                    next_position += 1;
                    position
                }
            };
            new_members.push(EnsembleMember {
                ensemble_id: ensemble_id.to_string(),
                node_id: node_id.clone(),
                position: index as i64,
                platform: member.platform.clone(),
                model: member.model.clone(),
                prompt_override: None,
                timeout_minutes: member.timeout_minutes,
                infra_retry_limit: member.infra_retry_limit,
                infra_crash_max_seconds: member.infra_crash_max_seconds,
                infra_backoff_seconds: member.infra_backoff_seconds,
            });
            new_nodes.push(GraphNode {
                id: node_id,
                spec_id: ensemble.spec_id.clone(),
                graph_id: ensemble.graph_id.clone(),
                name: format!("{} [{}]", ensemble.name, index + 1),
                kind: GraphNodeKind::Agent,
                config: member_node_config(
                    &member.platform,
                    member.model.as_deref(),
                    &ensemble.prompt_template,
                    member.timeout_minutes.unwrap_or(ensemble.timeout_minutes),
                    member.infra_retry_limit.or(ensemble.infra_retry_limit),
                    member
                        .infra_crash_max_seconds
                        .or(ensemble.infra_crash_max_seconds),
                    member
                        .infra_backoff_seconds
                        .or(ensemble.infra_backoff_seconds),
                ),
                position,
                created_at: chrono::Utc::now(),
            });
        }
        self.replace_ensemble_members(ensemble_id, &new_members, &new_nodes)?;
        let crew_len = crew_members.len() as i64;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE ensembles
             SET crew_version_applied = ?1, min_pass = MIN(min_pass, ?2)
             WHERE id = ?3",
            params![crew.version, crew_len, ensemble_id],
        )?;
        Ok(())
    }

    fn node_position(&self, node_id: &str) -> Result<Option<i64>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        Ok(conn
            .query_row(
                "SELECT position FROM graph_nodes WHERE id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The highest `graph_nodes.position` already used in an ensemble's scope
    /// (its spec, or the graph), or 0 when the scope has no nodes.
    fn scope_max_node_position(
        &self,
        spec_id: Option<&str>,
        graph_id: Option<&str>,
    ) -> Result<i64> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let max: Option<i64> = conn
            .query_row(
                "SELECT MAX(position) FROM graph_nodes
                 WHERE (?1 IS NOT NULL AND spec_id = ?1)
                    OR (?2 IS NOT NULL AND graph_id = ?2 AND spec_id IS NULL)",
                params![spec_id, graph_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(max.unwrap_or(0))
    }
}

fn insert_crew_member(
    tx: &rusqlite::Transaction<'_>,
    crew_id: &str,
    position: i64,
    member: &CrewMember,
) -> Result<()> {
    tx.execute(
        "INSERT INTO crew_members
         (crew_id, position, platform, model, timeout_minutes, infra_retry_limit,
          infra_crash_max_seconds, infra_backoff_seconds)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            crew_id,
            position,
            &member.platform,
            &member.model,
            &member.timeout_minutes,
            &member.infra_retry_limit,
            &member.infra_crash_max_seconds,
            &member.infra_backoff_seconds,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::roster_entries::{NewRosterEntry, RosterState};
    use crate::domain::graphs::{
        Ensemble, EnsembleKind, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphNodeRun,
        GraphRunStatus, GraphSpec, GraphSpecStatus,
    };

    fn test_db() -> Database {
        let dir = tempfile::tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn insert_graph(db: &Database, id: &str) {
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: "G".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        })
        .unwrap();
    }

    fn member(platform: &str) -> CrewMember {
        CrewMember {
            position: 0,
            platform: platform.to_string(),
            model: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    /// Insert a spec and a minimal 2-member ensemble (kind parallel) so bind
    /// and from-ensemble paths have a real ensemble to work against.
    fn insert_ensemble(db: &Database, id: &str, override_: Option<&str>) {
        insert_ensemble_sized(db, id, 2, 2, override_);
    }

    /// Insert a spec and a `member_count`-member parallel ensemble with the
    /// given `min_pass`.
    fn insert_ensemble_sized(
        db: &Database,
        id: &str,
        member_count: usize,
        min_pass: i64,
        override_: Option<&str>,
    ) {
        let spec_id = format!("{id}-spec");
        db.insert_graph_spec(&GraphSpec {
            id: spec_id.clone(),
            graph_id: None,
            name: "Spec One".to_string(),
            description: None,
            position: 1,
            parallelizable: false,
            status: GraphSpecStatus::Pending,
            started_at: None,
            completed_at: None,
            spec_start_head: None,
            spec_start_dirty: None,
            spec_end_dirty: None,
            spec_end_dirty_paths: None,
            spec_committed_head: None,
            workdir: None,
            completed_via: None,
            completed_via_reason: None,
            completed_via_at: None,
            spec_code: None,
            spec_scope: None,
            spec_type: None,
        })
        .ok();
        let now = chrono::Utc::now();
        let ensemble = Ensemble {
            mutex: Vec::new(),
            id: id.to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: format!("Ensemble {id}"),
            prompt_template: "tmpl".to_string(),
            join_node_id: format!("{id}-join"),
            entry_from_node: format!("{id}-kickoff"),
            entry_condition: GraphEdgeCondition::Always,
            min_pass,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: format!("{id}-join"),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            commit_rights: false,
            created_at: now,
            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        };
        let member_ids: Vec<String> = (1..=member_count).map(|i| format!("m{i}")).collect();
        let members: Vec<EnsembleMember> = member_ids
            .iter()
            .enumerate()
            .map(|(index, node_id)| EnsembleMember {
                ensemble_id: id.to_string(),
                node_id: format!("{id}-{node_id}"),
                position: index as i64,
                platform: "claude".to_string(),
                model: None,
                prompt_override: override_.map(str::to_string),
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            })
            .collect();
        let mut nodes = vec![GraphNode {
            id: format!("{id}-kickoff"),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: now,
        }];
        for (index, node_id) in member_ids.iter().enumerate() {
            nodes.push(GraphNode {
                id: format!("{id}-{node_id}"),
                spec_id: Some(spec_id.clone()),
                graph_id: None,
                name: node_id.to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude"}),
                position: index as i64 + 2,
                created_at: now,
            });
        }
        let join_node = GraphNode {
            id: format!("{id}-join"),
            spec_id: Some(spec_id),
            graph_id: None,
            name: "join".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": id}),
            position: 50,
            created_at: now,
        };
        db.insert_ensemble_unit(&ensemble, &members, &nodes, &join_node, &[])
            .unwrap();
    }

    #[test]
    fn version_bumps_only_on_member_write() {
        let db = test_db();
        let created = db
            .crew_create("implementers", None, &[member("claude"), member("codex")])
            .unwrap();
        assert_eq!(created.version, 1);
        // Description-only write: version unchanged.
        let same = db
            .crew_update("implementers", None, Some("the implementers"), None)
            .unwrap();
        assert_eq!(same.version, 1, "description-only write must not bump");
        // Member-list write: version bumped in the same call.
        let bumped = db
            .crew_update("implementers", Some(&[member("claude")]), None, None)
            .unwrap();
        assert_eq!(bumped.version, 2, "member write must bump the version");
    }

    #[test]
    fn from_ensemble_ok_and_refuses_prompt_override() {
        let db = test_db();
        insert_ensemble(&db, "ens-plain", None);
        let crew = db
            .crew_create_from_ensemble("plain", None, "ens-plain")
            .unwrap();
        let (_, members) = db.crew_get("plain").unwrap().unwrap();
        assert_eq!(members.len(), 2, "seeded from the ensemble's members");
        assert_eq!(crew.version, 1);

        insert_ensemble(&db, "ens-panel", Some("review from the security angle"));
        let err = db
            .crew_create_from_ensemble("panel", None, "ens-panel")
            .unwrap_err();
        assert!(
            err.to_string().contains("prompt_override"),
            "a panel with per-member angles must refuse: {err}"
        );
    }

    #[test]
    fn delete_refused_while_bound_lists_bindings() {
        let db = test_db();
        insert_ensemble(&db, "ens1", None);
        db.crew_create("implementers", None, &[member("claude"), member("codex")])
            .unwrap();
        db.bind_ensemble_to_crew("ens1", "implementers").unwrap();
        // Bound now: delete is refused and names the ensemble.
        let err = db.crew_delete("implementers").unwrap_err();
        assert!(
            err.to_string().contains("ensemble 'Ensemble ens1'"),
            "refusal must list the binding: {err}"
        );
        db.unbind_ensemble_crew("ens1").unwrap();
        db.crew_delete("implementers").unwrap();
    }

    #[test]
    fn bind_clamps_min_pass_and_stamps_version() {
        let db = test_db();
        insert_ensemble(&db, "ens1", None);
        db.crew_create("two", None, &[member("claude"), member("codex")])
            .unwrap();
        let crew = db.bind_ensemble_to_crew("ens1", "two").unwrap();
        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(details.members.len(), 2);
        assert_eq!(details.ensemble.min_pass, 2);
        assert_eq!(details.ensemble.crew_id.as_deref(), Some(crew.id.as_str()));
        assert_eq!(details.ensemble.crew_version_applied, Some(crew.version));
        // Unbind keeps the rows as literal members and clears the columns.
        db.unbind_ensemble_crew("ens1").unwrap();
        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(details.members.len(), 2, "rows kept on unbind");
        assert!(details.ensemble.crew_id.is_none());
        assert!(details.ensemble.crew_name.is_none());
        assert!(details.ensemble.crew_version_applied.is_none());
    }

    /// Spec req 5b: the sync never rewrites rows while a member of the
    /// ensemble is in flight. A fake `running` member run must make the sync
    /// skip and leave `crew_version_applied` behind; clearing it lets the
    /// next sync catch up.
    #[test]
    fn sync_is_skipped_while_a_member_is_in_flight() {
        let db = test_db();
        insert_graph(&db, "g1");
        insert_ensemble(&db, "ens1", None);
        db.crew_create("two", None, &[member("claude"), member("codex")])
            .unwrap();
        db.bind_ensemble_to_crew("ens1", "two").unwrap();
        let before = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(before.ensemble.crew_version_applied, Some(1));

        db.insert_graph_run(&GraphNodeRun {
            id: "run-1".to_string(),
            graph_id: "g1".to_string(),
            spec_id: "ens1-spec".to_string(),
            node_id: "ens1-m1".to_string(),
            status: GraphRunStatus::Running,
            input: None,
            output: None,
            started_at: chrono::Utc::now(),
            completed_at: None,
            iteration: 0,
            pid: None,
            boot_id: None,
            session_id: None,
            executed_platform: None,
            executed_model: None,
        })
        .unwrap();
        db.crew_update(
            "two",
            Some(&[member("claude"), member("codex"), member("gemini")]),
            None,
            None,
        )
        .unwrap();

        let outcome = db.sync_ensemble_with_crew("ens1").unwrap();
        assert_eq!(outcome, SyncOutcome::SkippedInflight);
        let during = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(during.members.len(), 2, "in-flight: member rows untouched");
        assert_eq!(
            during.ensemble.crew_version_applied,
            Some(1),
            "version stays behind so the next idle dispatch catches up"
        );

        // Clear the in-flight run: the next sync applies version 2.
        db.update_graph_run_result("run-1", GraphRunStatus::Pass, None, None)
            .unwrap();
        let outcome = db.sync_ensemble_with_crew("ens1").unwrap();
        assert_eq!(
            outcome,
            SyncOutcome::Synced {
                from_version: 1,
                to_version: 2
            }
        );
        assert_eq!(
            db.get_ensemble_details("ens1")
                .unwrap()
                .unwrap()
                .members
                .len(),
            3
        );
    }

    /// Spec req 5a: a crew edit made while the graph runs is picked up by the
    /// NEXT sync (the engine's lazy dispatch sync) — the rows are rewritten
    /// and the applied version advances.
    #[test]
    fn sync_rewrites_members_when_the_crew_version_is_newer() {
        let db = test_db();
        insert_ensemble(&db, "ens1", None);
        db.crew_create("two", None, &[member("claude"), member("codex")])
            .unwrap();
        db.bind_ensemble_to_crew("ens1", "two").unwrap();
        db.crew_update("two", Some(&[member("gemini"), member("grok")]), None, None)
            .unwrap();

        let outcome = db.sync_ensemble_with_crew("ens1").unwrap();
        assert_eq!(
            outcome,
            SyncOutcome::Synced {
                from_version: 1,
                to_version: 2
            }
        );
        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        let platforms: Vec<String> = details
            .members
            .iter()
            .map(|member| member.platform.clone())
            .collect();
        assert_eq!(platforms, vec!["gemini", "grok"]);
        assert_eq!(details.ensemble.crew_version_applied, Some(2));
    }

    /// Spec req 6: pair resolution takes the first member the roster reports
    /// available, and blocks (returns `None`) when none is.
    #[test]
    fn resolve_crew_pair_skips_an_unavailable_member() {
        let db = test_db();
        db.crew_create("two", None, &[member("claude"), member("codex")])
            .unwrap();
        db.roster_upsert(NewRosterEntry {
            platform: "claude",
            model_pattern: None,
            state: RosterState::Unavailable,
            reason: Some("quota"),
            until: None,
        })
        .unwrap();
        let (platform, _model, index) = db.resolve_crew_pair("two", None).unwrap().unwrap();
        assert_eq!(platform, "codex", "the unavailable first member is skipped");
        assert_eq!(index, 1);

        db.roster_upsert(NewRosterEntry {
            platform: "codex",
            model_pattern: None,
            state: RosterState::Unavailable,
            reason: Some("quota"),
            until: None,
        })
        .unwrap();
        assert!(
            db.resolve_crew_pair("two", None).unwrap().is_none(),
            "no available member blocks"
        );
    }

    /// CM49: the hook binding scan is live — an agent hook bound to a crew
    /// (its `crew` key, no platform) refuses the crew's deletion and names it.
    #[test]
    fn delete_refused_lists_bound_hook() {
        use crate::domain::graphs::{GraphCompletionHook, GraphHookEvent};
        let db = test_db();
        db.insert_graph(&crate::domain::graphs::Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: "g1".to_string(),
            name: "G".to_string(),
            description: None,
            workdir: "/tmp".to_string(),
            status: crate::domain::graphs::GraphStatus::Draft,
            trigger: None,
            created_at: chrono::Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: [(
                GraphHookEvent::OnCompleted,
                vec![GraphCompletionHook {
                    platform: None,
                    crew: Some("hooked".to_string()),
                    model: None,
                    effort: None,
                    prompt: Some("hi".to_string()),
                    command: None,
                    target_session_id: None,
                    target_session_name: None,
                    timeout_minutes: None,
                    target_graph_id: None,
                    queue_id: None,
                    workdir_override: None,
                    idea: None,
                }],
            )]
            .into_iter()
            .collect(),
            require_spec_type: false,
        })
        .unwrap();
        db.crew_create("hooked", None, &[member("claude")]).unwrap();

        let err = db.crew_delete("hooked").unwrap_err();
        assert!(
            err.to_string().contains("hook 'on_completed'"),
            "refusal must list the hook binding: {err}"
        );
    }

    /// CM49 req 4: binding a smaller crew shrinks the ensemble and clamps
    /// `min_pass` down with it (`MIN(min_pass, crew_len)`).
    #[test]
    fn bind_clamps_min_pass_on_shrink() {
        let db = test_db();
        insert_ensemble_sized(&db, "ens1", 3, 3, None);
        db.crew_create("two", None, &[member("claude"), member("codex")])
            .unwrap();
        db.bind_ensemble_to_crew("ens1", "two").unwrap();
        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(details.members.len(), 2, "the crew shrank the ensemble");
        assert_eq!(
            details.ensemble.min_pass, 2,
            "min_pass clamped to the crew size"
        );
    }

    /// CM49 req 3: a crew-bound agent NODE refuses its crew's deletion and is
    /// named in the error (the node binding kind).
    #[test]
    fn delete_refused_lists_bound_node() {
        let db = test_db();
        insert_graph(&db, "g1");
        db.crew_create("noded", None, &[member("claude")]).unwrap();
        db.insert_graph_node(&GraphNode {
            id: "n1".to_string(),
            spec_id: None,
            graph_id: Some("g1".to_string()),
            name: "crew node".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"crew": "noded"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();

        let err = db.crew_delete("noded").unwrap_err();
        assert!(
            err.to_string().contains("node 'crew node'"),
            "refusal must list the node binding: {err}"
        );
    }

    /// CM49 req 6: resolution starts after the failed index and wraps around
    /// the crew's end.
    #[test]
    fn resolve_crew_pair_wraps_after_index() {
        let db = test_db();
        db.crew_create("three", None, &[member("a"), member("b"), member("c")])
            .unwrap();
        let (_, _, index) = db.resolve_crew_pair("three", Some(0)).unwrap().unwrap();
        assert_eq!(index, 1, "the member after index 0");
        let (_, _, index) = db.resolve_crew_pair("three", Some(2)).unwrap().unwrap();
        assert_eq!(index, 0, "the rotation wraps past the last member");
    }
}
