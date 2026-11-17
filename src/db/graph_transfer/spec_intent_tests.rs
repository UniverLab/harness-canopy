//! A graph transfer carries no spec data — `intent` included.

use super::*;
use crate::domain::graph_transfer::{build_export_document, build_import_plan};
use crate::domain::graphs::{GraphNodeKind, GraphSpec, GraphSpecStatus, GraphStatus};
use chrono::Utc;

fn draft_graph(id: &str) -> Graph {
    Graph {
        trigger_prompt: None,
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: id.to_string(),
        name: "carrier".to_string(),
        description: None,
        workdir: "/tmp/project".to_string(),
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

fn local_spec() -> GraphSpec {
    GraphSpec {
        intent: Some("secret local intent".to_string()),
        id: "local-spec".to_string(),
        graph_id: Some("graph-1".to_string()),
        name: "Local spec".to_string(),
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    }
}

#[test]
fn export_and_import_carry_no_spec_data() {
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("state.db")).unwrap();
    db.insert_graph(&draft_graph("graph-1")).unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-1".to_string(),
        spec_id: None,
        graph_id: Some("graph-1".to_string()),
        name: "only".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: Utc::now(),
    })
    .unwrap();
    db.insert_graph_spec(&local_spec()).unwrap();

    let stored = db.get_graph("graph-1").unwrap().unwrap();
    let nodes = db.list_graph_nodes_for_graph("graph-1").unwrap();
    let edges = db.list_graph_edges_for_graph("graph-1").unwrap();
    let document = build_export_document(&stored, &nodes, &edges, &[]).unwrap();
    let json = serde_json::to_string(&document).unwrap();
    assert!(!json.contains("intent"), "export carried intent: {json}");
    assert!(
        !json.contains("Local spec") && !json.contains("local-spec"),
        "export carried a spec: {json}"
    );

    let imported = draft_graph("graph-2");
    let plan = build_import_plan(&document, &imported.id).unwrap();
    db.import_graph(&imported, &plan).unwrap();
    assert!(
        db.list_graph_specs("graph-2").unwrap().is_empty(),
        "an import must never materialize the source's specs"
    );
}
