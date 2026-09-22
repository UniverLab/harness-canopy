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
            "INSERT INTO ensembles (id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, on_pass_to, on_fail_to, kind, round_robin_index, commit_rights, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
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
                &ensemble.on_pass_to,
                &ensemble.on_fail_to,
                ensemble.kind.as_str(),
                ensemble.round_robin_index,
                ensemble.commit_rights,
                ensemble.created_at.timestamp(),
            ],
        )?;

        for member in members {
            tx.execute(
                "INSERT INTO ensemble_members (ensemble_id, node_id, position, platform, model, prompt_override, timeout_minutes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    &member.ensemble_id,
                    &member.node_id,
                    member.position,
                    &member.platform,
                    &member.model,
                    &member.prompt_override,
                    &member.timeout_minutes,
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
            "SELECT id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, on_pass_to, on_fail_to, kind, round_robin_index, commit_rights, created_at
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
            "SELECT ensemble_id, node_id, position, platform, model, prompt_override, timeout_minutes
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
    pub fn update_ensemble_join_config(
        &self,
        ensemble_id: &str,
        min_pass: Option<i64>,
        straggler_timeout_minutes: Option<Option<i64>>,
        quorum_grace_minutes: Option<Option<i64>>,
        timeout_minutes: Option<i64>,
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
                 timeout_minutes = COALESCE(?6, timeout_minutes)
             WHERE id = ?7",
            params![
                min_pass,
                straggler_timeout_minutes.map(|_| 1),
                straggler_timeout_minutes.flatten(),
                quorum_grace_minutes.map(|_| 1),
                quorum_grace_minutes.flatten(),
                timeout_minutes,
                ensemble_id,
            ],
        )?;
        Ok(rows > 0)
    }

    /// Update the join's exit routing (`on_pass_to`/`on_fail_to`) on the
    /// `ensembles` row. Recreating the underlying `graph_edges` rows is the
    /// caller's job (`graph_update_ensemble`) — this only updates the
    /// source-of-truth columns.
    pub fn update_ensemble_exit_wiring(
        &self,
        ensemble_id: &str,
        on_pass_to: Option<&str>,
        on_fail_to: Option<Option<&str>>,
    ) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "UPDATE ensembles
             SET on_pass_to = COALESCE(?1, on_pass_to),
                 on_fail_to = CASE WHEN ?2 IS NULL THEN on_fail_to ELSE ?3 END
             WHERE id = ?4",
            params![
                on_pass_to,
                on_fail_to.map(|_| 1),
                on_fail_to.flatten(),
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
                tx.execute(
                    "UPDATE graph_nodes
                     SET spec_id = ?1, graph_id = ?2, name = ?3, kind = ?4,
                         config = ?5, position = ?6, created_at = ?7
                     WHERE id = ?8",
                    params![
                        &node.spec_id,
                        &node.graph_id,
                        &node.name,
                        node.kind.as_str(),
                        serde_json::to_string(&node.config)?,
                        node.position,
                        node.created_at.timestamp(),
                        &member.node_id,
                    ],
                )?;
                tx.execute(
                    "UPDATE ensemble_members
                     SET node_id = ?1, position = ?2, platform = ?3, model = ?4,
                         prompt_override = ?5, timeout_minutes = ?6
                     WHERE ensemble_id = ?7 AND position = ?8",
                    params![
                        &member.node_id,
                        member.position,
                        &member.platform,
                        &member.model,
                        &member.prompt_override,
                        &member.timeout_minutes,
                        ensemble_id,
                        member.position,
                    ],
                )?;
            } else {
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
                tx.execute(
                    "INSERT INTO ensemble_members
                     (ensemble_id, node_id, position, platform, model, prompt_override, timeout_minutes)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        &member.ensemble_id,
                        &member.node_id,
                        member.position,
                        &member.platform,
                        &member.model,
                        &member.prompt_override,
                        &member.timeout_minutes,
                    ],
                )?;
                tx.execute(
                    "INSERT INTO graph_edges
                     (id, spec_id, graph_id, from_node, to_node, condition)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        &node.spec_id,
                        &node.graph_id,
                        &node.id,
                        &join_node_id,
                        GraphEdgeCondition::Always.as_str(),
                    ],
                )?;
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

    /// Replace an ensemble's entry wiring with a single new source, atomically:
    /// every current entry edge (into a member from outside the unit) is
    /// deleted and a fresh fan-out from `new_from_node` to every member is
    /// inserted, and the `ensembles` row's `entry_from_node`/`entry_condition`
    /// follows — all in one transaction, so a failure leaves the previous
    /// edges intact rather than an ensemble with no entry. The caller
    /// (`graph_update_ensemble`) owns validation: `new_from_node` exists in
    /// the ensemble's graph and is not ensemble-owned.
    pub fn rewire_ensemble_entry(
        &self,
        ensemble_id: &str,
        new_from_node: &str,
        new_condition: &GraphEdgeCondition,
    ) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (spec_id, graph_id, join_node_id, member_ids) =
            tx_ensemble_graph_scope(&tx, ensemble_id)?;

        delete_entry_edges(&tx, &member_ids, &join_node_id)?;
        insert_entry_fan_out(
            &tx,
            spec_id.as_deref(),
            graph_id.as_deref(),
            new_from_node,
            new_condition,
            &member_ids,
        )?;
        tx.execute(
            "UPDATE ensembles SET entry_from_node = ?1, entry_condition = ?2 WHERE id = ?3",
            params![new_from_node, new_condition.as_str(), ensemble_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Add one more entry source to an ensemble, atomically: entry edges from
    /// `from_node` to every member are inserted alongside the existing ones,
    /// so the ensemble can be entered from several places with no relay node.
    /// Existing sources are completed idempotently; a partial source gets the
    /// missing edges for each condition already represented by its edges.
    /// The `ensembles` row is untouched — it keeps naming the primary entry.
    pub fn add_ensemble_entry_source(
        &self,
        ensemble_id: &str,
        from_node: &str,
        condition: &GraphEdgeCondition,
    ) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (spec_id, graph_id, join_node_id, member_ids) =
            tx_ensemble_graph_scope(&tx, ensemble_id)?;
        let existing_sources = tx_entry_sources(&tx, ensemble_id, &member_ids, &join_node_id)?;
        let conditions: Vec<GraphEdgeCondition> = existing_sources
            .iter()
            .filter(|(node, _)| node == from_node)
            .map(|(_, condition)| condition.clone())
            .collect();
        if conditions.is_empty() {
            insert_missing_entry_fan_out(
                &tx,
                spec_id.as_deref(),
                graph_id.as_deref(),
                from_node,
                condition,
                &member_ids,
            )?;
        } else {
            for existing_condition in conditions {
                insert_missing_entry_fan_out(
                    &tx,
                    spec_id.as_deref(),
                    graph_id.as_deref(),
                    from_node,
                    &existing_condition,
                    &member_ids,
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Detach one entry source from an ensemble, atomically: every entry edge
    /// from `from_node` to a member is deleted. Refuses the ensemble's last
    /// entry source (an ensemble always keeps at least one entry), and when
    /// the detached source was the row's primary entry promotes another
    /// remaining source so the row never names a source with no edges.
    /// CB52 FR3: this repoint is the load-bearing behavior — `entry_from_node`
    /// is never left dangling when its source is detached via
    /// `remove_entry_from`.
    pub fn remove_ensemble_entry_source(&self, ensemble_id: &str, from_node: &str) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (_, _, join_node_id, member_ids) = tx_ensemble_graph_scope(&tx, ensemble_id)?;

        let sources = tx_entry_sources(&tx, ensemble_id, &member_ids, &join_node_id)?;
        if !sources.iter().any(|(node, _)| node == from_node) {
            anyhow::bail!("Node '{from_node}' is not an entry source of ensemble '{ensemble_id}'.");
        }
        if sources.len() < 2 {
            anyhow::bail!(
                "Cannot detach '{from_node}': it is the last entry source of ensemble '{ensemble_id}'. Rewire it with from_node instead, or delete the whole unit with graph_delete_ensemble."
            );
        }

        tx.execute(
            "DELETE FROM graph_edges WHERE from_node = ?1 AND to_node IN (
                 SELECT node_id FROM ensemble_members WHERE ensemble_id = ?2
             )",
            params![from_node, ensemble_id],
        )?;

        let primary: String = tx.query_row(
            "SELECT entry_from_node FROM ensembles WHERE id = ?1",
            params![ensemble_id],
            |row| row.get(0),
        )?;
        if primary == from_node {
            let (next_node, next_condition) = sources
                .into_iter()
                .find(|(node, _)| node != from_node)
                .expect("at least two sources were verified above");
            tx.execute(
                "UPDATE ensembles SET entry_from_node = ?1, entry_condition = ?2 WHERE id = ?3",
                params![next_node, next_condition.as_str(), ensemble_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Retarget one of the quorum's exit wirings (`pass` or `fail`), atomically:
    /// every existing edge from the join with `condition` is deleted and a
    /// fresh edge per `to_nodes` is inserted, and the `ensembles` row follows
    /// in the same transaction. `to_nodes` has one entry for a plain node
    /// target, or every member of a target ensemble when chaining ensembles
    /// (no intermediate node) — with `row_target` naming the value stored in
    /// the row (`on_pass_to`, or `on_fail_to`): the node id, or the target
    /// ensemble's quorum id when chained. `None` clears `on_fail_to` (a pass
    /// exit always has a target).
    pub fn retarget_ensemble_exit(
        &self,
        ensemble_id: &str,
        condition: &GraphEdgeCondition,
        to_nodes: &[String],
        row_target: Option<&str>,
    ) -> Result<()> {
        if to_nodes.is_empty() {
            anyhow::bail!("Cannot retarget an ensemble exit to zero targets.");
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;
        let (spec_id, graph_id, join_node_id, _) = tx_ensemble_graph_scope(&tx, ensemble_id)?;

        tx.execute(
            "DELETE FROM graph_edges WHERE from_node = ?1 AND condition = ?2",
            params![join_node_id, condition.as_str()],
        )?;
        for to_node in to_nodes {
            tx.execute(
                "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    spec_id,
                    graph_id,
                    join_node_id,
                    to_node,
                    condition.as_str(),
                ],
            )?;
        }
        match condition {
            GraphEdgeCondition::Pass => {
                let Some(target) = row_target else {
                    anyhow::bail!("A pass exit always has a target.");
                };
                tx.execute(
                    "UPDATE ensembles SET on_pass_to = ?1 WHERE id = ?2",
                    params![target, ensemble_id],
                )?;
            }
            GraphEdgeCondition::Fail => {
                tx.execute(
                    "UPDATE ensembles SET on_fail_to = ?1 WHERE id = ?2",
                    params![row_target, ensemble_id],
                )?;
            }
            _ => {
                anyhow::bail!("Ensemble exits only route on pass or fail.");
            }
        }
        tx.commit()?;
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

    pub fn insert_ensemble_blueprint(&self, blueprint: &EnsembleBlueprint) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "INSERT INTO ensemble_blueprints (id, name, prompt_template, members, min_pass, quorum_grace_minutes, builtin, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &blueprint.id,
                &blueprint.name,
                &blueprint.prompt_template,
                serde_json::to_string(&blueprint.members)?,
                blueprint.min_pass,
                blueprint.quorum_grace_minutes,
                blueprint.builtin,
                blueprint.created_at.timestamp(),
            ],
        )?;
        Ok(())
    }

    pub fn get_ensemble_blueprint_by_name(&self, name: &str) -> Result<Option<EnsembleBlueprint>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt_template, members, min_pass, quorum_grace_minutes, builtin, created_at
             FROM ensemble_blueprints WHERE name = ?1",
        )?;
        stmt.query_row(params![name], map_ensemble_blueprint_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn list_ensemble_blueprints(&self) -> Result<Vec<EnsembleBlueprint>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT id, name, prompt_template, members, min_pass, quorum_grace_minutes, builtin, created_at
             FROM ensemble_blueprints ORDER BY builtin DESC, name ASC",
        )?;
        let rows = stmt.query_map([], map_ensemble_blueprint_row)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Delete an ensemble blueprint by name. Mirrors
    /// [`Self::delete_blueprint_by_name`]: callers own any builtin guard —
    /// this performs none itself.
    pub fn delete_ensemble_blueprint_by_name(&self, name: &str) -> Result<bool> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let rows = conn.execute(
            "DELETE FROM ensemble_blueprints WHERE name = ?1",
            params![name],
        )?;
        Ok(rows > 0)
    }

    /// Overwrite a builtin ensemble blueprint row's `prompt_template`/
    /// `members`/`min_pass` in place — the ensemble mirror of
    /// [`Self::update_builtin_blueprint`], reconciling a stale builtin shape
    /// (e.g. a hardcoded model identity from before this field was dropped)
    /// with the current spec. Only ever called from
    /// [`Self::seed_builtin_ensemble_blueprints`].
    fn update_builtin_ensemble_blueprint(
        &self,
        id: &str,
        prompt_template: &str,
        members: &[EnsembleMemberSpec],
        min_pass: Option<i64>,
        quorum_grace_minutes: Option<i64>,
    ) -> Result<()> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        conn.execute(
            "UPDATE ensemble_blueprints SET prompt_template = ?1, members = ?2, min_pass = ?3, quorum_grace_minutes = ?4 WHERE id = ?5",
            params![
                prompt_template,
                serde_json::to_string(members)?,
                min_pass,
                quorum_grace_minutes,
                id,
            ],
        )?;
        Ok(())
    }

    /// Seed the builtin ensemble blueprints (currently just
    /// "ensemble-proposers") and keep already-seeded builtin rows in sync
    /// with the current `builtin_ensemble_blueprint_specs()` shape —
    /// idempotent and safe on every daemon startup, mirroring
    /// [`Self::seed_builtin_blueprints`]'s stale-name-delete /
    /// drifted-config-overwrite / leave-custom-rows-alone behavior exactly.
    pub fn seed_builtin_ensemble_blueprints(&self) -> Result<()> {
        let specs = builtin_ensemble_blueprint_specs();
        let current_names: std::collections::HashSet<&str> =
            specs.iter().map(|(name, ..)| *name).collect();

        for existing in self.list_ensemble_blueprints()? {
            if existing.builtin && !current_names.contains(existing.name.as_str()) {
                self.delete_ensemble_blueprint_by_name(&existing.name)?;
            }
        }

        for (name, prompt_template, members, min_pass) in specs {
            let members: Vec<EnsembleMemberSpec> = members
                .into_iter()
                .map(|(platform, model, prompt_override)| {
                    (
                        platform.to_string(),
                        model.map(str::to_string),
                        prompt_override.map(str::to_string),
                        None,
                    )
                })
                .collect();

            match self.get_ensemble_blueprint_by_name(name)? {
                Some(existing) if existing.builtin => {
                    if existing.prompt_template != prompt_template
                        || existing.members != members
                        || existing.min_pass != min_pass
                        || existing.quorum_grace_minutes.is_some()
                    {
                        self.update_builtin_ensemble_blueprint(
                            &existing.id,
                            prompt_template,
                            &members,
                            min_pass,
                            None,
                        )?;
                    }
                }
                // Name already claimed by a custom ensemble blueprint — leave it be.
                Some(_) => {}
                None => {
                    self.insert_ensemble_blueprint(&EnsembleBlueprint {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: name.to_string(),
                        prompt_template: prompt_template.to_string(),
                        members,
                        min_pass,
                        quorum_grace_minutes: None,
                        builtin: true,
                        created_at: Utc::now(),
                    })?;
                }
            }
        }
        Ok(())
    }
}

/// Parse a blueprint row's stored `members` JSON, tolerating the
/// pre-prompt-override 2-element `[platform, model]` shape, the
/// pre-member-timeout 3-element `[platform, model, prompt_override]` shape
/// (rows seeded before this field existed — including a builtin blueprint
/// from an older daemon), and the current 4-element
/// `[platform, model, prompt_override, timeout_minutes]` shape, so an
/// upgrade never needs a data migration for blueprints already in the DB.
fn parse_ensemble_blueprint_members(
    raw: &str,
) -> Result<Vec<EnsembleMemberSpec>, serde_json::Error> {
    let rows: Vec<serde_json::Value> = serde_json::from_str(raw)?;
    Ok(rows
        .into_iter()
        .map(|entry| {
            let platform = entry
                .get(0)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            let model = entry
                .get(1)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let prompt_override = entry
                .get(2)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let timeout_minutes = entry.get(3).and_then(serde_json::Value::as_i64);
            (platform, model, prompt_override, timeout_minutes)
        })
        .collect())
}

fn map_ensemble_blueprint_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EnsembleBlueprint> {
    let members_raw: String = row.get(3)?;
    let members = parse_ensemble_blueprint_members(&members_raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(3, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(EnsembleBlueprint {
        id: row.get(0)?,
        name: row.get(1)?,
        prompt_template: row.get(2)?,
        members,
        min_pass: row.get(4)?,
        quorum_grace_minutes: row.get(5)?,
        builtin: row.get(6)?,
        created_at: from_timestamp(row.get(7)?)?,
    })
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
    let kind_str: String = row.get(14)?;
    let kind = EnsembleKind::from_str(&kind_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            14,
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
        on_pass_to: row.get(12)?,
        on_fail_to: row.get(13)?,
        kind,
        round_robin_index: row.get(15)?,
        commit_rights: row.get(16)?,
        created_at: from_timestamp(row.get(17)?)?,
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

/// Distinct `(from_node, condition)` entry wirings into an ensemble's members,
/// derived from the graph's edges rather than the `ensembles` row: every edge
/// into a member from outside the unit (not from a fellow member, not from
/// the quorum). The edges ARE the wiring — the row's
/// `entry_from_node`/`entry_condition` simply names the primary (first)
/// source — so multi-source entry needs no extra bookkeeping column, and the
/// engine's fan-out detection (which reads these same edges) works unchanged
/// no matter how many sources there are.
pub fn ensemble_entry_sources(
    members: &[EnsembleMember],
    join_node_id: &str,
    edges: &[GraphEdge],
) -> Vec<(String, GraphEdgeCondition)> {
    let member_ids: std::collections::HashSet<&str> = members
        .iter()
        .map(|member| member.node_id.as_str())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut sources = Vec::new();
    for edge in edges {
        if !member_ids.contains(edge.to_node.as_str()) {
            continue;
        }
        if member_ids.contains(edge.from_node.as_str()) || edge.from_node == join_node_id {
            continue;
        }
        if seen.insert((edge.from_node.clone(), edge.condition.clone())) {
            sources.push((edge.from_node.clone(), edge.condition.clone()));
        }
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.as_str().cmp(b.1.as_str())));
    sources
}

/// The graph scope plus unit membership every entry/exit transaction needs:
/// `(spec_id, graph_id, join_node_id, member_ids in position order)`. Bails
/// when the ensemble does not exist.
type EnsembleGraphScope = (Option<String>, Option<String>, String, Vec<String>);

fn tx_ensemble_graph_scope(
    tx: &rusqlite::Transaction<'_>,
    ensemble_id: &str,
) -> Result<EnsembleGraphScope> {
    let (spec_id, graph_id, join_node_id): (Option<String>, Option<String>, String) = tx
        .query_row(
            "SELECT spec_id, graph_id, join_node_id FROM ensembles WHERE id = ?1",
            params![ensemble_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("Ensemble '{ensemble_id}' not found."))?;
    let mut stmt = tx.prepare(
        "SELECT node_id FROM ensemble_members WHERE ensemble_id = ?1 ORDER BY position ASC",
    )?;
    let member_ids = stmt
        .query_map(params![ensemble_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((spec_id, graph_id, join_node_id, member_ids))
}

/// Entry sources read inside a transaction — same shape as
/// [`ensemble_entry_sources`], but querying the rows the transaction itself
/// sees.
fn tx_entry_sources(
    tx: &rusqlite::Transaction<'_>,
    ensemble_id: &str,
    member_ids: &[String],
    join_node_id: &str,
) -> Result<Vec<(String, GraphEdgeCondition)>> {
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut stmt = tx.prepare(
        "SELECT DISTINCT from_node, condition FROM graph_edges WHERE to_node IN (
             SELECT node_id FROM ensemble_members WHERE ensemble_id = ?1
         )",
    )?;
    let rows = stmt.query_map(params![ensemble_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (from_node, condition_raw) = row?;
        if member_ids.iter().any(|id| id == &from_node) || from_node == join_node_id {
            continue;
        }
        let Some(condition) = GraphEdgeCondition::from_str(&condition_raw) else {
            continue;
        };
        if seen.insert((from_node.clone(), condition.clone())) {
            sources.push((from_node, condition));
        }
    }
    sources.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.as_str().cmp(b.1.as_str())));
    Ok(sources)
}

/// Delete every entry edge into `member_ids` from outside the unit (same
/// predicate as [`ensemble_entry_sources`]).
fn delete_entry_edges(
    tx: &rusqlite::Transaction<'_>,
    member_ids: &[String],
    join_node_id: &str,
) -> Result<()> {
    if member_ids.is_empty() {
        return Ok(());
    }
    let placeholders = member_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "DELETE FROM graph_edges WHERE to_node IN ({placeholders})
         AND from_node NOT IN ({placeholders}) AND from_node <> ?"
    );
    let mut params: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(member_ids.len() * 2 + 1);
    for id in member_ids {
        params.push(id);
    }
    for id in member_ids {
        params.push(id);
    }
    params.push(&join_node_id);
    tx.execute(&sql, params.as_slice())?;
    Ok(())
}

/// Insert one entry edge per member from `from_node` with `condition`.
fn insert_entry_fan_out(
    tx: &rusqlite::Transaction<'_>,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    from_node: &str,
    condition: &GraphEdgeCondition,
    member_ids: &[String],
) -> Result<()> {
    for member_id in member_ids {
        tx.execute(
            "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid::Uuid::new_v4().to_string(),
                spec_id,
                graph_id,
                from_node,
                member_id,
                condition.as_str(),
            ],
        )?;
    }

    Ok(())
}

fn insert_missing_entry_fan_out(
    tx: &rusqlite::Transaction<'_>,
    spec_id: Option<&str>,
    graph_id: Option<&str>,
    from_node: &str,
    condition: &GraphEdgeCondition,
    member_ids: &[String],
) -> Result<()> {
    for member_id in member_ids {
        tx.execute(
            "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6
             WHERE NOT EXISTS (
                 SELECT 1 FROM graph_edges
                 WHERE from_node = ?4 AND to_node = ?5 AND condition = ?6
             )",
            params![
                uuid::Uuid::new_v4().to_string(),
                spec_id,
                graph_id,
                from_node,
                member_id,
                condition.as_str(),
            ],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{GraphNodeKind, GraphSpec, GraphSpecStatus};
    use tempfile::{tempdir, NamedTempFile};

    fn test_db() -> Database {
        let tmp = NamedTempFile::new().expect("create temp file");
        let path = tmp.path().to_path_buf();
        std::mem::forget(tmp);
        Database::new(&path).expect("create test db")
    }

    fn insert_spec(db: &Database, id: &str) {
        db.insert_graph_spec(&GraphSpec {
            id: id.to_string(),
            graph_id: None,
            name: id.to_string(),
            description: Some("Do the thing".to_string()),
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
        })
        .unwrap();
    }

    /// The DB-layer half of the "one call -> N+1 nodes" contract: given the
    /// exact node/edge/ensemble shape `graph_add_ensemble` assembles for 3
    /// members, one `insert_ensemble_unit` transaction must leave behind 1
    /// join node + 3 member nodes (4 new `graph_nodes` rows total), the
    /// entry fan-out edge per member, the member->join fan-in edge per
    /// member, and the join's own pass edge — all in one shot.
    #[test]
    fn insert_ensemble_unit_creates_join_and_member_nodes_with_correct_wiring() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();

        let now = Utc::now();
        let member_ids = ["m1", "m2", "m3"];
        let member_nodes: Vec<GraphNode> = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| GraphNode {
                id: id.to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: format!("member-{}", i + 1),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({
                    "platform": "openrouter",
                    "prompt_template": "draft it",
                }),
                position: 2 + i as i64,
                created_at: now,
            })
            .collect();
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 5,
            created_at: now,
        };
        let mut edges = Vec::new();
        for id in &member_ids {
            edges.push(GraphEdge {
                id: format!("kickoff->{id}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: id.to_string(),
                condition: GraphEdgeCondition::Always,
            });
            edges.push(GraphEdge {
                id: format!("{id}->join1"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: id.to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            });
        }
        edges.push(GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        });
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 3,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members: Vec<EnsembleMember> = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: id.to_string(),
                position: i as i64,
                platform: "openrouter".to_string(),
                model: Some(format!("model-{i}")),
                prompt_override: None,
                timeout_minutes: None,
            })
            .collect();

        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        // 1 call -> N+1 new graph nodes (join + 3 members), on top of the 2
        // pre-existing (kickoff, arbiter).
        let all_nodes = db.list_graph_nodes("spec-1").unwrap();
        assert_eq!(all_nodes.len(), 6);
        assert!(db.get_graph_node("join1").unwrap().is_some());
        for id in &member_ids {
            assert!(db.get_graph_node(id).unwrap().is_some());
        }

        let all_edges = db.list_graph_edges("spec-1").unwrap();
        // 2 wiring edges per member (entry + join) + 1 join exit edge.
        assert_eq!(all_edges.len(), member_ids.len() * 2 + 1);

        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(details.members.len(), 3);
        // Deterministic member order (position ASC), matching insertion order.
        assert_eq!(
            details
                .members
                .iter()
                .map(|m| m.node_id.as_str())
                .collect::<Vec<_>>(),
            member_ids
        );
    }

    /// CB52 fixture: a spec-scoped ensemble with two entry sources
    /// (`kickoff` primary, `alt_entry` extra), `on_pass_to = arbiter`,
    /// `on_fail_to = fallback`, plus an unrelated node.
    fn insert_cb52_ensemble(db: &Database) {
        insert_spec(db, "spec-1");
        for (id, kind, position) in [
            ("kickoff", GraphNodeKind::Check, 1),
            ("alt_entry", GraphNodeKind::Check, 2),
            ("arbiter", GraphNodeKind::Agent, 6),
            ("fallback", GraphNodeKind::Agent, 7),
            ("unrelated", GraphNodeKind::Agent, 8),
        ] {
            db.insert_graph_node(&GraphNode {
                id: id.to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: id.to_string(),
                kind,
                config: serde_json::json!({}),
                position,
                created_at: Utc::now(),
            })
            .unwrap();
        }
        let now = Utc::now();
        let member_ids = ["m1", "m2"];
        let member_nodes: Vec<GraphNode> = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| GraphNode {
                id: id.to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: format!("member-{}", i + 1),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({
                    "platform": "openrouter",
                    "prompt_template": "draft it",
                }),
                position: 3 + i as i64,
                created_at: now,
            })
            .collect();
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 5,
            created_at: now,
        };
        let mut edges = Vec::new();
        for entry in ["kickoff", "alt_entry"] {
            for id in &member_ids {
                edges.push(GraphEdge {
                    id: format!("{entry}->{id}"),
                    spec_id: Some("spec-1".to_string()),
                    graph_id: None,
                    from_node: entry.to_string(),
                    to_node: id.to_string(),
                    condition: GraphEdgeCondition::Always,
                });
            }
        }
        for id in &member_ids {
            edges.push(GraphEdge {
                id: format!("{id}->join1"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: id.to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            });
        }
        edges.push(GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        });
        edges.push(GraphEdge {
            id: "join1->fallback".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "fallback".to_string(),
            condition: GraphEdgeCondition::Fail,
        });
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: Some("fallback".to_string()),
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members: Vec<EnsembleMember> = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: id.to_string(),
                position: i as i64,
                platform: "openrouter".to_string(),
                model: Some(format!("model-{i}")),
                prompt_override: None,
                timeout_minutes: None,
            })
            .collect();
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();
    }

    #[test]
    fn replace_ensemble_members_preserves_all_entry_sources_on_resize() {
        let db = test_db();
        insert_cb52_ensemble(&db);
        let now = Utc::now();
        let mut members = db.list_ensemble_members("ens1").unwrap();
        let mut nodes: Vec<GraphNode> = members
            .iter()
            .map(|member| db.get_graph_node(&member.node_id).unwrap().unwrap())
            .collect();
        for (position, id) in [(2, "m3"), (3, "m4")] {
            members.push(EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: id.to_string(),
                position,
                platform: "openrouter".to_string(),
                model: Some(format!("model-{position}")),
                prompt_override: None,
                timeout_minutes: None,
            });
            nodes.push(GraphNode {
                id: id.to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: format!("member-{}", position + 1),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({
                    "platform": "openrouter",
                    "prompt_template": "draft it",
                }),
                position: 9 + position,
                created_at: now,
            });
        }

        db.replace_ensemble_members("ens1", &members, &nodes)
            .unwrap();
        let four_ids: std::collections::HashSet<_> = members
            .iter()
            .map(|member| member.node_id.as_str())
            .collect();
        let entry_edges = db
            .list_graph_edges("spec-1")
            .unwrap()
            .into_iter()
            .filter(|edge| {
                ["kickoff", "alt_entry"].contains(&edge.from_node.as_str())
                    && four_ids.contains(edge.to_node.as_str())
            })
            .count();
        assert_eq!(entry_edges, 8);

        let shrunk_members = members[..3].to_vec();
        let shrunk_nodes = nodes[..3].to_vec();
        db.replace_ensemble_members("ens1", &shrunk_members, &shrunk_nodes)
            .unwrap();
        let three_ids: std::collections::HashSet<_> = shrunk_members
            .iter()
            .map(|member| member.node_id.as_str())
            .collect();
        let entry_edges = db
            .list_graph_edges("spec-1")
            .unwrap()
            .into_iter()
            .filter(|edge| {
                ["kickoff", "alt_entry"].contains(&edge.from_node.as_str())
                    && three_ids.contains(edge.to_node.as_str())
            })
            .count();
        assert_eq!(entry_edges, 6);
        assert!(db.get_graph_node("m4").unwrap().is_none());
    }

    #[test]
    fn replace_ensemble_members_rolls_back_on_insert_failure() {
        let db = test_db();
        insert_cb52_ensemble(&db);
        let before_members = db.list_ensemble_members("ens1").unwrap();
        let before_edges = db.list_graph_edges("spec-1").unwrap();
        let now = Utc::now();
        let mut members = before_members.clone();
        members.push(EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "kickoff".to_string(),
            position: 2,
            platform: "openrouter".to_string(),
            model: Some("bad-member".to_string()),
            prompt_override: None,
            timeout_minutes: None,
        });
        let mut nodes: Vec<GraphNode> = before_members
            .iter()
            .map(|member| db.get_graph_node(&member.node_id).unwrap().unwrap())
            .collect();
        nodes.push(GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "duplicate".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 99,
            created_at: now,
        });

        assert!(db
            .replace_ensemble_members("ens1", &members, &nodes)
            .is_err());
        let member_ids = |members: Vec<EnsembleMember>| {
            members
                .into_iter()
                .map(|member| (member.node_id, member.position))
                .collect::<Vec<_>>()
        };
        let edge_ids = |edges: Vec<GraphEdge>| {
            edges
                .into_iter()
                .map(|edge| (edge.id, edge.from_node, edge.to_node, edge.condition))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            member_ids(db.list_ensemble_members("ens1").unwrap()),
            member_ids(before_members)
        );
        assert_eq!(
            edge_ids(db.list_graph_edges("spec-1").unwrap()),
            edge_ids(before_edges)
        );
    }

    #[test]
    fn add_ensemble_entry_source_repairs_partial_source_idempotently() {
        let db = test_db();
        insert_cb52_ensemble(&db);
        let missing = db
            .list_graph_edges("spec-1")
            .unwrap()
            .into_iter()
            .find(|edge| edge.from_node == "alt_entry" && edge.to_node == "m2")
            .unwrap();
        assert!(db.delete_graph_edge(&missing.id).unwrap());

        db.add_ensemble_entry_source("ens1", "alt_entry", &GraphEdgeCondition::Always)
            .unwrap();
        db.add_ensemble_entry_source("ens1", "alt_entry", &GraphEdgeCondition::Pass)
            .unwrap();
        let repaired = db
            .list_graph_edges("spec-1")
            .unwrap()
            .into_iter()
            .filter(|edge| edge.from_node == "alt_entry")
            .collect::<Vec<_>>();
        assert_eq!(repaired.len(), 2);
        assert!(repaired.iter().any(|edge| edge.to_node == "m2"));
    }

    /// CB52 FR1 (DB half): every reference role is reported — primary entry,
    /// extra entry source, pass exit, fail exit — and nothing else is.
    /// A test that only covered one role would still pass with the other
    /// three roles' checks removed.
    #[test]
    fn ensembles_referencing_node_reports_all_four_roles() {
        let db = test_db();
        insert_cb52_ensemble(&db);

        let roles_for = |node_id: &str| {
            let mut roles: Vec<String> = db
                .ensembles_referencing_node(node_id)
                .unwrap()
                .into_iter()
                .map(|(_, role)| role)
                .collect();
            roles.sort();
            roles
        };
        assert_eq!(roles_for("kickoff"), vec!["entry_from_node"]);
        assert_eq!(roles_for("alt_entry"), vec!["entry_source"]);
        assert_eq!(roles_for("arbiter"), vec!["on_pass_to"]);
        assert_eq!(roles_for("fallback"), vec!["on_fail_to"]);
        assert!(roles_for("unrelated").is_empty());
        // Owned nodes are not "references": the member/join guard owns them.
        assert!(roles_for("m1").is_empty());
        assert!(roles_for("join1").is_empty());

        // Each hit names the ensemble it belongs to.
        let hits = db.ensembles_referencing_node("kickoff").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.ensemble.id, "ens1");
        assert_eq!(hits[0].0.ensemble.name, "Proposers");
    }

    /// CB52 FR4 (DB half): only a join node with no ensemble row is listed —
    /// a healthy ensemble's own join never is — and the per-graph/per-spec
    /// scopes agree with the global one.
    #[test]
    fn orphan_join_detection_lists_only_unowned_joins() {
        let db = test_db();
        insert_cb52_ensemble(&db);
        assert!(db.list_all_orphan_join_nodes().unwrap().is_empty());

        db.insert_graph_node(&GraphNode {
            id: "orphan-join".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "orphaned quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "gone"}),
            position: 9,
            created_at: Utc::now(),
        })
        .unwrap();

        let all = db.list_all_orphan_join_nodes().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "orphan-join");
        assert_eq!(all[0].name, "orphaned quorum");

        let for_spec = db.list_orphan_join_nodes_for_spec("spec-1").unwrap();
        assert_eq!(for_spec.len(), 1);
        assert_eq!(for_spec[0].id, "orphan-join");

        // Wrong scopes see nothing.
        assert!(db
            .list_orphan_join_nodes_for_spec("no-such-spec")
            .unwrap()
            .is_empty());
        assert!(db
            .list_orphan_join_nodes_for_graph("no-such-graph")
            .unwrap()
            .is_empty());
    }

    /// `graph_update_ensemble`'s prompt-propagation contract: updating the
    /// ensemble's shared prompt must land on the `ensembles` row (the
    /// source of truth `graph_get` reads) *and* on every member node's own
    /// `config.prompt_template` — exercising the exact two-step sequence
    /// (`update_graph_node_details` per member, then `update_ensemble_prompt`)
    /// the handler performs.
    #[test]
    fn update_ensemble_prompt_propagates_to_every_member_config() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_nodes = vec![
            GraphNode {
                id: "m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-1".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "openrouter", "prompt_template": "old prompt"}),
                position: 2,
                created_at: now,
            },
            GraphNode {
                id: "m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-2".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude", "prompt_template": "old prompt"}),
                position: 3,
                created_at: now,
            },
        ];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 4,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "kickoff->m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m2".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m2->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m2".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "old prompt".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m1".to_string(),
                position: 0,
                platform: "openrouter".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
            },
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m2".to_string(),
                position: 1,
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
            },
        ];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        // Simulates `graph_update_ensemble`'s propagate-without-resize path:
        // rewrite every member's config with the new prompt (preserving its
        // own platform/model), then update the ensemble row itself.
        for member in &members {
            let config = serde_json::json!({
                "platform": member.platform,
                "prompt_template": "new prompt",
            });
            db.update_graph_node_details(&member.node_id, None, None, Some(&config), None)
                .unwrap();
        }
        db.update_ensemble_prompt("ens1", "new prompt").unwrap();

        assert_eq!(
            db.get_ensemble("ens1").unwrap().unwrap().prompt_template,
            "new prompt"
        );
        for member in &members {
            let node = db.get_graph_node(&member.node_id).unwrap().unwrap();
            assert_eq!(
                node.config.get("prompt_template").and_then(|v| v.as_str()),
                Some("new prompt"),
                "member '{}' must carry the propagated prompt",
                member.node_id
            );
        }
    }

    /// `min_pass`/`straggler_timeout_minutes`/`timeout_minutes` are each
    /// independently optional in an update: passing `None` for a field must
    /// leave it unchanged, and `straggler_timeout_minutes` specifically
    /// needs `Some(None)` (as opposed to bare `None`) to actually clear an
    /// override back to "defer to timeout_minutes" — this exercises both.
    #[test]
    fn update_ensemble_join_config_leaves_omitted_fields_unchanged() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_nodes = vec![GraphNode {
            id: "m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "openrouter"}),
            position: 2,
            created_at: now,
        }];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 3,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Solo".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: Some(15),
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "openrouter".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        // Only min_pass changes; straggler_timeout_minutes/timeout_minutes omitted.
        db.update_ensemble_join_config("ens1", Some(1), None, None, None)
            .unwrap();
        let after = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(after.min_pass, 1);
        assert_eq!(after.straggler_timeout_minutes, Some(15));
        assert_eq!(after.timeout_minutes, 30);

        // Explicitly clear straggler_timeout_minutes back to "defer to timeout_minutes".
        db.update_ensemble_join_config("ens1", None, Some(None), None, None)
            .unwrap();
        let cleared = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(cleared.straggler_timeout_minutes, None);
        assert_eq!(
            cleared.effective_straggler_timeout_minutes(),
            cleared.timeout_minutes
        );
    }

    /// Removing a member cascades away its node, its `ensemble_members` row,
    /// and both of its wiring edges (entry + join) — the rest of the
    /// ensemble is left intact.
    #[test]
    fn remove_ensemble_member_cascades_node_and_edges() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_ids = ["m1", "m2"];
        let member_nodes: Vec<GraphNode> = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| GraphNode {
                id: id.to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: format!("member-{}", i + 1),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "openrouter"}),
                position: 2 + i as i64,
                created_at: now,
            })
            .collect();
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 4,
            created_at: now,
        };
        let mut edges = Vec::new();
        for id in &member_ids {
            edges.push(GraphEdge {
                id: format!("kickoff->{id}"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: id.to_string(),
                condition: GraphEdgeCondition::Always,
            });
            edges.push(GraphEdge {
                id: format!("{id}->join1"),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: id.to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            });
        }
        edges.push(GraphEdge {
            id: "join1->arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            from_node: "join1".to_string(),
            to_node: "arbiter".to_string(),
            condition: GraphEdgeCondition::Pass,
        });
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members: Vec<EnsembleMember> = member_ids
            .iter()
            .enumerate()
            .map(|(i, id)| EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: id.to_string(),
                position: i as i64,
                platform: "openrouter".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
            })
            .collect();
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        assert!(db.remove_ensemble_member("m2").unwrap());

        assert!(db.get_graph_node("m2").unwrap().is_none());
        let remaining_edges = db.list_graph_edges("spec-1").unwrap();
        assert!(!remaining_edges
            .iter()
            .any(|edge| edge.from_node == "m2" || edge.to_node == "m2"));
        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(
            details
                .members
                .iter()
                .map(|m| m.node_id.as_str())
                .collect::<Vec<_>>(),
            vec!["m1"]
        );
        // The surviving member and the join itself are unaffected.
        assert!(db.get_graph_node("m1").unwrap().is_some());
        assert!(db.get_graph_node("join1").unwrap().is_some());
    }

    #[test]
    fn get_ensemble_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let result = db.get_ensemble("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_ensemble_members_empty() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let members = db.list_ensemble_members("nonexistent").unwrap();
        assert!(members.is_empty());
    }

    #[test]
    fn get_ensemble_details_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let result = db.get_ensemble_details("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn list_ensembles_for_spec_empty() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let ensembles = db.list_ensembles_for_spec("nonexistent").unwrap();
        assert!(ensembles.is_empty());
    }

    #[test]
    fn list_ensembles_for_graph_empty() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let ensembles = db.list_ensembles_for_graph("nonexistent").unwrap();
        assert!(ensembles.is_empty());
    }

    #[test]
    fn get_ensemble_by_member_node_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let result = db.get_ensemble_by_member_node("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_ensemble_by_join_node_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let result = db.get_ensemble_by_join_node("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn update_ensemble_prompt_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let updated = db
            .update_ensemble_prompt("nonexistent", "new prompt")
            .unwrap();
        assert!(!updated);
    }

    #[test]
    fn remove_ensemble_member_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let removed = db.remove_ensemble_member("nonexistent").unwrap();
        assert!(!removed);
    }

    #[test]
    fn get_ensemble_blueprint_by_name_not_found() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        let result = db.get_ensemble_blueprint_by_name("nonexistent").unwrap();
        assert!(result.is_none());
    }

    /// An installation whose `ensemble_blueprints` row still carries the old
    /// hardcoded free-tier model identities must have them reconciled away
    /// (to `None`) on the next startup reseed, and the reseed must be
    /// idempotent.
    #[test]
    fn seed_builtin_ensemble_blueprints_migrates_hardcoded_model_identities() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();

        db.delete_ensemble_blueprint_by_name("ensemble-proposers")
            .unwrap();
        db.insert_ensemble_blueprint(&EnsembleBlueprint {
            id: uuid::Uuid::new_v4().to_string(),
            name: "ensemble-proposers".to_string(),
            prompt_template: "Draft it".to_string(),
            members: vec![
                (
                    "openrouter".to_string(),
                    Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                    None,
                    None,
                ),
                (
                    "openrouter".to_string(),
                    Some("qwen/qwen3-coder:free".to_string()),
                    None,
                    None,
                ),
                (
                    "openrouter".to_string(),
                    Some("meta-llama/llama-3.3-70b-instruct:free".to_string()),
                    None,
                    None,
                ),
            ],
            min_pass: None,
            quorum_grace_minutes: None,
            builtin: true,
            created_at: Utc::now(),
        })
        .unwrap();

        db.seed_builtin_ensemble_blueprints().unwrap();

        let migrated = db
            .get_ensemble_blueprint_by_name("ensemble-proposers")
            .unwrap()
            .unwrap();
        assert_eq!(migrated.members.len(), 3);
        for (platform, model, _, _) in &migrated.members {
            assert_eq!(platform, "openrouter");
            assert!(model.is_none(), "model identity must be reconciled away");
        }

        // Idempotent: a second reseed changes nothing further.
        db.seed_builtin_ensemble_blueprints().unwrap();
        let after_second = db
            .get_ensemble_blueprint_by_name("ensemble-proposers")
            .unwrap()
            .unwrap();
        assert_eq!(after_second.members, migrated.members);
    }

    /// A custom ensemble blueprint that claims the builtin's name must never
    /// be overwritten or deleted by the migration.
    #[test]
    fn seed_builtin_ensemble_blueprints_never_touches_a_custom_blueprint_with_a_builtin_name() {
        let dir = tempdir().unwrap();
        let db = Database::new(&dir.path().join("test.db")).unwrap();
        db.delete_ensemble_blueprint_by_name("ensemble-proposers")
            .unwrap();

        let custom = EnsembleBlueprint {
            id: uuid::Uuid::new_v4().to_string(),
            name: "ensemble-proposers".to_string(),
            prompt_template: "my own take".to_string(),
            members: vec![("claude".to_string(), None, None, None)],
            min_pass: Some(1),
            quorum_grace_minutes: None,
            builtin: false,
            created_at: Utc::now(),
        };
        db.insert_ensemble_blueprint(&custom).unwrap();

        db.seed_builtin_ensemble_blueprints().unwrap();

        let fetched = db
            .get_ensemble_blueprint_by_name("ensemble-proposers")
            .unwrap()
            .unwrap();
        assert!(
            !fetched.builtin,
            "custom ensemble blueprint must stay custom"
        );
        assert_eq!(fetched.prompt_template, "my own take");
    }

    /// A member's `prompt_override` round-trips through `insert_ensemble_unit`
    /// and `get_ensemble_details` — `Some` for a member that has one, `None`
    /// for a member that doesn't (using the shared `prompt_template` exactly
    /// as every ensemble did before this field existed).
    #[test]
    fn insert_ensemble_unit_round_trips_member_prompt_override() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_nodes = vec![
            GraphNode {
                id: "m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-1".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude", "prompt_template": "review for security"}),
                position: 2,
                created_at: now,
            },
            GraphNode {
                id: "m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-2".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude", "prompt_template": "draft it"}),
                position: 3,
                created_at: now,
            },
        ];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 4,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "kickoff->m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m2".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m2->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m2".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m1".to_string(),
                position: 0,
                platform: "claude".to_string(),
                model: None,
                prompt_override: Some("review for security".to_string()),
                timeout_minutes: None,
            },
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m2".to_string(),
                position: 1,
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
            },
        ];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(
            details.members[0].prompt_override.as_deref(),
            Some("review for security")
        );
        assert_eq!(details.members[1].prompt_override, None);
    }

    /// A member's `timeout_minutes` round-trips through `insert_ensemble_unit`
    /// and `get_ensemble_details` — `Some` for a member with its own override,
    /// `None` for a member that uses the ensemble's shared `timeout_minutes`
    /// (CM23).
    #[test]
    fn insert_ensemble_unit_round_trips_member_timeout_minutes() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_nodes = vec![
            GraphNode {
                id: "m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-1".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude", "prompt_template": "draft it", "timeout_minutes": 7}),
                position: 2,
                created_at: now,
            },
            GraphNode {
                id: "m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                name: "member-2".to_string(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({"platform": "claude", "prompt_template": "draft it", "timeout_minutes": 30}),
                position: 3,
                created_at: now,
            },
        ];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 4,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "kickoff->m2".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m2".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m2->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m2".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Proposers".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m1".to_string(),
                position: 0,
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: Some(7),
            },
            EnsembleMember {
                ensemble_id: "ens1".to_string(),
                node_id: "m2".to_string(),
                position: 1,
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
            },
        ];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        let details = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(details.members[0].timeout_minutes, Some(7));
        assert_eq!(details.members[1].timeout_minutes, None);
    }

    /// `update_ensemble_member` (the renamed `update_ensemble_member_platform`)
    /// sets `prompt_override` outright, in both directions: giving a member
    /// that had none its own override, and clearing an existing override back
    /// to `None` (the "use the shared prompt" state) — both are ordinary
    /// `Some`/`None` writes, never a "leave unchanged" skip, since a
    /// replacement member list is always given in full.
    #[test]
    fn update_ensemble_member_sets_and_clears_prompt_override() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_nodes = vec![GraphNode {
            id: "m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "draft it"}),
            position: 2,
            created_at: now,
        }];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 3,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Solo".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        db.update_ensemble_member("ens1", "m1", "claude", None, Some("new angle"), None)
            .unwrap();
        let after_set = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(
            after_set.members[0].prompt_override.as_deref(),
            Some("new angle")
        );

        db.update_ensemble_member("ens1", "m1", "claude", None, None, None)
            .unwrap();
        let after_clear = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(after_clear.members[0].prompt_override, None);
    }

    /// `update_ensemble_member`'s new `timeout_minutes` parameter (CM23) is an
    /// ordinary `Some`/`None` write, same convention as `prompt_override`:
    /// setting a member's own override, then clearing it back to `None` (the
    /// "use the ensemble's timeout_minutes" state).
    #[test]
    fn update_ensemble_member_sets_and_clears_member_timeout() {
        let db = test_db();
        insert_spec(&db, "spec-1");
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: Utc::now(),
        })
        .unwrap();
        let now = Utc::now();
        let member_nodes = vec![GraphNode {
            id: "m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude", "prompt_template": "draft it"}),
            position: 2,
            created_at: now,
        }];
        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "quorum".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({"ensemble_id": "ens1"}),
            position: 3,
            created_at: now,
        };
        let edges = vec![
            GraphEdge {
                id: "kickoff->m1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "kickoff".to_string(),
                to_node: "m1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "m1->join1".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "m1".to_string(),
                to_node: "join1".to_string(),
                condition: GraphEdgeCondition::Always,
            },
            GraphEdge {
                id: "join1->arbiter".to_string(),
                spec_id: Some("spec-1".to_string()),
                graph_id: None,
                from_node: "join1".to_string(),
                to_node: "arbiter".to_string(),
                condition: GraphEdgeCondition::Pass,
            },
        ];
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "Solo".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];
        db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
            .unwrap();

        db.update_ensemble_member("ens1", "m1", "claude", None, None, Some(3))
            .unwrap();
        let after_set = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(after_set.members[0].timeout_minutes, Some(3));

        db.update_ensemble_member("ens1", "m1", "claude", None, None, None)
            .unwrap();
        let after_clear = db.get_ensemble_details("ens1").unwrap().unwrap();
        assert_eq!(after_clear.members[0].timeout_minutes, None);
    }

    /// A blueprint row seeded before `prompt_override`/`timeout_minutes`
    /// existed stores its `members` JSON as 2-element `[platform, model]` or
    /// 3-element `[platform, model, prompt_override]` arrays — an upgrade
    /// must still be able to read those rows back (as "no override for any
    /// member") without a data migration, alongside the current 4-element
    /// shape (CM23).
    #[test]
    fn parse_ensemble_blueprint_members_tolerates_legacy_two_element_rows() {
        let legacy = r#"[["openrouter","deepseek/deepseek-chat-v3.1:free"],["claude",null]]"#;
        let parsed = parse_ensemble_blueprint_members(legacy).unwrap();
        assert_eq!(
            parsed,
            vec![
                (
                    "openrouter".to_string(),
                    Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                    None,
                    None
                ),
                ("claude".to_string(), None, None, None),
            ]
        );

        let three_element = r#"[["openrouter","deepseek/deepseek-chat-v3.1:free","review it"],["claude",null,null]]"#;
        let parsed = parse_ensemble_blueprint_members(three_element).unwrap();
        assert_eq!(
            parsed,
            vec![
                (
                    "openrouter".to_string(),
                    Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                    Some("review it".to_string()),
                    None
                ),
                ("claude".to_string(), None, None, None),
            ]
        );

        let current = r#"[["openrouter","deepseek/deepseek-chat-v3.1:free","review it",3],["claude",null,null,null]]"#;
        let parsed = parse_ensemble_blueprint_members(current).unwrap();
        assert_eq!(
            parsed,
            vec![
                (
                    "openrouter".to_string(),
                    Some("deepseek/deepseek-chat-v3.1:free".to_string()),
                    Some("review it".to_string()),
                    Some(3)
                ),
                ("claude".to_string(), None, None, None),
            ]
        );
    }

    #[test]
    fn insert_ensemble_unit_with_cascade_kind_persists_kind() {
        let db = test_db();
        let spec_id = "spec-1".to_string();
        insert_spec(&db, &spec_id);
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        let now = chrono::Utc::now();

        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "join".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({}),
            position: 100,
            created_at: now,
        };
        let member_node = GraphNode {
            id: "m1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "member".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 50,
            created_at: now,
        };
        let edge = GraphEdge {
            id: "e1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        };
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some(spec_id),
            graph_id: None,
            name: "Cascade".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::Cascade,
            round_robin_index: None,
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];

        db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
            .unwrap();

        let loaded = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(loaded.kind, EnsembleKind::Cascade);
        assert_eq!(loaded.round_robin_index, None);
    }

    #[test]
    fn insert_ensemble_unit_with_round_robin_kind_persists_index() {
        let db = test_db();
        let spec_id = "spec-1".to_string();
        insert_spec(&db, &spec_id);
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        let now = chrono::Utc::now();

        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "join".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({}),
            position: 100,
            created_at: now,
        };
        let member_node = GraphNode {
            id: "m1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "member".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 50,
            created_at: now,
        };
        let edge = GraphEdge {
            id: "e1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        };
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some(spec_id),
            graph_id: None,
            name: "RoundRobin".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::RoundRobin,
            round_robin_index: Some(0),
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];

        db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
            .unwrap();

        let loaded = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(loaded.kind, EnsembleKind::RoundRobin);
        assert_eq!(loaded.round_robin_index, Some(0));
    }

    #[test]
    fn update_ensemble_kind_changes_kind_and_clears_index() {
        let db = test_db();
        let spec_id = "spec-1".to_string();
        insert_spec(&db, &spec_id);
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        let now = chrono::Utc::now();

        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "join".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({}),
            position: 100,
            created_at: now,
        };
        let member_node = GraphNode {
            id: "m1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "member".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 50,
            created_at: now,
        };
        let edge = GraphEdge {
            id: "e1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        };
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some(spec_id),
            graph_id: None,
            name: "RR".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::RoundRobin,
            round_robin_index: Some(2),
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];

        db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
            .unwrap();

        db.update_ensemble_kind("ens1", Some(EnsembleKind::Cascade), Some(None))
            .unwrap();

        let loaded = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(loaded.kind, EnsembleKind::Cascade);
        assert_eq!(loaded.round_robin_index, None);
    }

    #[test]
    fn round_robin_index_advances_and_wraps() {
        let db = test_db();
        let spec_id = "spec-1".to_string();
        insert_spec(&db, &spec_id);
        db.insert_graph_node(&GraphNode {
            id: "kickoff".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "kickoff".to_string(),
            kind: GraphNodeKind::Check,
            config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
            position: 1,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        db.insert_graph_node(&GraphNode {
            id: "arbiter".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "arbiter".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: 10,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
        let now = chrono::Utc::now();

        let join_node = GraphNode {
            id: "join1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "join".to_string(),
            kind: GraphNodeKind::Join,
            config: serde_json::json!({}),
            position: 100,
            created_at: now,
        };
        let member_node = GraphNode {
            id: "m1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            name: "member".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 50,
            created_at: now,
        };
        let edge = GraphEdge {
            id: "e1".to_string(),
            spec_id: Some(spec_id.clone()),
            graph_id: None,
            from_node: "kickoff".to_string(),
            to_node: "m1".to_string(),
            condition: GraphEdgeCondition::Always,
        };
        let ensemble = Ensemble {
            commit_rights: false,
            id: "ens1".to_string(),
            spec_id: Some(spec_id),
            graph_id: None,
            name: "RR".to_string(),
            prompt_template: "draft it".to_string(),
            join_node_id: "join1".to_string(),
            entry_from_node: "kickoff".to_string(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            on_pass_to: "arbiter".to_string(),
            on_fail_to: None,
            kind: EnsembleKind::RoundRobin,
            round_robin_index: Some(0),
            created_at: now,
        };
        let members = vec![EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        }];

        db.insert_ensemble_unit(&ensemble, &members, &[member_node], &join_node, &[edge])
            .unwrap();

        db.update_ensemble_kind("ens1", Some(EnsembleKind::RoundRobin), Some(Some(1)))
            .unwrap();
        let loaded = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(loaded.round_robin_index, Some(1));

        db.update_ensemble_kind("ens1", Some(EnsembleKind::RoundRobin), Some(Some(2)))
            .unwrap();
        let loaded = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(loaded.round_robin_index, Some(2));

        db.update_ensemble_kind("ens1", Some(EnsembleKind::RoundRobin), Some(Some(0)))
            .unwrap();
        let loaded = db.get_ensemble("ens1").unwrap().unwrap();
        assert_eq!(loaded.round_robin_index, Some(0));
    }
}
