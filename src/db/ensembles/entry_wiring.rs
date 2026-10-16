use super::*;

impl Database {
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
pub(super) type EnsembleGraphScope = (Option<String>, Option<String>, String, Vec<String>);

pub(super) fn tx_ensemble_graph_scope(
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

/// Entry sources read inside a transaction — the row's primary entry, every
/// pending source recorded at import (a crew-bound ensemble whose members are
/// not materialized yet; see `domain/graph_transfer/pending.rs`), plus the
/// edges into its members with the same shape as
/// [`ensemble_entry_sources`].
pub(super) fn tx_entry_sources(
    tx: &rusqlite::Transaction<'_>,
    ensemble_id: &str,
    member_ids: &[String],
    join_node_id: &str,
) -> Result<Vec<(String, GraphEdgeCondition)>> {
    let mut sources = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // The row's primary entry names the first source even when no edge
    // materializes it yet — a crew-bound ensemble's members are supplied
    // later, so its entry fan-out has nothing to derive from until then.
    let (row_entry, row_condition_raw): (String, String) = tx.query_row(
        "SELECT entry_from_node, entry_condition FROM ensembles WHERE id = ?1",
        params![ensemble_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if let Some(row_condition) = GraphEdgeCondition::from_str(&row_condition_raw) {
        if seen.insert((row_entry.clone(), row_condition.clone())) {
            sources.push((row_entry, row_condition));
        }
    }

    // Pending entry sources: the ensemble's extra entry sources and any
    // chained exit that lands on it, recorded by `graph_import` because the
    // member nodes (and so the fan-out edges) did not exist yet.
    let mut pending_stmt = tx.prepare(
        "SELECT from_node, condition FROM ensemble_pending_entry_sources WHERE ensemble_id = ?1",
    )?;
    let pending_rows = pending_stmt.query_map(params![ensemble_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in pending_rows {
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
pub(super) fn delete_entry_edges(
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
pub(super) fn insert_entry_fan_out(
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

pub(super) fn insert_missing_entry_fan_out(
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

/// Update the `graph_nodes` and `ensemble_members` rows of an existing member
/// that keeps its slot in the new list.
pub(super) fn tx_update_existing_member(
    tx: &rusqlite::Transaction<'_>,
    ensemble_id: &str,
    member: &EnsembleMember,
    node: &GraphNode,
) -> Result<()> {
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
             prompt_override = ?5, timeout_minutes = ?6, infra_retry_limit = ?7, infra_crash_max_seconds = ?8, infra_backoff_seconds = ?9
         WHERE ensemble_id = ?10 AND position = ?11",
        params![
            &member.node_id,
            member.position,
            &member.platform,
            &member.model,
            &member.prompt_override,
            &member.timeout_minutes,
            &member.infra_retry_limit,
            &member.infra_crash_max_seconds,
            &member.infra_backoff_seconds,
            ensemble_id,
            member.position,
        ],
    )?;
    Ok(())
}

/// Insert a brand-new member: its `graph_nodes` row, its `ensemble_members`
/// row, and its entry edge from the ensemble's join node.
pub(super) fn tx_insert_new_member(
    tx: &rusqlite::Transaction<'_>,
    member: &EnsembleMember,
    node: &GraphNode,
    join_node_id: &str,
) -> Result<()> {
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
         (ensemble_id, node_id, position, platform, model, prompt_override, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds)
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
    tx.execute(
        "INSERT INTO graph_edges
         (id, spec_id, graph_id, from_node, to_node, condition)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            uuid::Uuid::new_v4().to_string(),
            &node.spec_id,
            &node.graph_id,
            &node.id,
            join_node_id,
            GraphEdgeCondition::Always.as_str(),
        ],
    )?;
    Ok(())
}
