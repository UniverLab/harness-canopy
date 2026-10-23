//! DB layer for `graph_import`: persist an entire
//! [`GraphImportPlan`](crate::domain::graph_transfer::GraphImportPlan) — the
//! graph row, every plain node/edge, and every ensemble unit (join + member
//! nodes, the wiring edges, the `ensembles`/`ensemble_members` rows) — in
//! one transaction, so an import can never leave a half-created graph behind
//! (spec decision 5: "no partially-created graph is ever left behind").
//!
//! An imported graph never carries a trigger or any hooks (all four hook
//! events — the export document has no field for either — see spec
//! decision 1), so unlike [`Database::insert_graph`] this never needs to
//! encode one.
//!
//! The `ensembles`/`ensemble_members` writes mirror the authoring path's full
//! column set — including `kind`, `round_robin_index` and the infra budget
//! columns — so nothing the import plan carries is silently dropped to a
//! column default (CB62).

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use rusqlite::{params, Transaction};

use crate::db::Database;
use crate::domain::graph_transfer::{GraphImportEnsemblePlan, GraphImportPlan};
use crate::domain::graphs::{Graph, GraphEdge, GraphEdgeCondition, GraphNode};

/// Record `name` once, preserving first-seen order.
fn record_missing_crew(missing: &mut Vec<String>, name: &str) {
    if !missing.iter().any(|existing| existing == name) {
        missing.push(name.to_string());
    }
}

impl Database {
    /// Persist an imported plan in one transaction and return the crew names a
    /// bound ensemble referenced that do not exist locally (`missing_crews`).
    /// A bound ensemble whose crew exists has its members materialized through
    /// the one sync path after the transaction commits; one whose crew is
    /// missing is left pending (`crew_id` NULL, `crew_name` set) and cannot
    /// dispatch until the crew is created.
    pub fn import_graph(&self, lp: &Graph, plan: &GraphImportPlan) -> Result<Vec<String>> {
        // Resolve each bound ensemble's crew BEFORE taking the connection lock
        // (the transaction below holds it for its whole life).
        let (missing_crews, resolved_crew_ids) = self.resolve_bound_crews(plan)?;

        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;

        // CM49: completion hooks travel in the document and are restored
        // verbatim (an empty map writes NULL, as before).
        let hooks_json = if plan.hooks.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&plan.hooks)?)
        };

        insert_graph_row_tx(&tx, lp, &hooks_json)?;

        for node in &plan.nodes {
            insert_graph_node_tx(&tx, node)?;
        }
        for ensemble_plan in &plan.ensembles {
            insert_graph_node_tx(&tx, &ensemble_plan.join_node)?;
            for node in &ensemble_plan.member_nodes {
                insert_graph_node_tx(&tx, node)?;
            }
        }
        for edge in &plan.edges {
            insert_graph_edge_tx(&tx, edge)?;
        }
        for (index, ensemble_plan) in plan.ensembles.iter().enumerate() {
            insert_ensemble_unit_tx(&tx, ensemble_plan, &resolved_crew_ids[index])?;
            insert_pending_entry_sources_tx(&tx, ensemble_plan)?;
        }

        tx.commit()?;
        drop(conn);

        // Materialize every bound ensemble whose crew exists locally, through
        // the one sync path (it creates the member nodes/rows and wires the
        // fan-out/fan-in, then stamps the applied version).
        for ensemble_plan in &plan.ensembles {
            if ensemble_plan.ensemble.crew_name.is_some() {
                self.sync_ensemble_with_crew(&ensemble_plan.ensemble.id)?;
            }
        }
        Ok(missing_crews)
    }

    /// Resolve every bound crew named by the plan, returning the crew names
    /// that do not exist locally (deduped, first-seen order) and the resolved
    /// crew id per ensemble (aligned with `plan.ensembles`).
    fn resolve_bound_crews(
        &self,
        plan: &GraphImportPlan,
    ) -> Result<(Vec<String>, Vec<Option<String>>)> {
        let mut missing_crews: Vec<String> = Vec::new();
        let mut resolved_crew_ids: Vec<Option<String>> = Vec::with_capacity(plan.ensembles.len());
        for ensemble_plan in &plan.ensembles {
            match ensemble_plan.ensemble.crew_name.as_deref() {
                Some(name) => match self.crew_get(name)? {
                    Some((crew, _)) => resolved_crew_ids.push(Some(crew.id)),
                    None => {
                        record_missing_crew(&mut missing_crews, name);
                        resolved_crew_ids.push(None);
                    }
                },
                None => resolved_crew_ids.push(None),
            }
        }
        // CM49: a crew-bound plain node or agent hook names a crew too; report
        // every name that does not exist locally (deduped) so the caller knows
        // what to create before the graph can run.
        let node_crews = plan
            .nodes
            .iter()
            .filter_map(|node| node.config.get("crew").and_then(serde_json::Value::as_str));
        for name in node_crews {
            if self.crew_get(name)?.is_none() {
                record_missing_crew(&mut missing_crews, name);
            }
        }
        let hook_crews = plan
            .hooks
            .values()
            .flatten()
            .filter_map(|hook| hook.crew.as_deref());
        for name in hook_crews {
            if self.crew_get(name)?.is_none() {
                record_missing_crew(&mut missing_crews, name);
            }
        }
        Ok((missing_crews, resolved_crew_ids))
    }

    /// Every pending entry source recorded for a graph's ensembles, keyed by
    /// ensemble id — `graph_export` passes these to
    /// [`build_export_document_with_pending`] so a crew-bound ensemble whose
    /// members are not materialised yet still exports its entry wiring
    /// (requirement 3). A literal-member ensemble has none.
    pub fn pending_entry_sources_for_graph(
        &self,
        graph_id: &str,
    ) -> Result<HashMap<String, Vec<(String, GraphEdgeCondition)>>> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let mut stmt = conn.prepare(
            "SELECT p.ensemble_id, p.from_node, p.condition
             FROM ensemble_pending_entry_sources p
             JOIN ensembles e ON e.id = p.ensemble_id
             WHERE e.graph_id = ?1",
        )?;
        let rows = stmt.query_map(params![graph_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut out: HashMap<String, Vec<(String, GraphEdgeCondition)>> = HashMap::new();
        for row in rows {
            let (ensemble_id, from_node, condition_raw) = row?;
            let Some(condition) = GraphEdgeCondition::from_str(&condition_raw) else {
                continue;
            };
            out.entry(ensemble_id)
                .or_default()
                .push((from_node, condition));
        }
        Ok(out)
    }
}

