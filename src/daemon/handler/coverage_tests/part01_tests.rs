//! Part 01 of `coverage_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

// ── transport_details ──────────────────────────────────────────

#[test]
fn transport_details_streamable_http() {
    let (transport, port_str) = super::transport_details(8080);
    assert_eq!(transport, "Streamable HTTP");
    assert_eq!(port_str, "8080");
}

#[test]
fn transport_details_stdio() {
    let (transport, port_str) = super::transport_details(0);
    assert_eq!(transport, "stdio");
    assert_eq!(port_str, "N/A");
}

#[test]
fn transport_details_large_port() {
    let (transport, port_str) = super::transport_details(65535);
    assert_eq!(transport, "Streamable HTTP");
    assert_eq!(port_str, "65535");
}

// ── append_temporal_agents_section ──────────────────────────────

#[test]
fn append_temporal_empty_agents() {
    let mut status = "Canopy v0.1.0".to_string();
    super::append_temporal_agents_section(&mut status, &[]);
    assert_eq!(status, "Canopy v0.1.0");
}

#[test]
fn append_temporal_no_expiring() {
    let mut status = "status".to_string();
    let agents = vec![make_agent("a1")];
    super::append_temporal_agents_section(&mut status, &agents);
    assert_eq!(status, "status");
}

#[test]
fn append_temporal_active_expiry() {
    let mut status = "status".to_string();
    let mut a = make_agent("exp");
    a.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
    super::append_temporal_agents_section(&mut status, &[a]);
    assert!(status.contains("Temporal agents:"));
    assert!(status.contains("exp"));
    assert!(status.contains("remaining"));
}

#[test]
fn append_temporal_expired() {
    let mut status = "status".to_string();
    let mut a = make_agent("old");
    a.expires_at = Some(chrono::Utc::now() - chrono::Duration::minutes(5));
    super::append_temporal_agents_section(&mut status, &[a]);
    assert!(status.contains("EXPIRED"));
}

#[test]
fn append_temporal_skips_disabled() {
    let mut status = "status".to_string();
    let mut a = make_agent("dis");
    a.enabled = false;
    a.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
    super::append_temporal_agents_section(&mut status, &[a]);
    assert_eq!(status, "status");
}

#[test]
fn append_temporal_multiple_agents() {
    let mut status = String::new();
    let mut a1 = make_agent("a1");
    a1.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(10));
    let mut a2 = make_agent("a2");
    a2.expires_at = Some(chrono::Utc::now() + chrono::Duration::minutes(5));
    let a3 = make_agent("a3"); // no expiry
    super::append_temporal_agents_section(&mut status, &[a1, a2, a3]);
    assert!(status.contains("a1"));
    assert!(status.contains("a2"));
    assert!(!status.contains("a3"));
}

// ── graph_run_blocker ───────────────────────────────────────────

#[test]
fn blocker_present() {
    let run = GraphNodeRun {
        id: "r1".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"blocker": "needs review"})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert_eq!(
        super::graph_run_blocker(&run).as_deref(),
        Some("needs review")
    );
}

#[test]
fn blocker_no_output() {
    let run = GraphNodeRun {
        id: "r2".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Pass,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert!(super::graph_run_blocker(&run).is_none());
}

#[test]
fn blocker_no_blocker_key() {
    let run = GraphNodeRun {
        id: "r3".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"result": "failed"})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert!(super::graph_run_blocker(&run).is_none());
}

#[test]
fn blocker_non_string_value() {
    let run = GraphNodeRun {
        id: "r4".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"blocker": 42})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    assert!(super::graph_run_blocker(&run).is_none());
}

// ── graph_node_json ─────────────────────────────────────────────

