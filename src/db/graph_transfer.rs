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

use anyhow::{anyhow, Result};
use rusqlite::{params, Transaction};

use crate::db::Database;
use crate::domain::graph_transfer::GraphImportPlan;
use crate::domain::graphs::{Graph, GraphEdge, GraphNode};

impl Database {
    pub fn import_graph(&self, lp: &Graph, plan: &GraphImportPlan) -> Result<()> {
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| anyhow!("Lock poisoned: {}", e))?;
        let tx = conn.transaction()?;

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
                Option::<String>::None,
            ],
        )?;

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
        for ensemble_plan in &plan.ensembles {
            let ensemble = &ensemble_plan.ensemble;
            tx.execute(
                "INSERT INTO ensembles (id, spec_id, graph_id, name, prompt_template, join_node_id, entry_from_node, entry_condition, min_pass, straggler_timeout_minutes, quorum_grace_minutes, timeout_minutes, on_pass_to, on_fail_to, commit_rights, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
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
                    ensemble.commit_rights,
                    ensemble.created_at.timestamp(),
                ],
            )?;
            for member in &ensemble_plan.members {
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
        }

        tx.commit()?;
        Ok(())
    }
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
        build_export_document, build_import_plan, GraphExportDocument,
    };
    use crate::domain::graphs::{GraphEdgeCondition, GraphNodeKind, GraphStatus};
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
            }],
            infra_node: None,
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
}
