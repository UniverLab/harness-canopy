//! F1: DB layer for ensembles — a group of agent-node members (each
//! optionally carrying its own prompt override) plus their quorum gate,
//! persisted as one [`Ensemble`] row on top of ordinary
//! `graph_nodes`/`graph_edges` rows (see `src/domain/graphs.rs` for why).

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension};
use std::io::{Error as IoError, ErrorKind};

use crate::db::Database;
use crate::domain::blueprints::{builtin_ensemble_blueprint_specs, EnsembleBlueprint};
use crate::domain::graphs::{
    Ensemble, EnsembleDetails, EnsembleKind, EnsembleMember, EnsembleMemberSpec, GraphEdge,
    GraphEdgeCondition, GraphNode, GraphNodeKind,
};

mod entry_wiring;
pub use entry_wiring::*;
mod blueprints;
mod update;
pub use update::EnsembleJoinConfigPatch;
use update::{decode_ensemble_mutex, encode_ensemble_mutex};
// Blueprint persistence lives entirely in `blueprints` (its `impl Database`
// methods are inherent, so no re-export is needed for callers); the only
// piece the tests in this module reach for directly is the member-list
// parser.
#[cfg(test)]
use blueprints::parse_ensemble_blueprint_members;
impl Database {
    /// Create an entire ensemble unit — the join node, every member node,
    /// every wiring edge (entry fan-out, member→join fan-in, join exit
    /// routing), the `ensembles` row, and every `ensemble_members` row — in
    /// one transaction. This is the DB-layer half of the "one MCP call"
    /// contract: `graph_add_ensemble` builds all these pieces and hands them
    /// here so a crash mid-creation can never leave a half-expanded ensemble.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_ensemble_unit(
        &self,
        ensemble: &Ensemble,
        members: &[EnsembleMember],
        member_nodes: &[GraphNode],
        join_node: &GraphNode,
        edges: &[GraphEdge],
    ) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;

        tx.execute(
            "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &join_node.id,
                &join_node.spec_id,
                &join_node.graph_id,
                &join_node.name,
                join_node.kind.as_str(),
                serde_json::to_string(&join_node.config)?,
                join_node.position,
                join_node.created_at.timestamp(),
            ],
        )?;

        for node in member_nodes {
            tx.execute(
                "INSERT INTO graph_nodes (id, spec_id, graph_id, name, kind, config, position, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &node.id,
                    &node.spec_id,
                    &node.graph_id,
                    &node.name,
                    node.kind.as_str(),
                    serde_json::to_string(&node.config)?,
                    node.position,
                    node.created_at.timestamp(),
                ],
            )?;
        }

        for edge in edges {
            tx.execute(
                "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    &edge.id,
                    &edge.spec_id,
                    &edge.graph_id,
                    &edge.from_node,
                    &edge.to_node,
                    edge.condition.as_str(),
                ],
            )?;
        }

        tx.execute(
            "INSERT INTO ensembles (id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds, on_pass_to, on_fail_to, kind, round_robin_index, commit_rights, created_at, crew_id, crew_name, crew_version_applied, mutex)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25)",
            params![
                &ensemble.id,
                &ensemble.spec_id,
                &ensemble.graph_id,
                &ensemble.name,
                &ensemble.prompt_template,
                &ensemble.join_node_id,
                &ensemble.entry_from_node,
                ensemble.entry_condition.as_str(),
                ensemble.min_pass,
                ensemble.straggler_timeout_minutes,
                ensemble.quorum_grace_minutes,
                ensemble.timeout_minutes,
                ensemble.infra_retry_limit,
                ensemble.infra_crash_max_seconds,
                ensemble.infra_backoff_seconds,
                &ensemble.on_pass_to,
                &ensemble.on_fail_to,
                ensemble.kind.as_str(),
                ensemble.round_robin_index,
                ensemble.commit_rights,
                ensemble.created_at.timestamp(),
                &ensemble.crew_id,
                &ensemble.crew_name,
                ensemble.crew_version_applied,
                encode_ensemble_mutex(&ensemble.mutex)?,
            ],
        )?;

        for member in members {
            tx.execute(
                "INSERT INTO ensemble_members (ensemble_id, node_id, position, platform, model, prompt_override, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    &member.ensemble_id,
                    &member.node_id,
                    member.position,
                    &member.platform,
                    &member.model,
                    &member.prompt_override,
                    &member.timeout_minutes,
                    &member.infra_retry_limit,
                    &member.infra_crash_max_seconds,
                    &member.infra_backoff_seconds,
                ],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn get_ensemble(&self, ensemble_id: &str) -> Result<Option<Ensemble>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds, on_pass_to, on_fail_to, kind, round_robin_index, commit_rights, created_at, crew_id, crew_name, crew_version_applied, mutex
             FROM ensembles WHERE id = ?1",
        )?;
        stmt.query_row(params![ensemble_id], map_ensemble_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn list_ensemble_members(&self, ensemble_id: &str) -> Result<Vec<EnsembleMember>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT ensemble_id, node_id, position, platform, model, prompt_override, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds
             FROM ensemble_members WHERE ensemble_id = ?1 ORDER BY position ASC",
        )?;
        let rows = stmt.query_map(params![ensemble_id], map_ensemble_member_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn get_ensemble_details(&self, ensemble_id: &str) -> Result<Option<EnsembleDetails>> {
        let Some(ensemble) = self.get_ensemble(ensemble_id)? else {
            return Ok(None);
        };
        let members = self.list_ensemble_members(ensemble_id)?;
        Ok(Some(EnsembleDetails { ensemble, members }))
    }

    /// Every ensemble defined directly on a spec's own graph.
    pub fn list_ensembles_for_spec(&self, spec_id: &str) -> Result<Vec<EnsembleDetails>> {
        let ids = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare("SELECT id FROM ensembles WHERE spec_id = ?1")?;
            let rows = stmt.query_map(params![spec_id], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        ids.iter()
            .filter_map(|id| self.get_ensemble_details(id).transpose())
            .collect()
    }

    /// Every ensemble defined on a graph's top-level graph.
    pub fn list_ensembles_for_graph(&self, graph_id: &str) -> Result<Vec<EnsembleDetails>> {
        let ids = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare("SELECT id FROM ensembles WHERE graph_id = ?1")?;
            let rows = stmt.query_map(params![graph_id], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        ids.iter()
            .filter_map(|id| self.get_ensemble_details(id).transpose())
            .collect()
    }

    /// Every ensemble across every graph — CM28's global reporting surface
    /// for `graph_audit_node_configs`, mirroring `list_all_graph_nodes`.
    pub fn list_all_ensembles(&self) -> Result<Vec<EnsembleDetails>> {
        let ids: Vec<String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            let mut stmt = conn.prepare("SELECT id FROM ensembles")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        ids.iter()
            .filter_map(|id| self.get_ensemble_details(id).transpose())
            .collect()
    }

    /// The ensemble `node_id` is a member of, if any — the engine's fan-out
    /// detection and the write-time "no individual member overrides" guard
    /// both go through here.
    pub fn get_ensemble_by_member_node(&self, node_id: &str) -> Result<Option<EnsembleDetails>> {
        let ensemble_id: Option<String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            conn.query_row(
                "SELECT ensemble_id FROM ensemble_members WHERE node_id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?
        };
        match ensemble_id {
            Some(id) => self.get_ensemble_details(&id),
            None => Ok(None),
        }
    }

    /// The ensemble `node_id` is the join node of, if any.
    pub fn get_ensemble_by_join_node(&self, node_id: &str) -> Result<Option<EnsembleDetails>> {
        let ensemble_id: Option<String> = {
            let conn = self
                .conn
                .lock()
                .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
            conn.query_row(
                "SELECT id FROM ensembles WHERE join_node_id = ?1",
                params![node_id],
                |row| row.get(0),
            )
            .optional()?
        };
        match ensemble_id {
            Some(id) => self.get_ensemble_details(&id),
            None => Ok(None),
        }
    }

    /// CB52: every ensemble that references `node_id` without owning it —
    /// as primary entry (`entry_from_node`), as an extra entry source (an
    /// edge from `node_id` into a member), or as an exit target
    /// (`on_pass_to`/`on_fail_to`). Returns one `(details, role)` entry per
    /// role, where role is `entry_from_node` | `entry_source` | `on_pass_to`
    /// | `on_fail_to`. Deleting such a node through the public surface is
    /// refused (`delete_graph_node_checked`); the schema additionally
    /// declares these FKs `ON DELETE RESTRICT` so a direct SQL delete can
    /// never silently cascade the ensemble row away and orphan its
    /// members/join.
    pub fn ensembles_referencing_node(
        &self,
        node_id: &str,
    ) -> Result<Vec<(EnsembleDetails, String)>> {
        let all = self.list_all_ensembles()?;
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut out = Vec::new();
        for details in &all {
            let ensemble = &details.ensemble;
            if ensemble.entry_from_node == node_id {
                out.push((details.clone(), "entry_from_node".to_string()));
            }
            if ensemble.on_pass_to == node_id {
                out.push((details.clone(), "on_pass_to".to_string()));
            }
            if let Some(on_fail_to) = &ensemble.on_fail_to {
                if on_fail_to == node_id {
                    out.push((details.clone(), "on_fail_to".to_string()));
                }
            }
            // An extra entry source: an edge from this node into a member,
            // where the node is neither the row's primary entry (already
            // reported above) nor a node the ensemble owns (a member or the
            // join — those are refused with the owned-node message instead).
            if ensemble.entry_from_node != node_id
                && !details.members.iter().any(|m| m.node_id == node_id)
                && ensemble.join_node_id != node_id
            {
                let edge_count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM graph_edges WHERE from_node = ?1 AND to_node IN (
                         SELECT node_id FROM ensemble_members WHERE ensemble_id = ?2
                     )",
                    params![node_id, ensemble.id],
                    |row| row.get(0),
                )?;
                if edge_count > 0 {
                    out.push((details.clone(), "entry_source".to_string()));
                }
            }
        }
        Ok(out)
    }

    /// CB52: every `join`-kind node with no `ensembles` row — the state left
    /// behind when an ensemble row was deleted (pre-fix: silently, via `ON
    /// DELETE CASCADE` from a referenced node) while its member/join nodes
    /// survived. Never auto-repaired (C1); reported via `graph_get`'s
    /// `orphan_join_warnings`, `graph_preflight`, and `canopy doctor`.
    pub fn list_all_orphan_join_nodes(&self) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes
             WHERE kind = 'join' AND id NOT IN (SELECT join_node_id FROM ensembles)
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map([], map_orphan_join_node_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// CB52: [`Self::list_all_orphan_join_nodes`], scoped to one graph's
    /// top-level graph.
    pub fn list_orphan_join_nodes_for_graph(&self, graph_id: &str) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes
             WHERE kind = 'join' AND id NOT IN (SELECT join_node_id FROM ensembles)
               AND graph_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![graph_id], map_orphan_join_node_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// CB52: [`Self::list_all_orphan_join_nodes`], scoped to one spec's graph.
    pub fn list_orphan_join_nodes_for_spec(&self, spec_id: &str) -> Result<Vec<GraphNode>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, spec_id, graph_id, name, kind, config, position, created_at
             FROM graph_nodes
             WHERE kind = 'join' AND id NOT IN (SELECT join_node_id FROM ensembles)
               AND spec_id = ?1
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![spec_id], map_orphan_join_node_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Replace the complete member list and rebuild entry fan-out in one
    /// transaction. Entry sources are derived from the existing edges before
    /// membership changes, so secondary sources and their conditions survive
    /// resizes.
    pub fn replace_ensemble_members(
        &self,
        ensemble_id: &str,
        members: &[EnsembleMember],
        member_nodes: &[GraphNode],
    ) -> Result<()> {
        if members.len() != member_nodes.len() {
            anyhow::bail!("Member and member-node plans must have the same length.");
        }

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (spec_id, graph_id, join_node_id, old_member_ids) =
            tx_ensemble_graph_scope(&tx, ensemble_id)?;
        let sources = tx_entry_sources(&tx, ensemble_id, &old_member_ids, &join_node_id)?;

        delete_entry_edges(&tx, &old_member_ids, &join_node_id)?;

        for old_member_id in old_member_ids.iter().skip(members.len()) {
            tx.execute(
                "DELETE FROM graph_nodes WHERE id = ?1",
                params![old_member_id],
            )?;
        }

        for (index, (member, node)) in members.iter().zip(member_nodes).enumerate() {
            if index < old_member_ids.len() {
                tx_update_existing_member(&tx, ensemble_id, member, node)?;
            } else {
                tx_insert_new_member(&tx, member, node, &join_node_id)?;
            }
        }

        let resulting_member_ids: Vec<String> = members
            .iter()
            .map(|member| member.node_id.clone())
            .collect();
        for (from_node, condition) in sources {
            insert_missing_entry_fan_out(
                &tx,
                spec_id.as_deref(),
                graph_id.as_deref(),
                &from_node,
                &condition,
                &resulting_member_ids,
            )?;
        }

        // Once members exist, the edges above carry the ensemble's entry
        // wiring, so the pending rows a crew-bound import recorded are no
        // longer the source of truth and would double-wire a later sync.
        if !members.is_empty() {
            tx.execute(
                "DELETE FROM ensemble_pending_entry_sources WHERE ensemble_id = ?1",
                params![ensemble_id],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    /// Remove a member outright: deletes its `graph_nodes` row, which cascades
    /// away its `ensemble_members` row and both wiring edges (entry, join) —
    /// every one of those FKs is `ON DELETE CASCADE`. Test-only helper since
    /// `graph_update_ensemble` moved to [`Self::replace_ensemble_members`].
    #[allow(dead_code)]
    pub fn remove_ensemble_member(&self, node_id: &str) -> Result<bool> {
        self.delete_graph_node(node_id)
    }

    /// Update an existing member's `platform`/`model`/`prompt_override`/
    /// `timeout_minutes` in place. Always sets `prompt_override`/
    /// `timeout_minutes` outright (never "leave unchanged"). The caller
    /// separately updates the member node's own `config` via
    /// [`Self::update_graph_node_details`]. Test-only helper since
    /// `graph_update_ensemble` moved to [`Self::replace_ensemble_members`].
    #[allow(dead_code)]
    pub fn update_ensemble_member(
        &self,
        ensemble_id: &str,
        node_id: &str,
        platform: &str,
        model: Option<&str>,
        prompt_override: Option<&str>,
        timeout_minutes: Option<i64>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensemble_members SET platform = ?1, model = ?2, prompt_override = ?3, timeout_minutes = ?4 WHERE ensemble_id = ?5 AND node_id = ?6",
            params![platform, model, prompt_override, timeout_minutes, ensemble_id, node_id],
        )?;
        Ok(rows > 0)
    }

    /// Delete a graph node outright — cascades away its edges (both
    /// directions) and, if it was an ensemble member, its `ensemble_members`
    /// row too. General-purpose (not ensemble-specific); callers own any
    /// "is this safe to delete" guard.
    pub fn delete_graph_node(&self, node_id: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute("DELETE FROM graph_nodes WHERE id = ?1", params![node_id])?;
        Ok(rows > 0)
    }

    /// Delete every edge from `node_id` with the given `condition` — used by
    /// `graph_update_ensemble` to retarget the join's exit routing (delete the
    /// old pass/fail edge, then insert the new one).
    pub fn delete_graph_edges_from_node_with_condition(
        &self,
        node_id: &str,
        condition: &GraphEdgeCondition,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "DELETE FROM graph_edges WHERE from_node = ?1 AND condition = ?2",
            params![node_id, condition.as_str()],
        )?;
        Ok(())
    }

    /// Delete an ensemble as one unit — every member node, the quorum node,
    /// and every edge naming any of them (entry fan-out, member→join fan-in,
    /// join exits), plus the `ensembles`/`ensemble_members` rows (via `ON
    /// DELETE CASCADE`) — in one transaction. The caller
    /// (`graph_delete_ensemble`) owns the guards: graph not running, and no
    /// other ensemble's exit chained into this one.
    pub fn delete_ensemble_unit(&self, ensemble_id: &str) -> Result<DeletedEnsembleUnit> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (_, _, join_node_id, member_ids) = tx_ensemble_graph_scope(&tx, ensemble_id)?;

        let mut all_owned = member_ids.clone();
        all_owned.push(join_node_id.clone());
        let placeholders = all_owned.iter().map(|_| "?").collect::<Vec<_>>().join(",");
        let deleted_edges: i64 = tx.query_row(
            &format!(
                "SELECT COUNT(*) FROM graph_edges WHERE from_node IN ({placeholders}) OR to_node IN ({placeholders})"
            ),
            rusqlite::params_from_iter(all_owned.iter().chain(all_owned.iter())),
            |row| row.get(0),
        )?;

        for node_id in &all_owned {
            tx.execute("DELETE FROM graph_nodes WHERE id = ?1", params![node_id])?;
        }
        // The `ensembles` row is typically already gone here: its
        // `join_node_id` foreign key is `ON DELETE CASCADE`, so deleting the
        // owned nodes above cascades to the row itself (existence was verified
        // up front by `tx_ensemble_graph_scope`). The entry/exit FKs
        // (`entry_from_node`/`on_pass_to`/`on_fail_to`) are CB52 `ON DELETE
        // RESTRICT` — they name nodes this unit does not own, so they never
        // fire here. Delete explicitly anyway for the case where a foreign
        // key was deferred — affecting zero rows is fine.
        tx.execute("DELETE FROM ensembles WHERE id = ?1", params![ensemble_id])?;
        tx.commit()?;
        Ok(DeletedEnsembleUnit {
            ensemble_id: ensemble_id.to_string(),
            member_node_ids: member_ids,
            join_node_id,
            deleted_edge_count: deleted_edges,
        })
    }
}

fn map_ensemble_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Ensemble> {
    let entry_condition =
        GraphEdgeCondition::from_str(&row.get::<_, String>(7)?).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                7,
                rusqlite::types::Type::Text,
                Box::new(IoError::new(
                    ErrorKind::InvalidData,
                    "Invalid ensemble entry_condition",
                )),
            )
        })?;
    let kind_str: String = row.get(17)?;
    let kind = EnsembleKind::from_str(&kind_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            17,
            rusqlite::types::Type::Text,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                format!("Invalid ensemble kind: {kind_str}"),
            )),
        )
    })?;
    Ok(Ensemble {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        graph_id: row.get(2)?,
        name: row.get(3)?,
        prompt_template: row.get(4)?,
        join_node_id: row.get(5)?,
        entry_from_node: row.get(6)?,
        entry_condition,
        min_pass: row.get(8)?,
        straggler_timeout_minutes: row.get(9)?,
        quorum_grace_minutes: row.get(10)?,
        timeout_minutes: row.get(11)?,
        infra_retry_limit: row.get(12)?,
        infra_crash_max_seconds: row.get(13)?,
        infra_backoff_seconds: row.get(14)?,
        on_pass_to: row.get(15)?,
        on_fail_to: row.get(16)?,
        kind,
        round_robin_index: row.get(18)?,
        commit_rights: row.get(19)?,
        mutex: decode_ensemble_mutex(row.get::<_, Option<String>>(24).ok().flatten()),
        created_at: from_timestamp(row.get(20)?)?,
        crew_id: row.get(21)?,
        crew_name: row.get(22)?,
        crew_version_applied: row.get(23)?,
    })
}

