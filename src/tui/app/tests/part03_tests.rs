//! Part 03 of `tests` (split from `src/tui/app/mod.rs`, pure move).
use super::*;

#[test]
fn navigate_parent_follows_incoming_edge() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B")],
        &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
        Some("b"),
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("b".to_string());
    app.graph_live_navigate_parent();
    assert_eq!(app.graph_live_highlighted_node_id(), Some("a"));
}

#[test]
fn navigate_child_no_op_at_leaf() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(&[("a", "A")], &[], Some("a"));
    app.graph_live_state = Some(state);
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("a".to_string());
    app.graph_live_navigate_child();
    assert_eq!(
        app.graph_live_highlighted_node_id(),
        Some("a"),
        "leaf child should be no-op"
    );
}

#[test]
fn navigate_parent_no_op_at_root() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B")],
        &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
        Some("a"),
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("a".to_string());
    app.graph_live_navigate_parent();
    assert_eq!(
        app.graph_live_highlighted_node_id(),
        Some("a"),
        "root parent should be no-op"
    );
}

#[test]
fn navigate_child_with_no_highlight_selects_dfs_first() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B")],
        &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
        None,
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;

    app.graph_live_navigate_child();

    assert_eq!(
        app.graph_live_highlighted_node_id(),
        Some("a"),
        "no highlight: child enters at the DFS entry node"
    );
    assert!(!app.graph_live_follow);
}