#[test]
fn node_json_agent() {
    let node = GraphNode {
        id: "n1".into(),
        spec_id: Some("s1".into()),
        graph_id: None,
        name: "implement".into(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 5,
        created_at: chrono::Utc::now(),
    };
    let json = super::graph_node_json(&node, &[]);
    assert_eq!(json["id"], "n1");
    assert_eq!(json["spec_id"], "s1");
    assert!(json["graph_id"].is_null());
    assert_eq!(json["kind"], "agent");
    assert_eq!(json["position"], 5);
}

#[test]
fn node_json_join_displays_quorum() {
    let node = GraphNode {
        id: "j1".into(),
        spec_id: None,
        graph_id: Some("l1".into()),
        name: "quorum".into(),
        kind: GraphNodeKind::Join,
        config: serde_json::json!({}),
        position: 10,
        created_at: chrono::Utc::now(),
    };
    let json = super::graph_node_json(&node, &[]);
    assert_eq!(json["kind"], "quorum");
}

// ── graph_edge_json ─────────────────────────────────────────────

#[test]
fn edge_json_all_fields() {
    let edge = GraphEdge {
        id: "e1".into(),
        spec_id: Some("s1".into()),
        graph_id: None,
        from_node: "n1".into(),
        to_node: "n2".into(),
        condition: GraphEdgeCondition::Pass,
    };
    let json = super::graph_edge_json(&edge);
    assert_eq!(json["id"], "e1");
    assert_eq!(json["from_node"], "n1");
    assert_eq!(json["to_node"], "n2");
    assert_eq!(json["condition"], "pass");
}

#[test]
fn edge_json_graph_owner() {
    let edge = GraphEdge {
        id: "e2".into(),
        spec_id: None,
        graph_id: Some("l1".into()),
        from_node: "n1".into(),
        to_node: "n2".into(),
        condition: GraphEdgeCondition::Always,
    };
    let json = super::graph_edge_json(&edge);
    assert_eq!(json["graph_id"], "l1");
    assert!(json["spec_id"].is_null());
    assert_eq!(json["condition"], "always");
}

// ── graph_run_json ──────────────────────────────────────────────

#[test]
fn run_json_pass_with_output() {
    let run = GraphNodeRun {
        id: "r1".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Pass,
        input: Some(serde_json::json!({"key": "val"})),
        output: Some(serde_json::json!({"result": "ok"})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 3,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    let json = super::graph_run_json(&run);
    assert_eq!(json["status"], "pass");
    assert_eq!(json["iteration"], 3);
    assert_eq!(json["input"]["key"], "val");
    assert_eq!(json["output"]["result"], "ok");
}

#[test]
fn run_json_running_no_completed() {
    let run = GraphNodeRun {
        id: "r2".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Running,
        input: None,
        output: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        iteration: 1,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    let json = super::graph_run_json(&run);
    assert!(json["completed_at"].is_null());
}

#[test]
fn run_json_fail() {
    let run = GraphNodeRun {
        id: "r3".into(),
        graph_id: "l1".into(),
        spec_id: "s1".into(),
        node_id: "n1".into(),
        status: GraphRunStatus::Fail,
        input: None,
        output: Some(serde_json::json!({"blocker": "stuck"})),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        iteration: 2,
        pid: None,
        boot_id: None,
        session_id: None,
        executed_platform: None,
        executed_model: None,
    };
    let json = super::graph_run_json(&run);
    assert_eq!(json["status"], "fail");
    assert_eq!(json["iteration"], 2);
}

// ── ensemble_details_json ──────────────────────────────────────

#[test]
fn ensemble_json_full() {
    let details = EnsembleDetails {
        ensemble: Ensemble {
            mutex: Vec::new(),
            commit_rights: false,
            id: "ens1".into(),
            spec_id: Some("s1".into()),
            graph_id: None,
            name: "proposers".into(),
            prompt_template: "draft".into(),
            join_node_id: "j1".into(),
            entry_from_node: "kickoff".into(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 2,
            straggler_timeout_minutes: Some(10),
            quorum_grace_minutes: None,
            timeout_minutes: 30,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "arbiter".into(),
            on_fail_to: Some("cleanup".into()),
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: vec![
            EnsembleMember {
                ensemble_id: "ens1".into(),
                node_id: "m1".into(),
                position: 0,
                platform: "claude".into(),
                model: None,
                prompt_override: Some("review for security issues only".into()),
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
            EnsembleMember {
                ensemble_id: "ens1".into(),
                node_id: "m2".into(),
                position: 1,
                platform: "codex".into(),
                model: Some("o1".into()),
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            },
        ],
    };
    let json = super::ensemble_details_json(&details, &[]);
    assert_eq!(json["id"], "ens1");
    assert_eq!(json["min_pass"], 2);
    assert_eq!(json["effective_straggler_timeout_minutes"], 10);
    assert_eq!(json["on_fail_to"], "cleanup");
    assert_eq!(json["members"].as_array().unwrap().len(), 2);
    let members = json["members"].as_array().unwrap();
    assert_eq!(members[0]["prompt_source"], "override");
    assert_eq!(
        members[0]["prompt_override"],
        "review for security issues only"
    );
    assert_eq!(members[1]["prompt_source"], "shared");
    assert!(members[1]["prompt_override"].is_null());
}

#[test]
fn ensemble_json_no_straggler() {
    let details = EnsembleDetails {
        ensemble: Ensemble {
            mutex: Vec::new(),
            commit_rights: false,
            id: "ens2".into(),
            spec_id: None,
            graph_id: Some("l1".into()),
            name: "t".into(),
            prompt_template: "t".into(),
            join_node_id: "j".into(),
            entry_from_node: "f".into(),
            entry_condition: GraphEdgeCondition::Always,
            min_pass: 1,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: 45,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: "to".into(),
            on_fail_to: None,
            kind: EnsembleKind::Parallel,
            round_robin_index: None,
            created_at: chrono::Utc::now(),

            crew_id: None,
            crew_name: None,
            crew_version_applied: None,
        },
        members: vec![EnsembleMember {
            ensemble_id: "ens2".into(),
            node_id: "m1".into(),
            position: 0,
            platform: "claude".into(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        }],
    };
    let json = super::ensemble_details_json(&details, &[]);
    assert_eq!(json["effective_straggler_timeout_minutes"], 45);
    assert!(json["on_fail_to"].is_null());
}

// ── graph_completion_hook_json ──────────────────────────────────

#[test]
fn hook_json_full() {
    let hook = GraphCompletionHook {
        platform: Some("claude".into()),
        model: Some("opus-4".into()),
        effort: None,
        prompt: Some("{{graph_name}} done".into()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: Some(10),
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let json = super::graph_completion_hook_json(&hook);
    assert_eq!(json["platform"], "claude");
    assert_eq!(json["model"], "opus-4");
    assert_eq!(json["timeout_minutes"], 10);
}

#[test]
fn hook_json_no_model() {
    let hook = GraphCompletionHook {
        platform: Some("mimo".into()),
        model: None,
        effort: None,
        prompt: Some("t".into()),
        command: None,
        target_session_id: None,
        target_session_name: None,
        timeout_minutes: None,
        target_graph_id: None,
        queue_id: None,
        workdir_override: None,
        idea: None,

        crew: None,
    };
    let json = super::graph_completion_hook_json(&hook);
    assert!(json["model"].is_null());
    assert!(json["timeout_minutes"].is_null());
}

// ── graph_completion_hook_run_json ──────────────────────────────

#[test]
fn hook_run_json_full() {
    let run = GraphCompletionHookRun {
        id: "chr1".into(),
        graph_id: "l1".into(),
        event: crate::domain::graphs::GraphHookEvent::OnCompleted,
        hook_index: 0,
        status: GraphRunStatus::Pass,
        output: Some(serde_json::json!({"summary": "done"})),
        summary: Some("ok".into()),
        started_at: chrono::Utc::now(),
        completed_at: Some(chrono::Utc::now()),
        pid: Some(123),
        boot_id: None,
        executed_platform: None,
        executed_model: None,
        hook_config: None,
    };
    let json = super::graph_completion_hook_run_json(&run);
    assert_eq!(json["id"], "chr1");
    assert_eq!(json["status"], "pass");
    assert_eq!(json["summary"], "ok");
}

#[test]
fn hook_run_json_no_completed() {
    let run = GraphCompletionHookRun {
        id: "chr2".into(),
        graph_id: "l1".into(),
        event: crate::domain::graphs::GraphHookEvent::OnFailed,
        hook_index: 1,
        status: GraphRunStatus::Running,
        output: None,
        summary: None,
        started_at: chrono::Utc::now(),
        completed_at: None,
        pid: None,
        boot_id: None,
        executed_platform: None,
        executed_model: None,
        hook_config: None,
    };
    let json = super::graph_completion_hook_run_json(&run);
    assert!(json["completed_at"].is_null());
    assert!(json["summary"].is_null());
}

// ── format_system_time ─────────────────────────────────────────

#[test]
fn format_system_time_now() {
    let formatted = super::format_system_time(std::time::SystemTime::now());
    assert!(chrono::DateTime::parse_from_rfc3339(&formatted).is_ok());
}

// ── model_result_footer ────────────────────────────────────────

#[test]
fn footer_live_source() {
    use crate::daemon::handler_formatting::ModelTruncation;
    use crate::domain::models_db::CatalogSource;
    let f = super::model_result_footer(
        "Models:",
        CatalogSource::Live,
        std::time::SystemTime::now(),
        std::time::Duration::from_secs(24 * 60 * 60),
        &ModelTruncation::default(),
        false,
    );
    assert!(f.contains("Source:"));
    assert!(!f.contains("unreachable"));
    assert!(f.contains("age:"));
    assert!(f.contains("refresh interval"));
}

#[test]
fn footer_stale_source_says_how_stale_in_the_same_line() {
    use crate::daemon::handler_formatting::ModelTruncation;
    use crate::domain::models_db::CatalogSource;
    let fetched_at = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 3600);
    let f = super::model_result_footer(
        "Models:",
        CatalogSource::Stale,
        fetched_at,
        std::time::Duration::from_secs(24 * 60 * 60),
        &ModelTruncation::default(),
        false,
    );
    let source_line = f
        .lines()
        .find(|l| l.starts_with("Source:"))
        .expect("footer must have a Source line");
    assert!(source_line.contains("unreachable"));
    // How stale must be on the *same* line as the unreachable notice, not
    // just somewhere in the footer. 30h renders as "1d6h".
    assert!(source_line.contains("1d6h"));
}

#[test]
fn footer_fresh_cache_does_not_say_refresh_due() {
    use crate::daemon::handler_formatting::ModelTruncation;
    use crate::domain::models_db::CatalogSource;
    let f = super::model_result_footer(
        "Models:",
        CatalogSource::Cache,
        std::time::SystemTime::now() - std::time::Duration::from_secs(60),
        std::time::Duration::from_secs(24 * 60 * 60),
        &ModelTruncation::default(),
        false,
    );
    assert!(!f.contains("refresh due"));
    assert!(f.contains("age: 1m"));
}

#[test]
fn footer_states_refresh_due_when_age_exceeds_ttl() {
    use crate::daemon::handler_formatting::ModelTruncation;
    use crate::domain::models_db::CatalogSource;
    // A source other than Stale whose age has still crept past the TTL
    // (e.g. the TTL was lowered after the cache was written) must still
    // surface a due-for-refresh notice, not just the raw age.
    let fetched_at = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    let f = super::model_result_footer(
        "Models:",
        CatalogSource::Live,
        fetched_at,
        std::time::Duration::from_secs(3600),
        &ModelTruncation::default(),
        false,
    );
    assert!(f.contains("refresh due"));
}

#[test]
fn format_duration_short_renders_expected_units() {
    use std::time::Duration;
    assert_eq!(super::format_duration_short(Duration::from_secs(45)), "45s");
    assert_eq!(super::format_duration_short(Duration::from_secs(90)), "1m");
    assert_eq!(
        super::format_duration_short(Duration::from_secs(2 * 3600 + 15 * 60)),
        "2h15m"
    );
    assert_eq!(
        super::format_duration_short(Duration::from_secs(24 * 3600)),
        "1d"
    );
    assert_eq!(
        super::format_duration_short(Duration::from_secs(3 * 24 * 3600 + 4 * 3600)),
        "3d4h"
    );
}

// ── build_ensemble_unit: with and without on_fail_to ───────────

#[test]
fn ensemble_unit_with_fail_to() {
    let members = vec![
        ("claude".into(), None, None, None, None, None, None),
        (
            "codex".into(),
            Some("o1".into()),
            None,
            None,
            None,
            None,
            None,
        ),
    ];
    let built = build_ensemble_unit(&EnsembleUnitSpec {
        mutex: Vec::new(),
        spec_id: Some("s1".into()),
        graph_id: None,
        name: "failing",
        prompt_template: "do it",
        members: &members,
        entry_from_node: "kickoff",
        entry_condition: GraphEdgeCondition::Always,
        on_pass_to: "arbiter",
        on_pass_fan_out: None,
        on_fail_to: Some("cleanup"),
        on_fail_fan_out: None,
        min_pass: 1,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        straggler_timeout_minutes: Some(10),
        quorum_grace_minutes: None,
        start_position: 1,
        kind: EnsembleKind::Parallel,
        commit_rights: false,
    });
    assert_eq!(built.ensemble.on_fail_to.as_deref(), Some("cleanup"));
    assert_eq!(built.ensemble.straggler_timeout_minutes, Some(10));
    let fail_edges: Vec<_> = built
        .edges
        .iter()
        .filter(|e| e.condition == GraphEdgeCondition::Fail)
        .collect();
    assert_eq!(fail_edges.len(), 1);
}

#[test]
fn ensemble_unit_no_fail_to() {
    let members = vec![("claude".into(), None, None, None, None, None, None)];
    let built = build_ensemble_unit(&EnsembleUnitSpec {
        mutex: Vec::new(),
        spec_id: None,
        graph_id: Some("l1".into()),
        name: "no-fail",
        prompt_template: "p",
        members: &members,
        entry_from_node: "start",
        entry_condition: GraphEdgeCondition::Pass,
        on_pass_to: "end",
        on_pass_fan_out: None,
        on_fail_to: None,
        on_fail_fan_out: None,
        min_pass: 1,
        timeout_minutes: 15,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        start_position: 5,
        kind: EnsembleKind::Parallel,
        commit_rights: false,
    });
    assert!(built.ensemble.on_fail_to.is_none());
    let fail_edges: Vec<_> = built
        .edges
        .iter()
        .filter(|e| e.condition == GraphEdgeCondition::Fail)
        .collect();
    assert!(fail_edges.is_empty());
}

#[test]
fn ensemble_unit_positions_sequential() {
    let members = vec![
        ("p1".into(), None, None, None, None, None, None),
        ("p2".into(), None, None, None, None, None, None),
        ("p3".into(), None, None, None, None, None, None),
    ];
    let built = build_ensemble_unit(&EnsembleUnitSpec {
        mutex: Vec::new(),
        spec_id: None,
        graph_id: Some("l1".into()),
        name: "pos",
        prompt_template: "p",
        members: &members,
        entry_from_node: "start",
        entry_condition: GraphEdgeCondition::Always,
        on_pass_to: "end",
        on_pass_fan_out: None,
        on_fail_to: None,
        on_fail_fan_out: None,
        min_pass: 3,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        start_position: 10,
        kind: EnsembleKind::Parallel,
        commit_rights: false,
    });
    for (i, member) in built.members.iter().enumerate() {
        assert_eq!(member.position, i as i64);
    }
    for (i, node) in built.member_nodes.iter().enumerate() {
        assert_eq!(node.position, 10 + i as i64);
    }
    assert_eq!(built.join_node.position, 13);
}

// ── validate_position_conflict: no graph and empty graph ─────────

#[test]
fn position_conflict_no_graph() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    assert!(validate_position_conflict(&db, None, "spec-x", 1).is_ok());
}

#[test]
fn position_conflict_empty_graph() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "graph-empty");
    assert!(validate_position_conflict(&db, Some("graph-empty"), "spec-x", 0).is_ok());
}

// ── validate_node_position_conflict: edge cases ────────────────

#[test]
fn node_position_no_owner() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let node = GraphNode {
        id: "n1".into(),
        spec_id: None,
        graph_id: None,
        name: "n1".into(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    assert!(validate_node_position_conflict(&db, &node, "n1", 1).is_ok());
}

#[test]
fn node_position_empty_siblings() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("s1")).unwrap();
    let node = GraphNode {
        id: "n1".into(),
        spec_id: Some("s1".into()),
        graph_id: None,
        name: "n1".into(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    assert!(validate_node_position_conflict(&db, &node, "n1", 5).is_ok());
}

#[test]
fn node_position_no_conflict_different_position() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("s1")).unwrap();
    db.insert_graph_node(&GraphNode {
        id: "n1".into(),
        spec_id: Some("s1".into()),
        graph_id: None,
        name: "n1".into(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    let node = GraphNode {
        id: "n2".into(),
        spec_id: Some("s1".into()),
        graph_id: None,
        name: "n2".into(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 5,
        created_at: chrono::Utc::now(),
    };
    assert!(validate_node_position_conflict(&db, &node, "n2", 2).is_ok());
}

// ── validate_queue_not_consumed edge cases ──────────────────────

#[test]
fn queue_not_consumed_empty_queue() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-e".into(),
        name: "empty".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    assert!(validate_queue_not_consumed(&db, "queue-e", "graph-1").is_ok());
}

#[test]
fn queue_not_consumed_pending_spec() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("s1")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-p".into(),
        name: "queue-p".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-p", "s1", None).unwrap();
    assert!(validate_queue_not_consumed(&db, "queue-p", "graph-other").is_ok());
}

#[test]
fn queue_not_consumed_own_graph() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&running_spec("s-owned")).unwrap();
    db.insert_graph(&make_graph("graph-owner", GraphStatus::Running))
        .unwrap();
    db.insert_queue(&Queue {
        id: "queue-o".into(),
        name: "queue-o".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member("queue-o", "s-owned", None).unwrap();
    insert_test_node(&db, "node-1", "s-owned");
    db.insert_graph_run(&graph_run_row(
        "run1",
        "graph-owner",
        "s-owned",
        GraphRunStatus::Running,
    ))
    .unwrap();
    assert!(validate_queue_not_consumed(&db, "queue-o", "graph-owner").is_ok());
}

// ── validate_queue_member_removable edge cases ──────────────────

#[test]
fn queue_member_removable_pending() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("s1")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-r".into(),
        name: "queue-r".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    assert!(validate_queue_member_removable(&db, "queue-r", "s1").is_ok());
}

#[test]
fn queue_member_removable_nonexistent() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_queue(&Queue {
        id: "queue-r".into(),
        name: "queue-r".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    assert!(validate_queue_member_removable(&db, "queue-r", "ghost").is_ok());
}

// ── resolve_reported_run: stale statuses ───────────────────────

#[test]
fn reported_run_rejects_pass_status() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "l1");
    db.insert_graph_spec(&standalone_spec("s1")).unwrap();
    insert_test_node(&db, "node-1", "s1");
    db.insert_graph_run(&graph_run_row("r1", "l1", "s1", GraphRunStatus::Pass))
        .unwrap();
    let result = resolve_reported_run(&db, Some("r1"), Some("n1"), None).unwrap();
    assert!(result
        .expect_err("pass run must be stale")
        .is_error
        .unwrap_or(false));
}

#[test]
fn reported_run_rejects_fail_status() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    insert_test_graph(&db, "l1");
    db.insert_graph_spec(&standalone_spec("s1")).unwrap();
    insert_test_node(&db, "node-1", "s1");
    db.insert_graph_run(&graph_run_row("r1", "l1", "s1", GraphRunStatus::Fail))
        .unwrap();
    let result = resolve_reported_run(&db, Some("r1"), Some("n1"), None).unwrap();
    assert!(result
        .expect_err("fail run must be stale")
        .is_error
        .unwrap_or(false));
}

// ── validate_queue_reorder_locking edge cases ───────────────────

#[test]
fn reorder_locking_all_pending() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&standalone_spec("a")).unwrap();
    db.insert_graph_spec(&standalone_spec("b")).unwrap();
    db.insert_graph_spec(&standalone_spec("c")).unwrap();
    db.insert_queue(&Queue {
        id: "p1".into(),
        name: "p1".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for id in ["a", "b", "c"] {
        db.append_queue_member("p1", id, None).unwrap();
    }
    let current = db.list_queue_member_spec_ids("p1").unwrap();
    let order = vec!["c".into(), "a".into(), "b".into()];
    assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
}

#[test]
fn reorder_locking_failed_spec_locked() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut f = standalone_spec("f");
    f.status = GraphSpecStatus::Failed;
    db.insert_graph_spec(&f).unwrap();
    db.insert_graph_spec(&standalone_spec("p")).unwrap();
    db.insert_queue(&Queue {
        id: "p1".into(),
        name: "p1".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member("p1", "f", None).unwrap();
    db.append_queue_member("p1", "p", None).unwrap();
    let current = db.list_queue_member_spec_ids("p1").unwrap();
    let order = vec!["p".into(), "f".into()];
    let err = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
    assert!(err.contains("failed"), "{err}");
}

#[test]
fn reorder_locking_skipped_spec_locked() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut s = standalone_spec("s");
    s.status = GraphSpecStatus::Skipped;
    db.insert_graph_spec(&s).unwrap();
    db.insert_graph_spec(&standalone_spec("p")).unwrap();
    db.insert_queue(&Queue {
        id: "p1".into(),
        name: "p1".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member("p1", "s", None).unwrap();
    db.append_queue_member("p1", "p", None).unwrap();
    let current = db.list_queue_member_spec_ids("p1").unwrap();
    let order = vec!["p".into(), "s".into()];
    let err = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
    assert!(err.contains("skipped"), "{err}");
}

#[test]
fn reorder_locking_mixed_pending_and_running() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&running_spec("r")).unwrap();
    db.insert_graph_spec(&standalone_spec("p1")).unwrap();
    db.insert_graph_spec(&standalone_spec("p2")).unwrap();
    db.insert_queue(&Queue {
        id: "p1".into(),
        name: "p1".into(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member("p1", "r", None).unwrap();
    db.append_queue_member("p1", "p1", None).unwrap();
    db.append_queue_member("p1", "p2", None).unwrap();
    let current = db.list_queue_member_spec_ids("p1").unwrap();
    let order = vec!["r".into(), "p2".into(), "p1".into()];
    assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
}

// ── validate_queue_reorder: all permutations ────────────────────

#[test]
fn queue_reorder_all_perms_of_three() {
    let current = vec!["a".into(), "b".into(), "c".into()];
    for perm in [
        ["a", "b", "c"],
        ["a", "c", "b"],
        ["b", "a", "c"],
        ["b", "c", "a"],
        ["c", "a", "b"],
        ["c", "b", "a"],
    ] {
        let reordered: Vec<String> = perm.into_iter().map(String::from).collect();
        assert!(validate_queue_reorder(&current, &reordered).is_ok());
    }
}

#[test]
fn queue_reorder_empty() {
    assert!(validate_queue_reorder(&[], &[]).is_ok());
}

#[test]
fn queue_reorder_large_queue() {
    let current: Vec<String> = (0..100).map(|i| format!("s{i}")).collect();
    let mut reordered = current.clone();
    reordered.reverse();
    assert!(validate_queue_reorder(&current, &reordered).is_ok());
}

// ── validate_ensemble_members: boundaries ──────────────────────

#[test]
fn ensemble_exactly_min() {
    let m: Vec<EnsembleMemberParams> = (0..2)
        .map(|i| EnsembleMemberParams {
            platform: format!("p{i}"),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    assert!(validate_ensemble_members(
        &m,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default()
    )
    .is_ok());
}

#[test]
fn ensemble_exactly_max() {
    let m: Vec<EnsembleMemberParams> = (0..8)
        .map(|i| EnsembleMemberParams {
            platform: format!("p{i}"),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    assert!(validate_ensemble_members(
        &m,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default()
    )
    .is_ok());
}

#[test]
fn ensemble_above_max() {
    let m: Vec<EnsembleMemberParams> = (0..9)
        .map(|i| EnsembleMemberParams {
            platform: format!("p{i}"),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    assert!(validate_ensemble_members(
        &m,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default()
    )
    .unwrap_err()
    .contains("2-8"));
}

#[test]
fn ensemble_empty_model_becomes_none() {
    let m = vec![
        EnsembleMemberParams {
            platform: "claude".into(),
            model: Some("".into()),
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMemberParams {
            platform: "mimo".into(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    let result = validate_ensemble_members(
        &m,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap();
    assert_eq!(result[0].1, None);
    assert_eq!(result[1].1, None);
}

// ── validate_node_config: gate edge cases ─────────────────────

#[test]
fn gate_empty_evaluate_and_value() {
    let config = serde_json::json!({"evaluate": "", "value": ""});
    validate_node_config(GraphNodeKind::Gate, &config).unwrap();
}

// ── spec_summary_json: all statuses ────────────────────────────

#[test]
fn spec_summary_all_statuses() {
    for status in [
        GraphSpecStatus::Pending,
        GraphSpecStatus::Running,
        GraphSpecStatus::Completed,
        GraphSpecStatus::Failed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Interrupted,
    ] {
        let mut spec = standalone_spec("s");
        spec.status = status;
        let json = spec_summary_json(&spec, true);
        assert_eq!(json["status"], status.as_str());
    }
}

// ── validate_non_empty: unicode and special chars ───────────────

#[test]
fn non_empty_unicode() {
    assert!(validate_non_empty("こんにちは", "f").is_ok());
}

#[test]
fn non_empty_mixed_whitespace() {
    assert!(validate_non_empty(" \t\n ", "f").is_err());
}

// ── validate_absolute_dir: trailing slash ──────────────────────

#[test]
fn absolute_dir_trailing_slash() {
    let dir = tempdir().unwrap();
    let path = format!("{}/", dir.path().to_string_lossy());
    assert!(validate_absolute_dir(&path).is_ok());
}

// ── validate_spec_set_status_target: all valid ─────────────────

#[test]
fn set_status_target_all_valid() {
    assert!(validate_spec_set_status_target("pending").is_ok());
    assert!(validate_spec_set_status_target("completed").is_ok());
    assert!(validate_spec_set_status_target("skipped").is_ok());
}

// ── validate_at_least_one_bool: mixed ──────────────────────────

#[test]
fn at_least_one_mixed() {
    assert!(validate_at_least_one_bool(&[false, true, false, true], "f").is_ok());
    assert!(validate_at_least_one_bool(&[false, false, false, false], "f").is_err());
}

// ── build_id_result: various keys ──────────────────────────────

#[test]
fn id_result_spec_id() {
    let r = build_id_result("abc", "spec_id");
    let t = format!("{:?}", r.content);
    assert!(t.contains("spec_id") && t.contains("abc"));
}

#[test]
fn id_result_ensemble_id() {
    let r = build_id_result("ens-1", "ensemble_id");
    let t = format!("{:?}", r.content);
    assert!(t.contains("ensemble_id") && t.contains("ens-1"));
}

#[test]
fn id_result_queue_id() {
    let r = build_id_result("q-1", "queue_id");
    let t = format!("{:?}", r.content);
    assert!(t.contains("queue_id") && t.contains("q-1"));
}

// ── build_json_result: nested ──────────────────────────────────

#[test]
fn json_result_nested() {
    let v = serde_json::json!({"mapping": {"old": "new"}, "wired": true});
    let r = build_json_result(&v);
    let t = format!("{:?}", r.content);
    assert!(t.contains("mapping") && t.contains("old"));
}

// ── member_node_config edge cases ──────────────────────────────

#[test]
fn member_config_zero_timeout() {
    let c = member_node_config("claude", None, "p", 0, None, None, None);
    assert_eq!(c["timeout_minutes"], 0);
}

// ── resolve_graph_target: whitespace-only ──────────────────────

#[test]
fn graph_target_whitespace_both() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let err = resolve_graph_target(&db, Some("   "), Some("  ")).unwrap_err();
    assert!(err.contains("exactly one"), "{err}");
}

// ── rag_result_json: negative distance ─────────────────────────

#[test]
fn rag_negative_distance() {
    use crate::rag::vector_store::SearchResult;
    let r = SearchResult {
        id: "sr-neg".into(),
        file_path: "/t.md".into(),
        content: "c".into(),
        created_at: 0,
        distance: Some(-0.5),
    };
    let json = rag_result_json(&r);
    assert!(json["distance"].as_f64().unwrap() < 0.0);
}

// ── node_copy_note edge cases ──────────────────────────────────

#[test]
fn copy_note_wired() {
    let n = node_copy_note("src", "dst", true);
    assert!(n.contains("src") && n.contains("dst") && !n.contains("Unwired"));
}

#[test]
fn copy_note_unwired() {
    let n = node_copy_note("src", "dst", false);
    assert!(n.contains("Unwired") && n.contains("graph_add_edge"));
}

#[test]
fn spec_summary_json_compact_omits_description() {
    let mut spec = standalone_spec("spec-1");
    spec.description = Some("A long description".to_string());
    let json = spec_summary_json(&spec, false);
    assert!(
        json.get("description").is_none(),
        "compact mode must omit description"
    );
    assert_eq!(json["id"], "spec-1");
    assert_eq!(json["name"], "spec-1");
    assert_eq!(json["status"], "pending");
}

#[test]
fn queue_details_json_compact_members_have_position_and_group() {
    let spec = standalone_spec("spec-1");
    let queue = Queue {
        id: "q-1".to_string(),
        name: "test-queue".to_string(),
        created_at: chrono::Utc::now(),
    };
    let mut member_groups = std::collections::HashMap::new();
    member_groups.insert("spec-1".to_string(), Some("group-a".to_string()));
    let details = QueueDetails {
        queue,
        members: vec![spec],
        member_groups,
    };
    let json = queue_details_json(&details, false);
    let members = json["members"].as_array().unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0]["queue_position"], 1);
    assert_eq!(members[0]["group"], "group-a");
    assert!(
        members[0].get("description").is_none(),
        "compact mode must omit description"
    );
}