fn map_ensemble_member_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnsembleMember> {
    Ok(EnsembleMember {
        ensemble_id: row.get(0)?,
        node_id: row.get(1)?,
        position: row.get(2)?,
        platform: row.get(3)?,
        model: row.get(4)?,
        prompt_override: row.get(5)?,
        timeout_minutes: row.get(6)?,
        infra_retry_limit: row.get(7)?,
        infra_crash_max_seconds: row.get(8)?,
        infra_backoff_seconds: row.get(9)?,
    })
}

/// CB52: map a `graph_nodes` row for the orphan-join helpers — same column
/// order as `map_graph_node_row` (`id, spec_id, graph_id, name, kind,
/// config, position, created_at`), duplicated here because that mapper is
/// private to `db::graphs`.
fn map_orphan_join_node_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let kind = GraphNodeKind::from_str(&row.get::<_, String>(4)?).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                "Invalid graph node kind",
            )),
        )
    })?;
    let config_raw: String = row.get(5)?;
    let config = serde_json::from_str(&config_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(GraphNode {
        id: row.get(0)?,
        spec_id: row.get(1)?,
        graph_id: row.get(2)?,
        name: row.get(3)?,
        kind,
        config,
        position: row.get(6)?,
        created_at: from_timestamp(row.get(7)?)?,
    })
}

fn from_timestamp(value: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp(value, 0).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(IoError::new(
                ErrorKind::InvalidData,
                "Invalid timestamp value",
            )),
        )
    })
}

/// What [`Database::delete_ensemble_unit`] removed — member and quorum ids
/// for the response, plus how many wiring edges went with them.
#[derive(Debug)]
pub struct DeletedEnsembleUnit {
    pub ensemble_id: String,
    pub member_node_ids: Vec<String>,
    pub join_node_id: String,
    pub deleted_edge_count: i64,
}
#[cfg(test)]
mod tests;
