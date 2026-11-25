use super::types::{App, Focus, SidebarLayer};
use super::AutomationKind;
use crate::db::Database;
use crate::domain::graphs::{Graph, GraphSpecStatus};

/// Compute `(done, total, current_spec_code, current_spec_intent)` for a graph card.
pub(crate) fn graph_card_fields(db: &Database, graph: &Graph) -> (usize, usize, String, String) {
    let entries = db
        .graph_queue_spec_states(&graph.id, graph.active_run_queue_id.as_deref())
        .unwrap_or_default();
    let done = entries
        .iter()
        .filter(|e| e.status == GraphSpecStatus::Completed)
        .count();
    let total = entries.len();

    let current = entries
        .iter()
        .find(|e| e.status == GraphSpecStatus::Running)
        .or_else(|| {
            entries.iter().find(|e| {
                matches!(
                    e.status,
                    GraphSpecStatus::Pending | GraphSpecStatus::Interrupted
                )
            })
        });

    let Some(cur) = current else {
        return (done, total, String::new(), String::new());
    };

    let code = cur.spec_code.clone().unwrap_or_default();
    let intent = db
        .get_graph_spec(&cur.spec_id)
        .ok()
        .flatten()
        .and_then(|s| s.intent)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cur.spec_name.clone());

    (done, total, code, intent)
}

/// Hit-test a click row against the drawn card map; on a hit select that
/// graph exactly like the sidebar graph click and claim the event.
pub(crate) fn graph_card_click(app: &mut App, row: u16) -> bool {
    let Some(graph_id) = app
        .graph_card_click_map
        .iter()
        .find(|(_, y0, y1)| row >= *y0 && row < *y1)
        .map(|(id, _, _)| id.clone())
    else {
        return false;
    };
    app.sidebar_layer = SidebarLayer::Automation;
    app.automation_kind = AutomationKind::Graph;
    app.selected_graph_id = Some(graph_id);
    app.refresh_graphs_selection();
    app.focus = Focus::Preview;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::graphs::{GraphSpec, GraphStatus, SpecReviewState};
    use chrono::Utc;
    use std::collections::BTreeMap;

    fn test_db() -> Database {
        let path = crate::test_support::temp_db_path();
        Database::new(&path).expect("create test db")
    }

    fn make_graph(id: &str) -> Graph {
        Graph {
            id: id.to_string(),
            name: "Grove".to_string(),
            description: None,
            workdir: "/tmp/grove".to_string(),
            status: GraphStatus::Running,
            trigger: None,
            trigger_prompt: None,
            created_at: Utc::now(),
            started_at: None,
            completed_at: None,
            autorun_at: None,
            auto_continue_at: None,
            auto_continue_action: None,
            active_run_queue_id: None,
            hooks: BTreeMap::new(),
            require_spec_type: false,
            archived: false,
            paused_by_reconciliation: false,
            allow_dirty_start: false,
            infra_node_id: None,
            enabled: true,
            enable_at: None,
            expires_at: None,
        }
    }

    fn make_spec(
        id: &str,
        graph_id: &str,
        name: &str,
        status: GraphSpecStatus,
        position: i64,
        code: Option<&str>,
        intent: Option<&str>,
    ) -> GraphSpec {
        GraphSpec {
            id: id.to_string(),
            graph_id: Some(graph_id.to_string()),
            name: name.to_string(),
            description: None,
            position,
            parallelizable: false,
            status,
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
            spec_code: code.map(str::to_string),
            spec_scope: None,
            spec_type: None,
            intent: intent.map(str::to_string),
            review_state: SpecReviewState::Approved,
            review_tier: None,
            review_findings: None,
            created_by_session: None,
        }
    }

    #[test]
    fn graph_card_fields_with_code_and_intent() {
        let db = test_db();
        let g = make_graph("g1");
        db.insert_graph(&g).unwrap();
        db.insert_graph_spec(&make_spec(
            "s1",
            "g1",
            "Done spec",
            GraphSpecStatus::Completed,
            0,
            Some("C-1"),
            Some("first"),
        ))
        .unwrap();
        db.insert_graph_spec(&make_spec(
            "s2",
            "g1",
            "Running spec",
            GraphSpecStatus::Running,
            1,
            Some("C-2"),
            Some("render cards"),
        ))
        .unwrap();

        let (done, total, code, intent) = graph_card_fields(&db, &g);
        assert_eq!(done, 1);
        assert_eq!(total, 2);
        assert_eq!(code, "C-2");
        assert_eq!(intent, "render cards");
    }

    #[test]
    fn graph_card_fields_falls_back_to_name_when_intent_absent() {
        let db = test_db();
        let g = make_graph("g1");
        db.insert_graph(&g).unwrap();
        db.insert_graph_spec(&make_spec(
            "s1",
            "g1",
            "Fallback Spec Name",
            GraphSpecStatus::Pending,
            0,
            None,
            None,
        ))
        .unwrap();

        let (done, total, code, intent) = graph_card_fields(&db, &g);
        assert_eq!(done, 0);
        assert_eq!(total, 1);
        assert_eq!(code, "");
        assert_eq!(intent, "Fallback Spec Name");
    }

    #[test]
    fn graph_card_fields_with_no_spec() {
        let db = test_db();
        let g = make_graph("g1");
        db.insert_graph(&g).unwrap();

        let (done, total, code, intent) = graph_card_fields(&db, &g);
        assert_eq!(done, 0);
        assert_eq!(total, 0);
        assert_eq!(code, "");
        assert_eq!(intent, "");
    }
}