fn insert_graph_row_tx(tx: &Transaction, lp: &Graph, hooks_json: &Option<String>) -> Result<()> {
    tx.execute(
        "INSERT INTO graphs (id, name, description, workdir, status, trigger_type, trigger_config, created_at, started_at, completed_at, autorun_at, active_run_queue_id, on_completed, auto_continue_at, auto_continue_action, archived, paused_by_reconciliation, infra_node_id, hooks)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
        params![
            &lp.id,
            &lp.name,
            &lp.description,
            &lp.workdir,
            lp.status.as_str(),
            Option::<String>::None,
            Option::<String>::None,
            lp.created_at.timestamp(),
            lp.started_at.map(|value| value.timestamp()),
            lp.completed_at.map(|value| value.timestamp()),
            lp.autorun_at.map(|value| value.timestamp()),
            &lp.active_run_queue_id,
            Option::<String>::None,
            lp.auto_continue_at.map(|value| value.timestamp()),
            &lp.auto_continue_action,
            lp.archived,
            lp.paused_by_reconciliation,
            &lp.infra_node_id,
            hooks_json,
        ],
    )?;
    Ok(())
}

fn insert_ensemble_unit_tx(
    tx: &Transaction,
    ensemble_plan: &GraphImportEnsemblePlan,
    crew_id: &Option<String>,
) -> Result<()> {
    let ensemble = &ensemble_plan.ensemble;
    tx.execute(
        "INSERT INTO ensembles (id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, infra_retry_limit, infra_crash_max_seconds, infra_backoff_seconds, on_pass_to, on_fail_to, kind, round_robin_index, commit_rights, created_at, crew_id, crew_name, crew_version_applied)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
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
            crew_id,
            &ensemble.crew_name,
            Option::<i64>::None,
        ],
    )?;
    for member in &ensemble_plan.members {
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
    Ok(())
}

/// CM49 import: record the entry sources a crew-bound ensemble must wire once
/// its members materialise. `INSERT OR IGNORE` makes a repeated source a
/// no-op rather than an error.
fn insert_pending_entry_sources_tx(
    tx: &Transaction,
    ensemble_plan: &GraphImportEnsemblePlan,
) -> Result<()> {
    for (from_node, condition) in &ensemble_plan.pending_entry_sources {
        tx.execute(
            "INSERT OR IGNORE INTO ensemble_pending_entry_sources (ensemble_id, from_node, condition)
             VALUES (?1, ?2, ?3)",
            params![
                &ensemble_plan.ensemble.id,
                from_node,
                condition.as_str()
            ],
        )?;
    }
    Ok(())
}

fn insert_graph_node_tx(tx: &Transaction, node: &GraphNode) -> Result<()> {
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
    Ok(())
}