#[test]
fn navigate_child_through_ensemble_member_target() {
    use crate::domain::graphs::{GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind};
    use crate::tui::app::graph_live_state::{EnsembleLiveInfo, EnsembleMemberLiveInfo};
    use chrono::Utc;
    use serde_json::json;

    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    let node = |id: &str, pos: i64| GraphNode {
        id: id.to_string(),
        spec_id: Some("s1".to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Agent,
        config: json!({}),
        position: pos,
        created_at: Utc::now(),
    };
    let edge = |id: &str, from: &str, to: &str| GraphEdge {
        id: id.to_string(),
        spec_id: Some("s1".to_string()),
        graph_id: None,
        from_node: from.to_string(),
        to_node: to.to_string(),
        condition: GraphEdgeCondition::Pass,
    };
    let state = GraphLiveState {
        graph_id: "lp1".to_string(),
        graph_name: "graph".to_string(),
        graph_status: GraphStatus::Running,
        workdir: "/tmp".to_string(),
        trigger_type: "manual".to_string(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        pause_waiting_for: Vec::new(),
        spec_queue: Vec::new(),
        done_count: 0,
        total_count: 0,
        current_spec_id: Some("s1".to_string()),
        effective_nodes: vec![
            node("start", 0),
            node("m1", 1),
            node("m2", 2),
            node("join1", 3),
        ],
        effective_edges: vec![
            edge("e1", "start", "m1"),
            edge("e2", "m1", "join1"),
            edge("e3", "m2", "join1"),
        ],
        ensembles: vec![EnsembleLiveInfo {
            ensemble_id: "ens1".to_string(),
            name: "E".to_string(),
            join_node_id: "join1".to_string(),
            members: vec![
                EnsembleMemberLiveInfo {
                    node_id: "m1".to_string(),
                    label: "claude".to_string(),
                    status: None,
                },
                EnsembleMemberLiveInfo {
                    node_id: "m2".to_string(),
                    label: "opencode".to_string(),
                    status: None,
                },
            ],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }],
        router_taken_routes: HashMap::new(),
        current_node_id: Some("start".to_string()),
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    };
    app.graph_live_state = Some(state);
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("start".to_string());

    // start -> m1 (a member) collapses to ens1, whose first member is m1.
    app.graph_live_navigate_child();
    assert_eq!(app.graph_live_highlighted_node_id(), Some("m1"));

    // From the ensemble member, the parent is the external start node.
    app.graph_live_navigate_parent();
    assert_eq!(app.graph_live_highlighted_node_id(), Some("start"));
}

#[test]
fn navigate_sibling_follows_dfs_order() {
    // Graph: a pass->b, a fail->c, b pass->d
    // DFS order should be a,b,d,c
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B"), ("c", "C"), ("d", "D")],
        &[
            ("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass),
            ("a", "c", crate::domain::graphs::GraphEdgeCondition::Fail),
            ("b", "d", crate::domain::graphs::GraphEdgeCondition::Pass),
        ],
        Some("a"),
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("a".to_string());
    let expected = vec!["b", "d", "c"];
    for exp in expected {
        app.graph_live_navigate_sibling(true);
        assert_eq!(
            app.graph_live_highlighted_node_id(),
            Some(exp),
            "sibling order mismatch"
        );
    }
    // wrap? sibling move_index wraps, so next should go to a again
    app.graph_live_navigate_sibling(true);
    assert_eq!(app.graph_live_highlighted_node_id(), Some("a"));
}

#[test]
fn graph_live_enter_seeds_from_most_recent_run_on_finished_graph() {
    // CT24 FR1 (b): on a finished graph with recorded runs, Enter seeds
    // the manual selection at the most recent run's node of the
    // last-queued (or strip-selected) spec — not the entry node.
    use crate::domain::graphs::{
        Graph, GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphNodeRun,
        GraphRunStatus, GraphSpec, GraphSpecStatus, GraphStatus,
    };
    let db = test_db();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        infra_node_id: None,
        id: "lp1".to_string(),
        name: "Nightly review".to_string(),
        description: None,
        workdir: "/tmp".to_string(),
        status: GraphStatus::Completed,
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
    db.insert_graph_spec(&GraphSpec {
        id: "s1".to_string(),
        graph_id: Some("lp1".to_string()),
        name: "Spec".to_string(),
        description: None,
        position: 0,
        parallelizable: false,
        status: GraphSpecStatus::Completed,
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
    .unwrap();
    for (position, id) in ["n1", "n2"].iter().enumerate() {
        db.insert_graph_node(&GraphNode {
            id: id.to_string(),
            spec_id: None,
            graph_id: Some("lp1".to_string()),
            name: id.to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({}),
            position: position as i64,
            created_at: chrono::Utc::now(),
        })
        .unwrap();
    }
    db.insert_graph_edge(&GraphEdge {
        id: "e1".to_string(),
        spec_id: None,
        graph_id: Some("lp1".to_string()),
        from_node: "n1".to_string(),
        to_node: "n2".to_string(),
        condition: GraphEdgeCondition::Pass,
    })
    .unwrap();
    // One completed run on n2 for s1: the "most recent run" the seed
    // must prefer over the entry node.
    db.insert_graph_run(&GraphNodeRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: "lp1".to_string(),
        spec_id: "s1".to_string(),
        node_id: "n2".to_string(),
        status: GraphRunStatus::Pass,
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

    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let details = db
        .get_graph_details("lp1")
        .ok()
        .flatten()
        .expect("seeded graph resolves");
    app.graph_live_state =
        crate::tui::app::graph_live_state::assemble_graph_live_state(&db, &details);
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;
    app.graph_spec_strip_selected = None;

    app.graph_live_enter();
    assert!(!app.graph_live_follow, "Enter must switch to manual mode");
    assert_eq!(
        app.graph_live_selected_node.as_deref(),
        Some("n2"),
        "seed must be the most recent run's node, not the entry node"
    );
}

#[test]
fn graph_live_enter_is_noop_when_not_following() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_live_follow = false;
    app.graph_live_selected_node = Some("kept".to_string());

    app.graph_live_enter();

    assert!(!app.graph_live_follow, "already manual: no change");
    assert_eq!(app.graph_live_selected_node.as_deref(), Some("kept"));
}

#[test]
fn graph_live_enter_prefers_current_node_on_running_graph() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B")],
        &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
        Some("b"),
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;

    app.graph_live_enter();

    assert!(!app.graph_live_follow);
    assert_eq!(
        app.graph_live_selected_node.as_deref(),
        Some("b"),
        "running graph seeds the engine's current node"
    );
}

#[test]
fn graph_live_enter_falls_back_to_dfs_entry_on_finished_graph_without_runs() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    let state = graph_state(
        &[("a", "A"), ("b", "B")],
        &[("a", "b", crate::domain::graphs::GraphEdgeCondition::Pass)],
        None,
    );
    app.graph_live_state = Some(state);
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;
    app.graph_spec_strip_selected = None;

    app.graph_live_enter();

    assert_eq!(
        app.graph_live_selected_node.as_deref(),
        Some("a"),
        "no runs: fall back to the DFS entry node"
    );
    assert!(!app.graph_live_follow);
}

#[test]
fn graph_live_enter_keeps_follow_when_graph_has_no_nodes() {
    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");
    app.graph_live_state = Some(graph_state(&[], &[], None));
    app.graph_live_follow = true;
    app.graph_live_selected_node = None;

    app.graph_live_enter();

    assert!(app.graph_live_follow, "no nodes: stay in auto-follow");
    assert!(app.graph_live_selected_node.is_none());
}

// ── dfs tests ───────────────────────────────────────────────────────

#[test]
fn dfs_endpoint_resolves_join_node_to_ensemble_id() {
    use crate::tui::app::graph_live_state::EnsembleLiveInfo;
    use std::collections::HashMap;

    let ens = EnsembleLiveInfo {
        ensemble_id: "ens1".into(),
        name: "Ensemble 1".into(),
        join_node_id: "join1".into(),
        members: vec![],
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
    };
    let ensemble_by_join: HashMap<&str, &EnsembleLiveInfo> = [("join1", &ens)].into();
    let member_to_ensemble: HashMap<String, String> = HashMap::new();
    let collapsed: HashMap<String, (bool, i64)> = HashMap::new();

    let result = App::dfs_endpoint("join1", &ensemble_by_join, &member_to_ensemble, &collapsed);
    assert_eq!(result, Some("ens1".into()));
}

#[test]
fn dfs_endpoint_resolves_ensemble_member_to_ensemble_id() {
    use crate::tui::app::graph_live_state::EnsembleLiveInfo;
    use std::collections::HashMap;

    let ens = EnsembleLiveInfo {
        ensemble_id: "ens1".into(),
        name: "Ensemble 1".into(),
        join_node_id: "join1".into(),
        members: vec![crate::tui::app::graph_live_state::EnsembleMemberLiveInfo {
            node_id: "m1".into(),
            label: "claude".into(),
            status: None,
        }],
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
    };
    let ensemble_by_join: HashMap<&str, &EnsembleLiveInfo> = [("join1", &ens)].into();
    let mut member_to_ensemble: HashMap<String, String> = HashMap::new();
    member_to_ensemble.insert("m1".into(), "ens1".into());
    let collapsed: HashMap<String, (bool, i64)> = HashMap::new();

    let result = App::dfs_endpoint("m1", &ensemble_by_join, &member_to_ensemble, &collapsed);
    assert_eq!(result, Some("ens1".into()));
}

#[test]
fn dfs_endpoint_resolves_regular_node_in_collapsed() {
    use std::collections::HashMap;

    let ensemble_by_join: HashMap<&str, &crate::tui::app::graph_live_state::EnsembleLiveInfo> =
        HashMap::new();
    let member_to_ensemble: HashMap<String, String> = HashMap::new();
    let mut collapsed: HashMap<String, (bool, i64)> = HashMap::new();
    collapsed.insert("regular".into(), (false, 0));

    let result = App::dfs_endpoint(
        "regular",
        &ensemble_by_join,
        &member_to_ensemble,
        &collapsed,
    );
    assert_eq!(result, Some("regular".into()));
}

#[test]
fn dfs_endpoint_returns_none_for_unknown_node() {
    use std::collections::HashMap;

    let ensemble_by_join: HashMap<&str, &crate::tui::app::graph_live_state::EnsembleLiveInfo> =
        HashMap::new();
    let member_to_ensemble: HashMap<String, String> = HashMap::new();
    let collapsed: HashMap<String, (bool, i64)> = HashMap::new();

    let result = App::dfs_endpoint(
        "unknown",
        &ensemble_by_join,
        &member_to_ensemble,
        &collapsed,
    );
    assert_eq!(result, None);
}

/// dfs_order collapses ensemble members to the first member's node id
#[test]
fn dfs_order_collapses_ensemble_members_to_first_member() {
    use crate::domain::graphs::{
        GraphEdge, GraphEdgeCondition, GraphNode, GraphNodeKind, GraphSpecStatus,
    };
    use crate::tui::app::graph_live_state::{
        EnsembleLiveInfo, EnsembleMemberLiveInfo, GraphLiveState, SpecQueueEntry,
    };
    use chrono::Utc;
    use std::collections::HashMap;

    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let mut app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    let state = GraphLiveState {
        graph_id: "g1".into(),
        graph_name: "Test".into(),
        graph_status: GraphStatus::Running,
        workdir: "/tmp".into(),
        trigger_type: "manual".into(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        pause_waiting_for: Vec::new(),
        spec_queue: vec![SpecQueueEntry {
            spec_id: "s1".into(),
            spec_name: "S1".into(),
            status: GraphSpecStatus::Running,
            failure_reason: None,
            spec_code: None,
            spec_type: None,
            spec_scope: None,
        }],
        done_count: 0,
        total_count: 1,
        current_spec_id: Some("s1".into()),
        effective_nodes: vec![
            GraphNode {
                id: "m1".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                name: "Member 1".into(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "m2".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                name: "Member 2".into(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 1,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "join1".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                name: "Join".into(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 2,
                created_at: Utc::now(),
            },
        ],
        effective_edges: vec![
            GraphEdge {
                id: "e1".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                from_node: "m1".into(),
                to_node: "m2".into(),
                condition: GraphEdgeCondition::Pass,
            },
            GraphEdge {
                id: "e2".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                from_node: "m2".into(),
                to_node: "join1".into(),
                condition: GraphEdgeCondition::Pass,
            },
        ],
        ensembles: vec![EnsembleLiveInfo {
            ensemble_id: "ens1".into(),
            name: "Ensemble 1".into(),
            join_node_id: "join1".into(),
            members: vec![
                EnsembleMemberLiveInfo {
                    node_id: "m1".into(),
                    label: "claude".into(),
                    status: None,
                },
                EnsembleMemberLiveInfo {
                    node_id: "m2".into(),
                    label: "opencode".into(),
                    status: None,
                },
            ],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }],
        router_taken_routes: HashMap::new(),
        current_node_id: Some("m1".into()),
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    };
    app.graph_live_state = Some(state);

    let order = app.dfs_order();
    // Ensemble members should be collapsed to first member (m1)
    // The final order contains concrete ids, so ens1 -> m1
    assert!(order.contains(&"m1".to_string()));
    // m2 and join1 should not appear in final order
    assert!(!order.contains(&"m2".to_string()));
    assert!(!order.contains(&"join1".to_string()));
    // m1 should appear exactly once
    assert_eq!(order.iter().filter(|n| *n == "m1").count(), 1);
}

/// collapse_nodes skips join node ids
#[test]
fn collapse_nodes_skips_join_node_ids() {
    use crate::domain::graphs::{GraphNode, GraphNodeKind};
    use crate::tui::app::graph_live_state::{
        EnsembleLiveInfo, EnsembleMemberLiveInfo, GraphLiveState,
    };
    use chrono::Utc;

    let db = test_db();
    let data_dir = tempdir().expect("create data dir");
    let _app = App::new(
        Arc::clone(&db),
        data_dir.path(),
        &crate::domain::canopy_config::CanopyConfig::default(),
    )
    .expect("create app");

    let state = GraphLiveState {
        graph_id: "g1".into(),
        graph_name: "Test".into(),
        graph_status: GraphStatus::Running,
        workdir: "/tmp".into(),
        trigger_type: "manual".into(),
        schedule_expr: None,
        watch_path: None,
        autorun_at: None,
        pause_waiting_for: Vec::new(),
        spec_queue: vec![],
        done_count: 0,
        total_count: 0,
        current_spec_id: None,
        effective_nodes: vec![
            GraphNode {
                id: "m1".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                name: "Member 1".into(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 0,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "join1".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                name: "Join".into(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 1,
                created_at: Utc::now(),
            },
            GraphNode {
                id: "regular".into(),
                spec_id: Some("s1".into()),
                graph_id: Some("g1".into()),
                name: "Regular".into(),
                kind: GraphNodeKind::Agent,
                config: serde_json::json!({}),
                position: 2,
                created_at: Utc::now(),
            },
        ],
        effective_edges: vec![],
        ensembles: vec![EnsembleLiveInfo {
            ensemble_id: "ens1".into(),
            name: "Ensemble 1".into(),
            join_node_id: "join1".into(),
            members: vec![EnsembleMemberLiveInfo {
                node_id: "m1".into(),
                label: "claude".into(),
                status: None,
            }],
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
        }],
        router_taken_routes: std::collections::HashMap::new(),
        current_node_id: None,
        current_node_status: None,
        current_node_started_at: None,
        current_node_iteration: None,
    };

    let collapsed = App::collapse_nodes(&state);
    // join1 should be skipped
    assert!(!collapsed.contains_key("join1"));
    // m1 should be collapsed into ensemble
    assert!(!collapsed.contains_key("m1"));
    // ens1 should be present
    assert!(collapsed.contains_key("ens1"));
    // regular node should be present
    assert!(collapsed.contains_key("regular"));
    assert!(collapsed["ens1"].0); // is_ensemble
    assert!(!collapsed["regular"].0); // not ensemble
}