fn insert_graph_edge_tx(tx: &Transaction, edge: &GraphEdge) -> Result<()> {
    tx.execute(
        "INSERT INTO graph_edges (id, spec_id, graph_id, from_node, to_node, condition, route)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            &edge.id,
            &edge.spec_id,
            &edge.graph_id,
            &edge.from_node,
            &edge.to_node,
            edge.condition.as_str(),
            edge.condition.route_label(),
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graph_transfer::{
        build_export_document, build_export_document_with_pending, build_import_plan,
        GraphExportDocument, GraphExportEnsembleTarget, GraphExportEntrySource,
    };
    use crate::domain::graphs::{
        Ensemble, EnsembleDetails, EnsembleKind, GraphEdgeCondition, GraphNodeKind, GraphStatus,
    };
    use crate::domain::validation::{
        validate_ensembles_in_graph, validate_graph, GraphEdgeView, GraphNodeView,
    };
    use chrono::Utc;
    use tempfile::tempdir;

    fn test_db() -> Database {
        let dir = tempdir().unwrap();
        Database::new(&dir.path().join("test.db")).unwrap()
    }

    fn draft_graph(id: &str, name: &str, workdir: &str) -> Graph {
        Graph {
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            id: id.to_string(),
            name: name.to_string(),
            description: Some("imported".to_string()),
            workdir: workdir.to_string(),
            status: GraphStatus::Draft,
            trigger: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: std::collections::BTreeMap::new(),
            require_spec_type: false,
        }
    }

    fn simple_document() -> GraphExportDocument {
        use crate::domain::graph_transfer::{GraphExportEdge, GraphExportNode};
        GraphExportDocument {
            format_version: 2,
            name: "shared-graph".to_string(),
            description: Some("A shared design".to_string()),
            nodes: vec![
                GraphExportNode {
                    name: "implementer".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({"prompt_template": "implement it"}),
                },
                GraphExportNode {
                    name: "gate".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "cargo test"}),
                },
            ],
            edges: vec![GraphExportEdge {
                from_node: "implementer".to_string(),
                to_node: "gate".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            ensembles: vec![],
            infra_node: None,

            hooks: Default::default(),
        }
    }

    /// The DB-layer half of decision 5's all-or-nothing contract: one
    /// `import_graph` call persists the graph row, every node, and
    /// every edge together.
    #[test]
    fn import_graph_persists_graph_nodes_and_edges_atomically() {
        let db = test_db();
        let document = simple_document();
        let lp = draft_graph("graph-1", "shared-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();

        db.import_graph(&lp, &plan).unwrap();

        assert!(db.get_graph("graph-1").unwrap().is_some());
        let nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        assert_eq!(nodes.len(), 2);
        let edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        assert_eq!(edges.len(), 1);
    }

    /// A second import of the same document under a different graph id must
    /// create an entirely independent graph rather than colliding with (or
    /// overwriting) the first — decision 4: import never updates/merges.
    #[test]
    fn import_graph_never_collides_across_two_imports() {
        let db = test_db();
        let document = simple_document();

        let lp1 = draft_graph("graph-1", "shared-graph", "/tmp/project");
        let plan1 = build_import_plan(&document, &lp1.id).unwrap();
        db.import_graph(&lp1, &plan1).unwrap();

        let lp2 = draft_graph("graph-2", "shared-graph (2)", "/tmp/project");
        let plan2 = build_import_plan(&document, &lp2.id).unwrap();
        db.import_graph(&lp2, &plan2).unwrap();

        let all = db.list_graphs(Some("/tmp/project"), true).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(db.list_graph_nodes_for_graph("graph-1").unwrap().len(), 2);
        assert_eq!(db.list_graph_nodes_for_graph("graph-2").unwrap().len(), 2);
    }

    /// Ensemble import persists the join node, every member node, and the
    /// `ensembles`/`ensemble_members` rows in the same transaction as the
    /// rest of the graph.
    #[test]
    fn import_graph_persists_ensemble_unit() {
        use crate::domain::graph_transfer::{
            GraphExportEnsemble, GraphExportEnsembleMember, GraphExportEnsembleTarget,
            GraphExportNode,
        };
        let db = test_db();
        let document = GraphExportDocument {
            format_version: 2,
            name: "ensemble-graph".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "downstream".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 2,
                    config: serde_json::json!({"prompt_template": "wrap up"}),
                },
            ],
            edges: vec![],
            ensembles: vec![GraphExportEnsemble {
                commit_rights: false,
                name: "Proposers".to_string(),
                kind: None,
                prompt_template: "draft it".to_string(),
                entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                entry_condition: GraphEdgeCondition::Always,
                extra_entry_sources: vec![],
                on_pass_to: GraphExportEnsembleTarget::Node("downstream".to_string()),
                on_fail_to: None,
                min_pass: 2,
                timeout_minutes: 30,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                members: vec![
                    GraphExportEnsembleMember {
                        platform: Some("openrouter".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                    GraphExportEnsembleMember {
                        platform: Some("openrouter".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                ],

                crew: None,
            }],
            infra_node: None,

            hooks: Default::default(),
        };
        let lp = draft_graph("graph-1", "ensemble-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();

        db.import_graph(&lp, &plan).unwrap();

        // 2 plain nodes + 2 members + 1 join = 5.
        let nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        assert_eq!(nodes.len(), 5);
        let ensembles = db.list_ensembles_for_graph("graph-1").unwrap();
        assert_eq!(ensembles.len(), 1);
        assert_eq!(ensembles[0].members.len(), 2);

        // Re-exporting from the DB's own view must reconstruct the same
        // ensemble shape (round trip through actual persistence, not just
        // the in-memory plan).
        let graph_nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        let graph_edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        let lp_row = db.get_graph("graph-1").unwrap().unwrap();
        let redone =
            build_export_document(&lp_row, &graph_nodes, &graph_edges, &ensembles).unwrap();
        assert_eq!(redone.ensembles.len(), 1);
        assert_eq!(redone.nodes.len(), 2);
    }

    /// CM2: importing a document with `infra_node` set must persist the
    /// resolved node id on the graph row — not silently drop it.
    #[test]
    fn import_graph_persists_infra_node_id() {
        use crate::domain::graph_transfer::{GraphExportEdge, GraphExportNode};
        let db = test_db();
        let document = GraphExportDocument {
            format_version: 2,
            name: "infra-graph".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "A".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 1,
                    config: serde_json::json!({"prompt_template": "do it"}),
                },
                GraphExportNode {
                    name: "B".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![GraphExportEdge {
                from_node: "A".to_string(),
                to_node: "B".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            ensembles: vec![],
            infra_node: Some("B".to_string()),

            hooks: Default::default(),
        };
        let mut lp = draft_graph("graph-1", "infra-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();
        let expected_infra_id = plan
            .infra_node_id
            .clone()
            .expect("plan resolved infra node");
        lp.infra_node_id = Some(expected_infra_id.clone());

        db.import_graph(&lp, &plan).unwrap();

        let stored = db.get_graph("graph-1").unwrap().expect("graph exists");
        assert_eq!(
            stored.infra_node_id,
            Some(expected_infra_id),
            "imported graph must persist infra_node_id"
        );
    }

    /// CB62: the exact measured defect — a `cascade` ensemble must come back
    /// as `cascade` from the DB, not as the column default `parallel`. Also
    /// pins `round_robin_index` persistence.
    #[test]
    fn import_graph_persists_ensemble_kind() {
        use crate::domain::graph_transfer::{
            GraphExportEnsemble, GraphExportEnsembleMember, GraphExportEnsembleTarget,
            GraphExportNode,
        };
        use crate::domain::graphs::EnsembleKind;
        let db = test_db();
        let member = || GraphExportEnsembleMember {
            platform: Some("claude".to_string()),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
        };
        let document = GraphExportDocument {
            format_version: 2,
            name: "kinds-graph".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "final".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![],
            ensembles: vec![
                GraphExportEnsemble {
                    commit_rights: false,
                    name: "Cascade Team".to_string(),
                    kind: Some("cascade".to_string()),
                    prompt_template: "go".to_string(),
                    entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                    entry_condition: GraphEdgeCondition::Always,
                    extra_entry_sources: vec![],
                    on_pass_to: GraphExportEnsembleTarget::Node("final".to_string()),
                    on_fail_to: None,
                    min_pass: 1,
                    timeout_minutes: 30,
                    straggler_timeout_minutes: None,
                    quorum_grace_minutes: None,
                    members: vec![member(), member()],

                    crew: None,
                },
                GraphExportEnsemble {
                    commit_rights: false,
                    name: "RR Team".to_string(),
                    kind: Some("round_robin".to_string()),
                    prompt_template: "go".to_string(),
                    entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                    entry_condition: GraphEdgeCondition::Always,
                    extra_entry_sources: vec![],
                    on_pass_to: GraphExportEnsembleTarget::Node("final".to_string()),
                    on_fail_to: None,
                    min_pass: 1,
                    timeout_minutes: 30,
                    straggler_timeout_minutes: None,
                    quorum_grace_minutes: None,
                    members: vec![member(), member()],

                    crew: None,
                },
            ],
            infra_node: None,

            hooks: Default::default(),
        };
        let lp = draft_graph("graph-1", "kinds-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();
        db.import_graph(&lp, &plan).unwrap();

        let stored = db.list_ensembles_for_graph("graph-1").unwrap();
        assert_eq!(stored.len(), 2);
        let cascade = stored
            .iter()
            .find(|d| d.ensemble.name == "Cascade Team")
            .expect("cascade ensemble present");
        let rr = stored
            .iter()
            .find(|d| d.ensemble.name == "RR Team")
            .expect("round_robin ensemble present");
        assert_eq!(cascade.ensemble.kind, EnsembleKind::Cascade);
        assert_eq!(rr.ensemble.kind, EnsembleKind::RoundRobin);
        assert_eq!(rr.ensemble.round_robin_index, Some(0));
    }

    /// CB71: a cascade entered from `unlock` on pass AND from
    /// `check_committed` on fail (the canopy-v4.1 committer loop: "Unlock for
    /// committer"/"Check committed") must survive export -> import -> DB ->
    /// re-export with both sources and the full fan-out, and the ensemble
    /// section must re-export byte-identically.
    #[test]
    fn import_graph_persists_every_entry_source() {
        use crate::domain::graph_transfer::{
            GraphExportEnsemble, GraphExportEnsembleMember, GraphExportEnsembleTarget,
            GraphExportEntrySource, GraphExportNode, GRAPH_EXPORT_FORMAT_VERSION,
        };
        let db = test_db();
        let document = GraphExportDocument {
            format_version: GRAPH_EXPORT_FORMAT_VERSION,
            name: "cb71-committer".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "unlock".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "check_committed".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "final".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 3,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![],
            ensembles: vec![GraphExportEnsemble {
                commit_rights: true,
                name: "Committer".to_string(),
                kind: Some("cascade".to_string()),
                prompt_template: "commit".to_string(),
                entry_from_node: GraphExportEnsembleTarget::Node("unlock".to_string()),
                entry_condition: GraphEdgeCondition::Pass,
                extra_entry_sources: vec![GraphExportEntrySource {
                    from_node: "check_committed".to_string(),
                    condition: GraphEdgeCondition::Fail,
                }],
                on_pass_to: GraphExportEnsembleTarget::Node("final".to_string()),
                on_fail_to: Some(GraphExportEnsembleTarget::Node(
                    "check_committed".to_string(),
                )),
                min_pass: 1,
                timeout_minutes: 30,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                members: vec![
                    GraphExportEnsembleMember {
                        platform: Some("claude".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                    GraphExportEnsembleMember {
                        platform: Some("copilot".to_string()),
                        model: None,
                        prompt_override: None,
                        timeout_minutes: None,
                    },
                ],

                crew: None,
            }],
            infra_node: None,

            hooks: Default::default(),
        };
        let lp = draft_graph("graph-1", "cb71-committer", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();
        db.import_graph(&lp, &plan).unwrap();

        let nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        let node_ids: std::collections::HashMap<&str, &str> = nodes
            .iter()
            .map(|node| (node.name.as_str(), node.id.as_str()))
            .collect();
        let member_ids: std::collections::HashSet<&str> = ["Committer [1]", "Committer [2]"]
            .into_iter()
            .map(|name| *node_ids.get(name).expect("imported ensemble member"))
            .collect();
        let edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        let mut unlock_targets: Vec<&str> = edges
            .iter()
            .filter(|edge| {
                edge.from_node == *node_ids.get("unlock").expect("unlock node")
                    && edge.condition == GraphEdgeCondition::Pass
            })
            .map(|edge| edge.to_node.as_str())
            .filter(|target| member_ids.contains(target))
            .collect();
        let mut check_committed_targets: Vec<&str> = edges
            .iter()
            .filter(|edge| {
                edge.from_node
                    == *node_ids
                        .get("check_committed")
                        .expect("check_committed node")
                    && edge.condition == GraphEdgeCondition::Fail
            })
            .map(|edge| edge.to_node.as_str())
            .filter(|target| member_ids.contains(target))
            .collect();
        unlock_targets.sort_unstable();
        check_committed_targets.sort_unstable();
        assert_eq!(
            unlock_targets.len(),
            2,
            "unlock/pass must fan out to both members"
        );
        assert_eq!(
            check_committed_targets.len(),
            2,
            "check_committed/fail must fan out to both members"
        );
        assert_eq!(
            unlock_targets, check_committed_targets,
            "both sources must reach the same two members"
        );
        assert_eq!(unlock_targets.len(), member_ids.len());
        assert_eq!(check_committed_targets.len(), member_ids.len());

        let ensembles = db.list_ensembles_for_graph("graph-1").unwrap();
        let stored_graph = db.get_graph("graph-1").unwrap().unwrap();
        let redone = build_export_document(&stored_graph, &nodes, &edges, &ensembles).unwrap();
        assert_eq!(
            serde_json::to_value(&redone.ensembles).unwrap(),
            serde_json::to_value(&document.ensembles).unwrap()
        );
        assert_eq!(
            redone.ensembles[0].extra_entry_sources,
            document.ensembles[0].extra_entry_sources
        );
    }

    /// CB62 FR4 (DB half): the persisted round trip compared as whole
    /// serialized documents — this is the test that fails on the current
    /// code and passes once the INSERT carries `kind`. Every optional
    /// ensemble/member field is non-default so a silently-dropped column
    /// shows up as a document diff (shape-derived, per the NFR).
    #[test]
    fn import_graph_round_trips_the_whole_document() {
        use crate::domain::graph_transfer::{
            GraphExportEdge, GraphExportEnsemble, GraphExportEnsembleMember,
            GraphExportEnsembleTarget, GraphExportNode,
        };
        let db = test_db();
        let doc = GraphExportDocument {
            format_version: 2,
            name: "cb62-full".to_string(),
            description: Some("all fields".to_string()),
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "final".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 2,
                    config: serde_json::json!({"command": "true"}),
                },
            ],
            edges: vec![GraphExportEdge {
                from_node: "kickoff".to_string(),
                to_node: "final".to_string(),
                condition: GraphEdgeCondition::Always,
            }],
            ensembles: vec![
                GraphExportEnsemble {
                    commit_rights: true,
                    name: "Alpha".to_string(),
                    kind: Some("cascade".to_string()),
                    prompt_template: "go".to_string(),
                    entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                    entry_condition: GraphEdgeCondition::Always,
                    extra_entry_sources: vec![
                        crate::domain::graph_transfer::GraphExportEntrySource {
                            from_node: "kickoff".to_string(),
                            condition: GraphEdgeCondition::Error,
                        },
                    ],
                    on_pass_to: GraphExportEnsembleTarget::Ensemble {
                        ensemble: "Beta".to_string(),
                    },
                    on_fail_to: Some(GraphExportEnsembleTarget::Node("final".to_string())),
                    min_pass: 1,
                    timeout_minutes: 15,
                    straggler_timeout_minutes: Some(7),
                    quorum_grace_minutes: None,
                    members: vec![
                        GraphExportEnsembleMember {
                            platform: Some("copilot".to_string()),
                            model: Some("model-a".to_string()),
                            prompt_override: Some("angle a".to_string()),
                            timeout_minutes: Some(15),
                        },
                        GraphExportEnsembleMember {
                            platform: Some("opencode".to_string()),
                            model: None,
                            prompt_override: None,
                            timeout_minutes: None,
                        },
                    ],

                    crew: None,
                },
                GraphExportEnsemble {
                    commit_rights: false,
                    name: "Beta".to_string(),
                    kind: Some("round_robin".to_string()),
                    prompt_template: "review".to_string(),
                    entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                    entry_condition: GraphEdgeCondition::Always,
                    extra_entry_sources: vec![],
                    on_pass_to: GraphExportEnsembleTarget::Node("final".to_string()),
                    on_fail_to: None,
                    min_pass: 1,
                    timeout_minutes: 30,
                    straggler_timeout_minutes: Some(5),
                    quorum_grace_minutes: None,
                    members: vec![
                        GraphExportEnsembleMember {
                            platform: Some("claude".to_string()),
                            model: Some("sonnet".to_string()),
                            prompt_override: None,
                            timeout_minutes: None,
                        },
                        GraphExportEnsembleMember {
                            platform: Some("claude".to_string()),
                            model: None,
                            prompt_override: Some("angle b".to_string()),
                            timeout_minutes: Some(9),
                        },
                    ],

                    crew: None,
                },
            ],
            infra_node: Some("final".to_string()),

            hooks: Default::default(),
        };
        let lp = draft_graph("graph-1", "cb62-full", "/tmp/project");
        let plan = build_import_plan(&doc, &lp.id).unwrap();
        let mut lp = lp;
        lp.description = doc.description.clone();
        lp.infra_node_id = plan.infra_node_id.clone();
        db.import_graph(&lp, &plan).unwrap();

        // Read back the imported graph exactly as graph_export does...
        let graph_nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        let graph_edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        let ensembles = db.list_ensembles_for_graph("graph-1").unwrap();
        let stored_graph = db.get_graph("graph-1").unwrap().unwrap();
        let redone =
            build_export_document(&stored_graph, &graph_nodes, &graph_edges, &ensembles).unwrap();

        // ...and compare the whole serialized documents section by section.
        let first_value = serde_json::to_value(&doc).unwrap();
        let second_value = serde_json::to_value(&redone).unwrap();
        assert_eq!(
            first_value["ensembles"], second_value["ensembles"],
            "CB62: every ensemble field must survive the DB round trip"
        );
        assert_eq!(first_value["nodes"], second_value["nodes"]);
        assert_eq!(first_value["edges"], second_value["edges"]);
        assert_eq!(first_value["infra_node"], second_value["infra_node"]);
        assert_eq!(first_value["name"], second_value["name"]);
        assert_eq!(first_value["description"], second_value["description"]);
    }

    /// CM49: a document whose ensemble is bound to a crew that exists locally
    /// imports bound — its members are materialized from the crew and the
    /// re-export carries `crew` with no members.
    #[test]
    fn import_crew_bound_ensemble_binds_when_crew_exists() {
        let db = test_db();
        db.crew_create(
            "two",
            None,
            &[crew_member("openrouter"), crew_member("opencode-go")],
        )
        .unwrap();
        let document = crew_bound_document("two");
        let lp = draft_graph("graph-1", "crew-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();

        let missing = db.import_graph(&lp, &plan).unwrap();
        assert!(missing.is_empty(), "the crew exists locally");

        let ensembles = db.list_ensembles_for_graph("graph-1").unwrap();
        assert_eq!(ensembles[0].members.len(), 2, "members materialized");
        assert_eq!(ensembles[0].ensemble.crew_name.as_deref(), Some("two"));
        assert_eq!(ensembles[0].ensemble.crew_version_applied, Some(1));

        // Re-export must carry the crew name and no members.
        let graph_nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        let graph_edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        let lp_row = db.get_graph("graph-1").unwrap().unwrap();
        let redone =
            build_export_document(&lp_row, &graph_nodes, &graph_edges, &ensembles).unwrap();
        assert_eq!(redone.ensembles[0].crew.as_deref(), Some("two"));
        assert!(redone.ensembles[0].members.is_empty());
    }

    /// CM49: the same document with no local crew still imports — the
    /// ensemble is pending (`crew_name` set, no members) and the name is
    /// reported under `missing_crews`; creating the crew and syncing later
    /// materializes the members.
    #[test]
    fn import_crew_bound_ensemble_without_crew_is_pending() {
        let db = test_db();
        let document = crew_bound_document("two");
        let lp = draft_graph("graph-1", "crew-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();

        let missing = db.import_graph(&lp, &plan).unwrap();
        assert_eq!(missing, vec!["two".to_string()]);

        let ensembles = db.list_ensembles_for_graph("graph-1").unwrap();
        assert!(ensembles[0].members.is_empty(), "pending: no members yet");
        assert_eq!(ensembles[0].ensemble.crew_id, None);
        assert_eq!(ensembles[0].ensemble.crew_name.as_deref(), Some("two"));
        assert_eq!(ensembles[0].ensemble.crew_version_applied, None);
        assert_eq!(
            db.sync_ensemble_with_crew(&ensembles[0].ensemble.id)
                .unwrap(),
            crate::db::crews::SyncOutcome::MissingCrew
        );

        // Create the crew; the next sync materializes it.
        db.crew_create(
            "two",
            None,
            &[crew_member("openrouter"), crew_member("opencode-go")],
        )
        .unwrap();
        db.sync_ensemble_with_crew(&ensembles[0].ensemble.id)
            .unwrap();
        let ensembles = db.list_ensembles_for_graph("graph-1").unwrap();
        assert_eq!(ensembles[0].members.len(), 2);
    }

    fn crew_member(platform: &str) -> crate::domain::crews::CrewMember {
        crate::domain::crews::CrewMember {
            position: 0,
            platform: platform.to_string(),
            model: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }
    }

    fn crew_bound_document(crew: &str) -> GraphExportDocument {
        use crate::domain::graph_transfer::{
            GraphExportEnsemble, GraphExportEnsembleTarget, GraphExportNode,
        };
        GraphExportDocument {
            format_version: 5,
            name: "crew-graph".to_string(),
            description: None,
            nodes: vec![
                GraphExportNode {
                    name: "kickoff".to_string(),
                    kind: GraphNodeKind::Check,
                    position: 1,
                    config: serde_json::json!({"command": "true"}),
                },
                GraphExportNode {
                    name: "downstream".to_string(),
                    kind: GraphNodeKind::Agent,
                    position: 2,
                    config: serde_json::json!({"platform": "claude"}),
                },
            ],
            edges: vec![],
            ensembles: vec![GraphExportEnsemble {
                commit_rights: false,
                name: "Proposers".to_string(),
                kind: None,
                prompt_template: "draft it".to_string(),
                entry_from_node: GraphExportEnsembleTarget::Node("kickoff".to_string()),
                entry_condition: GraphEdgeCondition::Always,
                extra_entry_sources: vec![],
                on_pass_to: GraphExportEnsembleTarget::Node("downstream".to_string()),
                on_fail_to: None,
                min_pass: 2,
                timeout_minutes: 30,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                crew: Some(crew.to_string()),
                members: vec![],
            }],
            infra_node: None,

            hooks: Default::default(),
        }
    }

    /// A document with a crew-bound agent node AND a crew-bound agent hook.
    fn crew_hook_document(crew: &str) -> GraphExportDocument {
        use crate::domain::graph_transfer::{GraphExportEdge, GraphExportNode};
        use crate::domain::graphs::{GraphCompletionHook, GraphHookEvent};
        let mut document = simple_document();
        document.format_version = 5;
        document.nodes.push(GraphExportNode {
            name: "crewnode".to_string(),
            kind: GraphNodeKind::Agent,
            position: 3,
            config: serde_json::json!({"crew": crew, "prompt_template": "x"}),
        });
        document.edges.push(GraphExportEdge {
            from_node: "implementer".to_string(),
            to_node: "crewnode".to_string(),
            condition: GraphEdgeCondition::Always,
        });
        document.hooks.insert(
            GraphHookEvent::OnCompleted,
            vec![GraphCompletionHook {
                platform: None,
                crew: Some(crew.to_string()),
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
        );
        document
    }

    /// CM49 req 7: a crew-bound hook and node survive the round trip — the
    /// hook is persisted with its `crew` and re-exported, and a crew-bound
    /// node is NOT reported as missing a platform.
    #[test]
    fn crew_bound_hook_and_node_round_trip() {
        use crate::domain::graphs::GraphHookEvent;
        let db = test_db();
        db.crew_create("crewhook", None, &[crew_member("claude")])
            .unwrap();
        let document = crew_hook_document("crewhook");
        let lp = draft_graph("graph-1", "crew-hook-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();
        assert!(
            !crate::domain::graph_transfer::agent_nodes_missing_platform(&plan)
                .contains(&"crewnode".to_string()),
            "a crew-bound node names a harness and must not be listed"
        );

        let missing = db.import_graph(&lp, &plan).unwrap();
        assert!(missing.is_empty(), "the crew exists locally");

        let stored = db.get_graph("graph-1").unwrap().unwrap();
        let hook = &stored.hooks.get(&GraphHookEvent::OnCompleted).unwrap()[0];
        assert_eq!(hook.crew.as_deref(), Some("crewhook"));

        let nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
        let edges = db.list_graph_edges_for_graph("graph-1").unwrap();
        let redone = build_export_document(&stored, &nodes, &edges, &[]).unwrap();
        assert_eq!(
            redone.hooks.get(&GraphHookEvent::OnCompleted).unwrap()[0]
                .crew
                .as_deref(),
            Some("crewhook"),
            "the hook crew binding round-trips through export"
        );
    }

    /// CM49 req 7: a crew-bound node/hook whose crew is absent still imports
    /// and its name is reported under `missing_crews`.
    #[test]
    fn crew_bound_hook_without_local_crew_is_reported() {
        let db = test_db();
        let document = crew_hook_document("ghost");
        let lp = draft_graph("graph-1", "crew-hook-graph", "/tmp/project");
        let plan = build_import_plan(&document, &lp.id).unwrap();
        let missing = db.import_graph(&lp, &plan).unwrap();
        assert_eq!(missing, vec!["ghost".to_string()]);
        assert!(
            db.get_graph("graph-1").unwrap().is_some(),
            "import succeeds"
        );
    }

    /// A graph with two crew-bound ensembles on the same crew: "Team A"
    /// (round_robin) entered from `kickoff` (primary) and `alt` (an extra
    /// `fail` source), and "Team B" (cascade) whose `on_fail_to` chains into
    /// A. Both carry no member nodes of their own — the crew supplies them.
    fn seed_crew_chain_graph(db: &Database) -> Graph {
        let lp = draft_graph("graph-1", "crew-chain", "/tmp/project");
        db.insert_graph(&lp).unwrap();
        let now = Utc::now();
        for (id, position) in [("kickoff", 1), ("alt", 2), ("final", 9)] {
            db.insert_graph_node(&GraphNode {
                id: id.to_string(),
                spec_id: None,
                graph_id: Some("graph-1".to_string()),
                name: id.to_string(),
                kind: GraphNodeKind::Check,
                config: serde_json::json!({"command": "true"}),
                position,
                created_at: now,
            })
            .unwrap();
        }
        // kickoff -> alt keeps the graph at a single entry point (the `fail`
        // source is reached rather than dangling).
        db.insert_graph_edge(&GraphEdge {
            id: "kickoff->alt".to_string(),
            spec_id: None,
            graph_id: Some("graph-1".to_string()),
            from_node: "kickoff".to_string(),
            to_node: "alt".to_string(),
            condition: GraphEdgeCondition::Always,
        })
        .unwrap();
        db.crew_create(
            "reviewers",
            None,
            &[crew_member("openrouter"), crew_member("opencode-go")],
        )
        .unwrap();

        for (suffix, kind, position, on_fail_to) in [
            ("A", EnsembleKind::RoundRobin, 10, None),
            ("B", EnsembleKind::Cascade, 20, Some("joinA")),
        ] {
            let ensemble = Ensemble {
                id: format!("ens{suffix}"),
                spec_id: None,
                graph_id: Some("graph-1".to_string()),
                name: format!("Team {suffix}"),
                prompt_template: "go".to_string(),
                join_node_id: format!("join{suffix}"),
                entry_from_node: "kickoff".to_string(),
                entry_condition: GraphEdgeCondition::Always,
                min_pass: 1,
                straggler_timeout_minutes: None,
                quorum_grace_minutes: None,
                timeout_minutes: 30,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
                on_pass_to: "final".to_string(),
                on_fail_to: on_fail_to.map(str::to_string),
                kind,
                round_robin_index: (kind == EnsembleKind::RoundRobin).then_some(0),
                commit_rights: false,
                created_at: now,
                crew_id: None,
                crew_name: Some("reviewers".to_string()),
                crew_version_applied: None,
            };
            let join_node = GraphNode {
                id: format!("join{suffix}"),
                spec_id: None,
                graph_id: Some("graph-1".to_string()),
                name: format!("Team {suffix} (quorum)"),
                kind: GraphNodeKind::Join,
                config: serde_json::json!({"ensemble_id": format!("ens{suffix}")}),
                position,
                created_at: now,
            };
            let exit = GraphEdge {
                id: format!("join{suffix}->final"),
                spec_id: None,
                graph_id: Some("graph-1".to_string()),
                from_node: format!("join{suffix}"),
                to_node: "final".to_string(),
                condition: GraphEdgeCondition::Pass,
            };
            db.insert_ensemble_unit(&ensemble, &[], &[], &join_node, &[exit])
                .unwrap();
            db.sync_ensemble_with_crew(&ensemble.id).unwrap();
        }

        // Team A's extra `fail` entry source.
        db.add_ensemble_entry_source("ensA", "alt", &GraphEdgeCondition::Fail)
            .unwrap();
        // Team B's fail exit chains into A: the row holds A's quorum id, the
        // edges fan out to every A member.
        let a_members: Vec<String> = db
            .list_ensemble_members("ensA")
            .unwrap()
            .into_iter()
            .map(|member| member.node_id)
            .collect();
        db.retarget_ensemble_exit("ensB", &GraphEdgeCondition::Fail, &a_members, Some("joinA"))
            .unwrap();

        lp
    }

    /// `graph_export`'s document for a graph, reading the pending rows exactly
    /// as the real handler does.
    fn export_graph(db: &Database, graph_id: &str) -> GraphExportDocument {
        let lp = db.get_graph(graph_id).unwrap().unwrap();
        let nodes = db.list_graph_nodes_for_graph(graph_id).unwrap();
        let edges = db.list_graph_edges_for_graph(graph_id).unwrap();
        let ensembles = db.list_ensembles_for_graph(graph_id).unwrap();
        let pending = db.pending_entry_sources_for_graph(graph_id).unwrap();
        build_export_document_with_pending(&lp, &nodes, &edges, &ensembles, &pending).unwrap()
    }

    /// The same structural checks `graph_preflight` runs before spending probe
    /// quota: `validate_graph` (the entry-point check that reported "Graph has
    /// multiple entry points") and `validate_ensembles_in_graph` (every member
    /// wired in and both exits wired out).
    fn assert_graph_is_structurally_runnable(
        nodes: &[GraphNode],
        edges: &[GraphEdge],
        ensembles: &[EnsembleDetails],
    ) {
        let router_labels: Vec<Vec<String>> = nodes.iter().map(|_| Vec::new()).collect();
        let node_views: Vec<GraphNodeView<'_>> = nodes
            .iter()
            .enumerate()
            .map(|(idx, node)| GraphNodeView {
                id: &node.id,
                kind: node.kind,
                route_labels: &router_labels[idx],
            })
            .collect();
        let edge_views: Vec<GraphEdgeView<'_>> = edges
            .iter()
            .map(|edge| GraphEdgeView {
                from: &edge.from_node,
                to: &edge.to_node,
                condition: &edge.condition,
            })
            .collect();
        if let Err(e) = validate_graph(&node_views, &edge_views) {
            panic!("imported graph must report no entry-point error: {e}");
        }
        validate_ensembles_in_graph(ensembles, nodes, edges)
            .expect("imported ensembles must be fully wired");
    }

    /// Requirement 4: two crew-bound ensembles (a round_robin with an extra
    /// `fail` entry source, and a cascade chained by `on_fail_to` into it)
    /// survive export → import → export with identical edges and ensemble
    /// wiring, and the imported graph is structurally runnable. Exercised
    /// both for the document as exported and with `members` explicitly
    /// cleared.
    #[test]
    fn crew_bound_ensembles_round_trip_with_wiring() {
        let db = test_db();
        let lp = seed_crew_chain_graph(&db);
        let first = export_graph(&db, &lp.id);

        let team_a = first
            .ensembles
            .iter()
            .find(|ensemble| ensemble.name == "Team A")
            .expect("Team A");
        assert_eq!(team_a.crew.as_deref(), Some("reviewers"));
        assert!(
            team_a.members.is_empty(),
            "a crew-bound ensemble carries no members"
        );
        assert_eq!(
            team_a.extra_entry_sources,
            vec![GraphExportEntrySource {
                from_node: "alt".to_string(),
                condition: GraphEdgeCondition::Fail,
            }]
        );
        let team_b = first
            .ensembles
            .iter()
            .find(|ensemble| ensemble.name == "Team B")
            .expect("Team B");
        assert_eq!(
            team_b.on_fail_to,
            Some(GraphExportEnsembleTarget::Ensemble {
                ensemble: "Team A".to_string(),
            })
        );

        for (graph_id, clear_members) in [("graph-2", false), ("graph-3", true)] {
            let mut document = first.clone();
            if clear_members {
                for ensemble in &mut document.ensembles {
                    ensemble.members.clear();
                }
            }

            let lp = draft_graph(graph_id, "crew-chain", "/tmp/project");
            let plan = build_import_plan(&document, graph_id).unwrap();
            let missing = db.import_graph(&lp, &plan).unwrap();
            assert!(missing.is_empty(), "the crew exists locally");

            let nodes = db.list_graph_nodes_for_graph(graph_id).unwrap();
            let edges = db.list_graph_edges_for_graph(graph_id).unwrap();
            let ensembles = db.list_ensembles_for_graph(graph_id).unwrap();
            for details in &ensembles {
                assert_eq!(
                    details.members.len(),
                    2,
                    "{}'s members must materialise from the crew",
                    details.ensemble.name
                );
            }
            assert_graph_is_structurally_runnable(&nodes, &edges, &ensembles);

            // Every member is entered from the extra `fail` source, and B's
            // chained fail exit reaches all of A's members.
            let node_id = |name: &str| {
                nodes
                    .iter()
                    .find(|node| node.name == name)
                    .unwrap_or_else(|| panic!("node '{name}'"))
                    .id
                    .clone()
            };
            let alt = node_id("alt");
            let team_a = ensembles
                .iter()
                .find(|details| details.ensemble.name == "Team A")
                .expect("Team A");
            let team_b_join = ensembles
                .iter()
                .find(|details| details.ensemble.name == "Team B")
                .expect("Team B")
                .ensemble
                .join_node_id
                .clone();
            for member in &team_a.members {
                assert!(
                    edges.iter().any(|edge| {
                        edge.from_node == alt
                            && edge.to_node == member.node_id
                            && edge.condition == GraphEdgeCondition::Fail
                    }),
                    "A member must be entered from 'alt' on fail"
                );
                assert!(
                    edges.iter().any(|edge| {
                        edge.from_node == team_b_join
                            && edge.to_node == member.node_id
                            && edge.condition == GraphEdgeCondition::Fail
                    }),
                    "A member must be entered from Team B's chained fail exit"
                );
            }

            let redone = export_graph(&db, graph_id);
            assert_eq!(first.edges, redone.edges, "edges must round-trip");
            assert_eq!(
                first.ensembles, redone.ensembles,
                "ensemble wiring must round-trip"
            );
        }
    }

    /// Requirement 3: with the crew missing locally the import still records
    /// the ensemble's entry wiring; creating the crew and syncing wires a
    /// fully runnable ensemble, and the pending rows are then consumed.
    #[test]
    fn crew_bound_ensemble_without_crew_wires_on_later_sync() {
        let source = test_db();
        let lp = seed_crew_chain_graph(&source);
        let document = export_graph(&source, &lp.id);

        let target = test_db();
        let lp = draft_graph("graph-2", "crew-chain", "/tmp/project");
        let plan = build_import_plan(&document, "graph-2").unwrap();
        let missing = target.import_graph(&lp, &plan).unwrap();
        assert_eq!(missing, vec!["reviewers".to_string()]);

        // Both ensembles are pending: no members, but their entry wiring is
        // recorded.
        let ensembles = target.list_ensembles_for_graph("graph-2").unwrap();
        for details in &ensembles {
            assert!(details.members.is_empty(), "no crew, no members yet");
        }
        assert!(
            !target
                .pending_entry_sources_for_graph("graph-2")
                .unwrap()
                .is_empty(),
            "entry wiring must be recorded while the crew is missing"
        );

        // Create the crew; the next sync materialises and fully wires both.
        target
            .crew_create(
                "reviewers",
                None,
                &[crew_member("openrouter"), crew_member("opencode-go")],
            )
            .unwrap();
        for details in &target.list_ensembles_for_graph("graph-2").unwrap() {
            target
                .sync_ensemble_with_crew(&details.ensemble.id)
                .unwrap();
        }

        let nodes = target.list_graph_nodes_for_graph("graph-2").unwrap();
        let edges = target.list_graph_edges_for_graph("graph-2").unwrap();
        let ensembles = target.list_ensembles_for_graph("graph-2").unwrap();
        assert_graph_is_structurally_runnable(&nodes, &edges, &ensembles);

        let alt = nodes
            .iter()
            .find(|node| node.name == "alt")
            .expect("alt node")
            .id
            .clone();
        let team_a = ensembles
            .iter()
            .find(|details| details.ensemble.name == "Team A")
            .expect("Team A");
        assert_eq!(team_a.members.len(), 2);
        for member in &team_a.members {
            assert!(
                edges.iter().any(|edge| {
                    edge.from_node == alt
                        && edge.to_node == member.node_id
                        && edge.condition == GraphEdgeCondition::Fail
                }),
                "the recorded extra entry source must be wired after the sync"
            );
        }
        assert!(
            target
                .pending_entry_sources_for_graph("graph-2")
                .unwrap()
                .is_empty(),
            "pending rows are consumed once the members exist"
        );
    }

    /// Requirement 3 (export half): a crew-bound ensemble that is still pending
    /// — its crew absent, so it has no member nodes and no fan-out edges — must
    /// still export every wired entry source, read from the pending rows rather
    /// than the absent edges. The document that comes back is identical to the
    /// one a graph with the crews materialised exports, so re-importing it on a
    /// machine that has the crews is lossless.
    #[test]
    fn pending_crew_bound_ensemble_exports_its_recorded_wiring() {
        let source = test_db();
        let lp = seed_crew_chain_graph(&source);
        let first = export_graph(&source, &lp.id);

        // Import where the crew is missing: both ensembles stay pending.
        let pending = test_db();
        let lp = draft_graph("graph-2", "crew-chain", "/tmp/project");
        let plan = build_import_plan(&first, "graph-2").unwrap();
        let missing = pending.import_graph(&lp, &plan).unwrap();
        assert_eq!(missing, vec!["reviewers".to_string()]);
        for details in &pending.list_ensembles_for_graph("graph-2").unwrap() {
            assert!(details.members.is_empty(), "still pending: no members");
        }

        // Export while pending: the entry wiring must come from the recorded
        // rows, reproducing the source document exactly.
        let redone = export_graph(&pending, "graph-2");
        let team_a = redone
            .ensembles
            .iter()
            .find(|ensemble| ensemble.name == "Team A")
            .expect("Team A");
        assert_eq!(
            team_a.extra_entry_sources,
            vec![GraphExportEntrySource {
                from_node: "alt".to_string(),
                condition: GraphEdgeCondition::Fail,
            }],
            "the pending extra entry source must export from the recorded rows"
        );
        assert_eq!(
            first.edges, redone.edges,
            "edges must round-trip while pending"
        );
        assert_eq!(
            first.ensembles, redone.ensembles,
            "ensemble wiring must export from the pending rows"
        );
    }
}
