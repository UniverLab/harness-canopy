//! Part 01 of `tests` (split from `src/domain/graphs.rs`, pure move).
use super::*;

#[test]
fn graph_status_as_str_roundtrip() {
    assert_eq!(GraphStatus::Draft.as_str(), "draft");
    assert_eq!(GraphStatus::Running.as_str(), "running");
    assert_eq!(GraphStatus::Pausing.as_str(), "pausing");
    assert_eq!(GraphStatus::Paused.as_str(), "paused");
    assert_eq!(GraphStatus::Completed.as_str(), "completed");
    assert_eq!(GraphStatus::Failed.as_str(), "failed");
}

#[test]
fn graph_status_from_str() {
    assert_eq!(GraphStatus::from_str("running"), GraphStatus::Running);
    assert_eq!(GraphStatus::from_str("pausing"), GraphStatus::Pausing);
    assert_eq!(GraphStatus::from_str("paused"), GraphStatus::Paused);
    assert_eq!(GraphStatus::from_str("completed"), GraphStatus::Completed);
    assert_eq!(GraphStatus::from_str("failed"), GraphStatus::Failed);
    assert_eq!(GraphStatus::from_str("invalid"), GraphStatus::Draft);
}

#[test]
fn graph_spec_status_as_str() {
    assert_eq!(GraphSpecStatus::Pending.as_str(), "pending");
    assert_eq!(GraphSpecStatus::Running.as_str(), "running");
    assert_eq!(GraphSpecStatus::Completed.as_str(), "completed");
    assert_eq!(GraphSpecStatus::Failed.as_str(), "failed");
    assert_eq!(GraphSpecStatus::Skipped.as_str(), "skipped");
    assert_eq!(GraphSpecStatus::Interrupted.as_str(), "interrupted");
}

#[test]
fn graph_spec_status_from_str() {
    assert_eq!(
        GraphSpecStatus::from_str("running"),
        GraphSpecStatus::Running
    );
    assert_eq!(
        GraphSpecStatus::from_str("completed"),
        GraphSpecStatus::Completed
    );
    assert_eq!(GraphSpecStatus::from_str("failed"), GraphSpecStatus::Failed);
    assert_eq!(
        GraphSpecStatus::from_str("skipped"),
        GraphSpecStatus::Skipped
    );
    assert_eq!(
        GraphSpecStatus::from_str("interrupted"),
        GraphSpecStatus::Interrupted
    );
    // An unknown status (e.g. written by a newer binary) must never
    // silently read as `Completed` — that would skip work that hasn't
    // actually run. `Pending` is the only safe fallback.
    assert_eq!(
        GraphSpecStatus::from_str("invalid"),
        GraphSpecStatus::Pending
    );
}

#[test]
fn graph_node_kind_as_str() {
    assert_eq!(GraphNodeKind::Agent.as_str(), "agent");
    assert_eq!(GraphNodeKind::Check.as_str(), "check");
    assert_eq!(GraphNodeKind::Gate.as_str(), "gate");
    assert_eq!(GraphNodeKind::Join.as_str(), "join");
}

#[test]
fn graph_node_kind_from_str() {
    assert_eq!(GraphNodeKind::from_str("agent"), Some(GraphNodeKind::Agent));
    assert_eq!(GraphNodeKind::from_str("check"), Some(GraphNodeKind::Check));
    assert_eq!(GraphNodeKind::from_str("gate"), Some(GraphNodeKind::Gate));
    assert_eq!(GraphNodeKind::from_str("join"), Some(GraphNodeKind::Join));
    assert!(GraphNodeKind::from_str("invalid").is_none());
}

#[test]
fn graph_node_kind_display_str() {
    assert_eq!(GraphNodeKind::Agent.display_str(), "agent");
    assert_eq!(GraphNodeKind::Check.display_str(), "check");
    assert_eq!(GraphNodeKind::Gate.display_str(), "gate");
    assert_eq!(GraphNodeKind::Join.display_str(), "quorum");
}

#[test]
fn ensemble_straggler_timeout_defaults_to_member_timeout() {
    let ensemble = super::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some("spec1".to_string()),
        graph_id: None,
        name: "Proposers".to_string(),
        prompt_template: "{{spec_content}}".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "n0".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 2,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: super::EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: chrono::Utc::now(),

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    assert_eq!(ensemble.effective_straggler_timeout_minutes(), 30);

    let mut overridden = ensemble;
    overridden.straggler_timeout_minutes = Some(5);
    assert_eq!(overridden.effective_straggler_timeout_minutes(), 5);
}

/// CM24: `quorum_grace_minutes` defaults to `None` (wait-for-all), and a
/// set value is carried verbatim — there is no `effective_*` helper.
#[test]
fn ensemble_quorum_grace_none_by_default() {
    let ensemble = super::Ensemble {
        mutex: Vec::new(),
        commit_rights: false,
        id: "ens1".to_string(),
        spec_id: Some("spec1".to_string()),
        graph_id: None,
        name: "Proposers".to_string(),
        prompt_template: "{{spec_content}}".to_string(),
        join_node_id: "join1".to_string(),
        entry_from_node: "n0".to_string(),
        entry_condition: GraphEdgeCondition::Always,
        min_pass: 2,
        straggler_timeout_minutes: None,
        quorum_grace_minutes: None,
        timeout_minutes: 30,
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: super::EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: chrono::Utc::now(),

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    assert_eq!(ensemble.quorum_grace_minutes, None);

    let mut with_grace = ensemble;
    with_grace.quorum_grace_minutes = Some(0);
    assert_eq!(with_grace.quorum_grace_minutes, Some(0));
}

#[test]
fn graph_edge_condition_as_str() {
    assert_eq!(GraphEdgeCondition::Pass.as_str(), "pass");
    assert_eq!(GraphEdgeCondition::Fail.as_str(), "fail");
    assert_eq!(GraphEdgeCondition::Always.as_str(), "always");
}

#[test]
fn graph_edge_condition_from_str() {
    assert_eq!(
        GraphEdgeCondition::from_str("pass"),
        Some(GraphEdgeCondition::Pass)
    );
    assert_eq!(
        GraphEdgeCondition::from_str("fail"),
        Some(GraphEdgeCondition::Fail)
    );
    assert_eq!(
        GraphEdgeCondition::from_str("always"),
        Some(GraphEdgeCondition::Always)
    );
    assert!(GraphEdgeCondition::from_str("invalid").is_none());
}

#[test]
fn graph_run_status_as_str() {
    assert_eq!(GraphRunStatus::Running.as_str(), "running");
    assert_eq!(GraphRunStatus::Pass.as_str(), "pass");
    assert_eq!(GraphRunStatus::Fail.as_str(), "fail");
    assert_eq!(GraphRunStatus::Interrupted.as_str(), "interrupted");
}

#[test]
fn graph_run_status_from_str() {
    assert_eq!(GraphRunStatus::from_str("pass"), GraphRunStatus::Pass);
    assert_eq!(GraphRunStatus::from_str("fail"), GraphRunStatus::Fail);
    assert_eq!(
        GraphRunStatus::from_str("interrupted"),
        GraphRunStatus::Interrupted
    );
    assert_eq!(GraphRunStatus::from_str("invalid"), GraphRunStatus::Running);
}

#[test]
fn manual_graph_has_no_schedule_and_is_not_cron_or_watch() {
    let lp = graph_with_trigger(GraphStatus::Draft, None);
    assert_eq!(lp.trigger_type_label(), "manual");
    assert_eq!(lp.schedule_expr(), None);
    assert!(!lp.is_cron());
    assert!(!lp.is_watch());
    assert_eq!(lp.watch_path(), None);
}

#[test]
fn cron_graph_exposes_schedule_expr() {
    let lp = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Cron {
            schedule_expr: "30 8 * * *".to_string(),
        }),
    );
    assert_eq!(lp.trigger_type_label(), "cron");
    assert_eq!(lp.schedule_expr(), Some("30 8 * * *"));
    assert!(lp.is_cron());
    assert!(!lp.is_watch());
}

#[test]
fn watch_graph_exposes_path_and_events() {
    let lp = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Watch {
            path: "/tmp/watch".to_string(),
            events: vec![super::WatchEvent::Create],
            debounce_seconds: 2,
            recursive: false,
        }),
    );
    assert_eq!(lp.trigger_type_label(), "watch");
    assert!(lp.is_watch());
    assert_eq!(lp.watch_path(), Some("/tmp/watch"));
    assert_eq!(lp.watch_events(), Some(&[super::WatchEvent::Create][..]));
}

#[test]
fn running_or_paused_graph_is_not_fireable() {
    // A trigger must not relaunch a graph that is already executing.
    assert!(!graph_with_trigger(GraphStatus::Running, None).is_fireable());
    assert!(!graph_with_trigger(GraphStatus::Paused, None).is_fireable());
    assert!(graph_with_trigger(GraphStatus::Draft, None).is_fireable());
    assert!(graph_with_trigger(GraphStatus::Completed, None).is_fireable());
    assert!(graph_with_trigger(GraphStatus::Failed, None).is_fireable());
}

#[test]
fn future_autorun_at_is_not_due() {
    let mut lp = graph_with_trigger(GraphStatus::Failed, None);
    lp.autorun_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
    assert!(!lp.is_autorun_due(chrono::Utc::now()));
}

#[test]
fn past_autorun_at_is_due_on_a_fireable_graph() {
    let mut lp = graph_with_trigger(GraphStatus::Failed, None);
    lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    assert!(lp.is_autorun_due(chrono::Utc::now()));
}

#[test]
fn no_autorun_at_is_never_due() {
    let lp = graph_with_trigger(GraphStatus::Failed, None);
    assert!(!lp.is_autorun_due(chrono::Utc::now()));
}

#[test]
fn past_autorun_at_is_not_due_while_running_or_paused() {
    for status in [GraphStatus::Running, GraphStatus::Paused] {
        let mut lp = graph_with_trigger(status, None);
        lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        assert!(
            !lp.is_autorun_due(chrono::Utc::now()),
            "{status:?} graph must not fire autorun_at"
        );
    }
}

/// C1: a graph reconciliation paused after an unclean daemon exit must
/// still fire its pending, due `autorun_at` — that schedule is the
/// resilience node's one shot at an unattended quota-reset resume, and
/// nobody asked for the graph to stop.
#[test]
fn past_autorun_at_is_due_on_a_reconciliation_paused_graph() {
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.paused_by_reconciliation = true;
    lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    assert!(lp.is_autorun_due(chrono::Utc::now()));
}

/// C1: the operator's own pause must keep blocking the schedule even
/// though a reconciliation pause no longer does — `paused_by_reconciliation`
/// is what tells the two apart, not `Paused` alone.
#[test]
fn past_autorun_at_is_not_due_on_an_operator_paused_graph() {
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.paused_by_reconciliation = false;
    lp.autorun_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    assert!(!lp.is_autorun_due(chrono::Utc::now()));
}

#[test]
fn past_auto_continue_at_is_due_while_paused() {
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.auto_continue_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    assert!(lp.is_auto_continue_due(chrono::Utc::now()));
}

#[test]
fn future_auto_continue_at_is_not_due() {
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.auto_continue_at = Some(chrono::Utc::now() + chrono::Duration::hours(1));
    assert!(!lp.is_auto_continue_due(chrono::Utc::now()));
}

#[test]
fn no_auto_continue_at_is_never_due() {
    let lp = graph_with_trigger(GraphStatus::Paused, None);
    assert!(!lp.is_auto_continue_due(chrono::Utc::now()));
}

#[test]
fn past_auto_continue_at_is_not_due_while_not_paused() {
    for status in [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ] {
        let mut lp = graph_with_trigger(status, None);
        lp.auto_continue_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
        assert!(
            !lp.is_auto_continue_due(chrono::Utc::now()),
            "{status:?} graph must not fire auto_continue_at"
        );
        assert!(
            lp.is_auto_continue_time_reached(chrono::Utc::now()),
            "{status:?} graph's auto_continue_at time itself must still register as reached \
                 so the scheduler can clear the stale schedule"
        );
    }
}

// ── GraphStatus: as_str / from_str roundtrip ─────────────────────────

#[test]
fn graph_status_as_str_from_str_roundtrip() {
    let statuses = [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Paused,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ];
    for s in statuses {
        let s_str = s.as_str();
        assert_eq!(
            GraphStatus::from_str(s_str),
            s,
            "roundtrip failed for {s_str}"
        );
    }
}

#[test]
fn graph_status_from_str_unknown_defaults_to_draft() {
    let unknowns = ["", "DRAFT", "Running", "PENDING", "unknown", "123"];
    for input in unknowns {
        assert_eq!(
            GraphStatus::from_str(input),
            GraphStatus::Draft,
            "expected Draft for {input:?}"
        );
    }
}

#[test]
fn graph_status_as_str_returns_lowercase() {
    for s in [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Paused,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ] {
        assert_eq!(s.as_str(), s.as_str().to_lowercase());
    }
}

// ── GraphSpecStatus: as_str / from_str roundtrip ─────────────────────

#[test]
fn graph_spec_status_as_str_from_str_roundtrip() {
    let statuses = [
        GraphSpecStatus::Pending,
        GraphSpecStatus::Running,
        GraphSpecStatus::Completed,
        GraphSpecStatus::Failed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Interrupted,
    ];
    for s in statuses {
        let s_str = s.as_str();
        assert_eq!(
            GraphSpecStatus::from_str(s_str),
            s,
            "roundtrip failed for {s_str}"
        );
    }
}

#[test]
fn graph_spec_status_from_str_unknown_defaults_to_pending() {
    let unknowns = ["", "PENDING", "Running", "DONE", "unknown", "xyz"];
    for input in unknowns {
        assert_eq!(
            GraphSpecStatus::from_str(input),
            GraphSpecStatus::Pending,
            "expected Pending for {input:?}"
        );
    }
}

#[test]
fn graph_spec_status_as_str_returns_lowercase() {
    for s in [
        GraphSpecStatus::Pending,
        GraphSpecStatus::Running,
        GraphSpecStatus::Completed,
        GraphSpecStatus::Failed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Interrupted,
    ] {
        assert_eq!(s.as_str(), s.as_str().to_lowercase());
    }
}

#[test]
fn graph_spec_status_pending_is_distinct_from_running() {
    assert_ne!(
        GraphSpecStatus::Pending.as_str(),
        GraphSpecStatus::Running.as_str()
    );
}

// ── GraphNodeKind: as_str / from_str / display_str roundtrip ─────────

#[test]
fn graph_node_kind_as_str_from_str_roundtrip() {
    let kinds = [
        GraphNodeKind::Agent,
        GraphNodeKind::Check,
        GraphNodeKind::Gate,
        GraphNodeKind::Join,
    ];
    for k in kinds {
        let k_str = k.as_str();
        assert_eq!(
            GraphNodeKind::from_str(k_str),
            Some(k),
            "roundtrip failed for {k_str}"
        );
    }
}

#[test]
fn graph_node_kind_from_str_invalid_returns_none() {
    let invalids = ["", "AGENT", "Agent", "ensemble", "unknown", "workflow"];
    for input in invalids {
        assert_eq!(
            GraphNodeKind::from_str(input),
            None,
            "expected None for {input:?}"
        );
    }
}

#[test]
fn graph_node_kind_display_str_matches_as_str_except_join() {
    for k in [
        GraphNodeKind::Agent,
        GraphNodeKind::Check,
        GraphNodeKind::Gate,
    ] {
        assert_eq!(k.display_str(), k.as_str());
    }
    assert_eq!(GraphNodeKind::Join.display_str(), "quorum");
    assert_ne!(
        GraphNodeKind::Join.display_str(),
        GraphNodeKind::Join.as_str()
    );
}

#[test]
fn graph_node_kind_as_str_returns_lowercase() {
    for k in [
        GraphNodeKind::Agent,
        GraphNodeKind::Check,
        GraphNodeKind::Gate,
        GraphNodeKind::Join,
    ] {
        assert_eq!(k.as_str(), k.as_str().to_lowercase());
    }
}

// ── GraphEdgeCondition: as_str / from_str roundtrip ──────────────────

#[test]
fn graph_edge_condition_as_str_from_str_roundtrip() {
    let conds = [
        GraphEdgeCondition::Pass,
        GraphEdgeCondition::Fail,
        GraphEdgeCondition::Always,
    ];
    for c in conds {
        let c_str = c.as_str().to_string();
        assert_eq!(
            GraphEdgeCondition::from_str(&c_str),
            Some(c.clone()),
            "roundtrip failed for {c_str}"
        );
    }
}

#[test]
fn graph_edge_condition_from_str_invalid_returns_none() {
    let invalids = ["", "PASS", "Pass", "never", "sometimes", "123"];
    for input in invalids {
        assert_eq!(
            GraphEdgeCondition::from_str(input),
            None,
            "expected None for {input:?}"
        );
    }
}

#[test]
fn graph_edge_condition_as_str_returns_lowercase() {
    for c in [
        GraphEdgeCondition::Pass,
        GraphEdgeCondition::Fail,
        GraphEdgeCondition::Always,
    ] {
        assert_eq!(c.as_str(), c.as_str().to_lowercase());
    }
}

// ── GraphRunStatus: as_str / from_str roundtrip ──────────────────────

#[test]
fn graph_run_status_as_str_from_str_roundtrip() {
    let statuses = [
        GraphRunStatus::Running,
        GraphRunStatus::Pass,
        GraphRunStatus::Fail,
    ];
    for s in statuses {
        let s_str = s.as_str();
        assert_eq!(
            GraphRunStatus::from_str(s_str),
            s,
            "roundtrip failed for {s_str}"
        );
    }
}

#[test]
fn graph_run_status_from_str_unknown_defaults_to_running() {
    let unknowns = ["", "RUNNING", "Running", "done", "unknown", "42"];
    for input in unknowns {
        assert_eq!(
            GraphRunStatus::from_str(input),
            GraphRunStatus::Running,
            "expected Running for {input:?}"
        );
    }
}

#[test]
fn graph_run_status_as_str_returns_lowercase() {
    for s in [
        GraphRunStatus::Running,
        GraphRunStatus::Pass,
        GraphRunStatus::Fail,
    ] {
        assert_eq!(s.as_str(), s.as_str().to_lowercase());
    }
}

// ── SpecAdminStatusOutcome: variant construction & equality ─────────

#[test]
fn spec_admin_status_outcome_success_variants() {
    let a = SpecAdminStatusOutcome::Success;
    assert!(matches!(a, SpecAdminStatusOutcome::Success));
}

#[test]
fn spec_admin_status_outcome_not_found_variants() {
    let a = SpecAdminStatusOutcome::NotFound;
    assert!(matches!(a, SpecAdminStatusOutcome::NotFound));
}

#[test]
fn spec_admin_status_outcome_not_standalone_carries_id() {
    let outcome = SpecAdminStatusOutcome::NotStandalone("spec-42".to_string());
    match outcome {
        SpecAdminStatusOutcome::NotStandalone(id) => assert_eq!(id, "spec-42"),
        _ => panic!("expected NotStandalone"),
    }
}

#[test]
fn spec_admin_status_outcome_active_run_carries_ids() {
    let outcome = SpecAdminStatusOutcome::ActiveRun {
        graph_id: "graph-1".to_string(),
        run_id: "run-2".to_string(),
    };
    match outcome {
        SpecAdminStatusOutcome::ActiveRun { graph_id, run_id } => {
            assert_eq!(graph_id, "graph-1");
            assert_eq!(run_id, "run-2");
        }
        _ => panic!("expected ActiveRun"),
    }
}

#[test]
fn spec_admin_status_outcome_variants_are_distinct() {
    let success = SpecAdminStatusOutcome::Success;
    let not_found = SpecAdminStatusOutcome::NotFound;
    let not_standalone = SpecAdminStatusOutcome::NotStandalone("x".to_string());
    let active_run = SpecAdminStatusOutcome::ActiveRun {
        graph_id: "a".to_string(),
        run_id: "b".to_string(),
    };
    assert_ne!(format!("{success:?}"), format!("{not_found:?}"));
    assert_ne!(format!("{not_standalone:?}"), format!("{active_run:?}"));
}

// ── GraphResetOutcome: variant construction & equality ───────────────

#[test]
fn graph_reset_outcome_not_found() {
    let a = GraphResetOutcome::NotFound;
    assert!(matches!(a, GraphResetOutcome::NotFound));
}

#[test]
fn graph_reset_outcome_in_flight_carries_run_info() {
    let started_at = chrono::Utc::now();
    let a = GraphResetOutcome::InFlight {
        run_id: "run-1".to_string(),
        node_id: "node-1".to_string(),
        started_at,
    };
    match a {
        GraphResetOutcome::InFlight {
            run_id,
            node_id,
            started_at: at,
        } => {
            assert_eq!(run_id, "run-1");
            assert_eq!(node_id, "node-1");
            assert_eq!(at, started_at);
        }
        _ => panic!("expected InFlight"),
    }
}

#[test]
fn graph_reset_outcome_invalid_spec_carries_id() {
    let outcome = GraphResetOutcome::InvalidSpec("bad-spec".to_string());
    match outcome {
        GraphResetOutcome::InvalidSpec(id) => assert_eq!(id, "bad-spec"),
        _ => panic!("expected InvalidSpec"),
    }
}

#[test]
fn graph_reset_outcome_reset_carries_count() {
    let outcome = GraphResetOutcome::Reset {
        spec_count: 5,
        skipped_count: 0,
    };
    match outcome {
        GraphResetOutcome::Reset {
            spec_count,
            skipped_count: _,
        } => assert_eq!(spec_count, 5),
        _ => panic!("expected Reset"),
    }
}

#[test]
fn graph_reset_outcome_variants_are_distinct() {
    let not_found = GraphResetOutcome::NotFound;
    let in_flight = GraphResetOutcome::InFlight {
        run_id: "run-1".to_string(),
        node_id: "node-1".to_string(),
        started_at: chrono::Utc::now(),
    };
    let invalid = GraphResetOutcome::InvalidSpec("x".to_string());
    let reset = GraphResetOutcome::Reset {
        spec_count: 0,
        skipped_count: 0,
    };
    assert_ne!(format!("{not_found:?}"), format!("{in_flight:?}"));
    assert_ne!(format!("{invalid:?}"), format!("{reset:?}"));
}

// ── GraphStatus: equality & Debug ────────────────────────────────────

#[test]
fn graph_status_variants_are_distinct() {
    let all = [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Paused,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let a = all[i];
            let b = all[j];
            assert_ne!(a, b, "{a:?} should differ from {b:?}");
        }
    }
}

#[test]
fn graph_status_debug_format_matches_variant_name() {
    assert_eq!(format!("{:?}", GraphStatus::Draft), "Draft");
    assert_eq!(format!("{:?}", GraphStatus::Running), "Running");
    assert_eq!(format!("{:?}", GraphStatus::Paused), "Paused");
    assert_eq!(format!("{:?}", GraphStatus::Completed), "Completed");
    assert_eq!(format!("{:?}", GraphStatus::Failed), "Failed");
}

// ── GraphSpecStatus: equality & Debug ────────────────────────────────

#[test]
fn graph_spec_status_variants_are_distinct() {
    let all = [
        GraphSpecStatus::Pending,
        GraphSpecStatus::Running,
        GraphSpecStatus::Completed,
        GraphSpecStatus::Failed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Interrupted,
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let a = all[i];
            let b = all[j];
            assert_ne!(a, b, "{a:?} should differ from {b:?}");
        }
    }
}

// ── GraphNodeKind: equality & Debug ──────────────────────────────────

#[test]
fn graph_node_kind_variants_are_distinct() {
    let all = [
        GraphNodeKind::Agent,
        GraphNodeKind::Check,
        GraphNodeKind::Gate,
        GraphNodeKind::Join,
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let a = all[i];
            let b = all[j];
            assert_ne!(a, b, "{a:?} should differ from {b:?}");
        }
    }
}

// ── GraphEdgeCondition: equality & Debug ─────────────────────────────

#[test]
fn graph_edge_condition_variants_are_distinct() {
    let all = [
        GraphEdgeCondition::Pass,
        GraphEdgeCondition::Fail,
        GraphEdgeCondition::Always,
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let a = &all[i];
            let b = &all[j];
            assert_ne!(a, b, "{a:?} should differ from {b:?}");
        }
    }
}

// ── GraphRunStatus: equality & Debug ─────────────────────────────────

#[test]
fn graph_run_status_variants_are_distinct() {
    let all = [
        GraphRunStatus::Running,
        GraphRunStatus::Pass,
        GraphRunStatus::Fail,
    ];
    for i in 0..all.len() {
        for j in (i + 1)..all.len() {
            let a = all[i];
            let b = all[j];
            assert_ne!(a, b, "{a:?} should differ from {b:?}");
        }
    }
}

// ── Clone: all status enums are Clone ───────────────────────────────

#[test]
fn graph_status_is_clone() {
    let s = GraphStatus::Running;
    let cloned = s;
    assert_eq!(s, cloned);
}

#[test]
fn graph_spec_status_is_clone() {
    let s = GraphSpecStatus::Failed;
    let cloned = s;
    assert_eq!(s, cloned);
}

#[test]
fn graph_node_kind_is_clone() {
    let k = GraphNodeKind::Join;
    let cloned = k;
    assert_eq!(k, cloned);
}

#[test]
fn graph_edge_condition_is_clone() {
    let c = GraphEdgeCondition::Always;
    let cloned = c.clone();
    assert_eq!(c, cloned);
}

#[test]
fn graph_run_status_is_clone() {
    let s = GraphRunStatus::Pass;
    let cloned = s;
    assert_eq!(s, cloned);
}

// ── Graph: is_fireable exhaustive coverage ───────────────────────────

#[test]
fn graph_is_fireable_exhaustive() {
    let expected_fireable = [
        (GraphStatus::Draft, true),
        (GraphStatus::Running, false),
        (GraphStatus::Pausing, false),
        (GraphStatus::Paused, false),
        (GraphStatus::Completed, true),
        (GraphStatus::Failed, true),
    ];
    for (status, expected) in expected_fireable {
        let lp = graph_with_trigger(status, None);
        assert_eq!(
            lp.is_fireable(),
            expected,
            "{status:?}.is_fireable() should be {expected}"
        );
    }
}

// ── Graph: trigger_type_label exhaustive ─────────────────────────────

#[test]
fn graph_trigger_type_label_exhaustive() {
    let lp_manual = graph_with_trigger(GraphStatus::Draft, None);
    assert_eq!(lp_manual.trigger_type_label(), "manual");

    let lp_cron = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Cron {
            schedule_expr: "* * * * *".to_string(),
        }),
    );
    assert_eq!(lp_cron.trigger_type_label(), "cron");

    let lp_watch = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Watch {
            path: "/tmp".to_string(),
            events: vec![],
            debounce_seconds: 2,
            recursive: false,
        }),
    );
    assert_eq!(lp_watch.trigger_type_label(), "watch");
}

// ── Graph: schedule_expr / watch_path exhaustive ─────────────────────

#[test]
fn graph_schedule_expr_only_some_for_cron() {
    let cron = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Cron {
            schedule_expr: "0 9 * * 1-5".to_string(),
        }),
    );
    assert_eq!(cron.schedule_expr(), Some("0 9 * * 1-5"));

    let manual = graph_with_trigger(GraphStatus::Draft, None);
    assert_eq!(manual.schedule_expr(), None);

    let watch = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Watch {
            path: "/src".to_string(),
            events: vec![],
            debounce_seconds: 2,
            recursive: false,
        }),
    );
    assert_eq!(watch.schedule_expr(), None);
}

#[test]
fn graph_watch_path_only_some_for_watch() {
    let watch = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Watch {
            path: "/data".to_string(),
            events: vec![super::WatchEvent::Modify],
            debounce_seconds: 2,
            recursive: false,
        }),
    );
    assert_eq!(watch.watch_path(), Some("/data"));

    let cron = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Cron {
            schedule_expr: "* * * * *".to_string(),
        }),
    );
    assert_eq!(cron.watch_path(), None);

    let manual = graph_with_trigger(GraphStatus::Draft, None);
    assert_eq!(manual.watch_path(), None);
}

// ── Graph: is_autorun_due edge cases ─────────────────────────────────

#[test]
fn graph_autorun_due_exactly_at_threshold() {
    let now = chrono::Utc::now();
    let mut lp = graph_with_trigger(GraphStatus::Completed, None);
    lp.autorun_at = Some(now);
    assert!(lp.is_autorun_due(now));
}

#[test]
fn graph_autorun_not_due_one_second_before() {
    let now = chrono::Utc::now();
    let mut lp = graph_with_trigger(GraphStatus::Completed, None);
    lp.autorun_at = Some(now + chrono::Duration::seconds(1));
    assert!(!lp.is_autorun_due(now));
}

// ── Graph: is_auto_continue_due edge cases ───────────────────────────

#[test]
fn graph_auto_continue_due_exactly_at_threshold() {
    let now = chrono::Utc::now();
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.auto_continue_at = Some(now);
    assert!(lp.is_auto_continue_due(now));
}

#[test]
fn graph_auto_continue_not_due_one_second_before() {
    let now = chrono::Utc::now();
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.auto_continue_at = Some(now + chrono::Duration::seconds(1));
    assert!(!lp.is_auto_continue_due(now));
}

#[test]
fn graph_auto_continue_time_reached_independent_of_status() {
    let now = chrono::Utc::now();
    for status in [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Paused,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ] {
        let mut lp = graph_with_trigger(status, None);
        lp.auto_continue_at = Some(now - chrono::Duration::seconds(1));
        assert!(
            lp.is_auto_continue_time_reached(now),
            "{status:?}: time_reached should be true"
        );
    }
}

#[test]
fn graph_auto_continue_time_not_reached_in_future() {
    let now = chrono::Utc::now();
    let mut lp = graph_with_trigger(GraphStatus::Paused, None);
    lp.auto_continue_at = Some(now + chrono::Duration::hours(1));
    assert!(!lp.is_auto_continue_time_reached(now));
}

#[test]
fn graph_auto_continue_time_not_reached_when_none() {
    let lp = graph_with_trigger(GraphStatus::Paused, None);
    assert!(!lp.is_auto_continue_time_reached(chrono::Utc::now()));
}

// ── Graph: is_cron / is_watch exhaustive ─────────────────────────────

#[test]
fn graph_is_cron_and_is_watch_exhaustive() {
    let manual = graph_with_trigger(GraphStatus::Draft, None);
    assert!(!manual.is_cron());
    assert!(!manual.is_watch());

    let cron = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Cron {
            schedule_expr: "* * * * *".to_string(),
        }),
    );
    assert!(cron.is_cron());
    assert!(!cron.is_watch());

    let watch = graph_with_trigger(
        GraphStatus::Draft,
        Some(super::Trigger::Watch {
            path: "/x".to_string(),
            events: vec![],
            debounce_seconds: 2,
            recursive: false,
        }),
    );
    assert!(!watch.is_cron());
    assert!(watch.is_watch());
}

// ── serde: GraphStatus roundtrip ─────────────────────────────────────

#[test]
fn graph_status_serde_roundtrip() {
    let statuses = [
        GraphStatus::Draft,
        GraphStatus::Running,
        GraphStatus::Paused,
        GraphStatus::Completed,
        GraphStatus::Failed,
    ];
    for s in statuses {
        let json = serde_json::to_string(&s).unwrap();
        let deserialized: GraphStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, s, "serde roundtrip failed for {json}");
    }
}

#[test]
fn graph_status_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_string(&GraphStatus::Draft).unwrap(),
        "\"draft\""
    );
    assert_eq!(
        serde_json::to_string(&GraphStatus::Running).unwrap(),
        "\"running\""
    );
    assert_eq!(
        serde_json::to_string(&GraphStatus::Completed).unwrap(),
        "\"completed\""
    );
}

// ── serde: GraphSpecStatus roundtrip ─────────────────────────────────

#[test]
fn graph_spec_status_serde_roundtrip() {
    let statuses = [
        GraphSpecStatus::Pending,
        GraphSpecStatus::Running,
        GraphSpecStatus::Completed,
        GraphSpecStatus::Failed,
        GraphSpecStatus::Skipped,
        GraphSpecStatus::Interrupted,
    ];
    for s in statuses {
        let json = serde_json::to_string(&s).unwrap();
        let deserialized: GraphSpecStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, s, "serde roundtrip failed for {json}");
    }
}

#[test]
fn graph_spec_status_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_string(&GraphSpecStatus::Pending).unwrap(),
        "\"pending\""
    );
    assert_eq!(
        serde_json::to_string(&GraphSpecStatus::Skipped).unwrap(),
        "\"skipped\""
    );
    assert_eq!(
        serde_json::to_string(&GraphSpecStatus::Interrupted).unwrap(),
        "\"interrupted\""
    );
}

// ── serde: GraphNodeKind roundtrip ───────────────────────────────────

#[test]
fn graph_node_kind_serde_roundtrip() {
    let kinds = [
        GraphNodeKind::Agent,
        GraphNodeKind::Check,
        GraphNodeKind::Gate,
        GraphNodeKind::Join,
    ];
    for k in kinds {
        let json = serde_json::to_string(&k).unwrap();
        let deserialized: GraphNodeKind = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, k, "serde roundtrip failed for {json}");
    }
}

#[test]
fn graph_node_kind_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_string(&GraphNodeKind::Agent).unwrap(),
        "\"agent\""
    );
    assert_eq!(
        serde_json::to_string(&GraphNodeKind::Gate).unwrap(),
        "\"gate\""
    );
}

// ── serde: GraphEdgeCondition roundtrip ──────────────────────────────

#[test]
fn graph_edge_condition_serde_roundtrip() {
    let conds = [
        GraphEdgeCondition::Pass,
        GraphEdgeCondition::Fail,
        GraphEdgeCondition::Always,
    ];
    for c in conds {
        let json = serde_json::to_string(&c).unwrap();
        let deserialized: GraphEdgeCondition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, c, "serde roundtrip failed for {json}");
    }
}

#[test]
fn graph_edge_condition_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_string(&GraphEdgeCondition::Pass).unwrap(),
        "\"pass\""
    );
    assert_eq!(
        serde_json::to_string(&GraphEdgeCondition::Always).unwrap(),
        "\"always\""
    );
}

// ── serde: GraphRunStatus roundtrip ──────────────────────────────────

#[test]
fn graph_run_status_serde_roundtrip() {
    let statuses = [
        GraphRunStatus::Running,
        GraphRunStatus::Pass,
        GraphRunStatus::Fail,
    ];
    for s in statuses {
        let json = serde_json::to_string(&s).unwrap();
        let deserialized: GraphRunStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, s, "serde roundtrip failed for {json}");
    }
}

#[test]
fn graph_run_status_serde_uses_snake_case() {
    assert_eq!(
        serde_json::to_string(&GraphRunStatus::Running).unwrap(),
        "\"running\""
    );
    assert_eq!(
        serde_json::to_string(&GraphRunStatus::Pass).unwrap(),
        "\"pass\""
    );
}

// ── serde: deserialization from string variants ─────────────────────

#[test]
fn graph_status_deserialize_from_json_string() {
    let input = "\"completed\"";
    let s: GraphStatus = serde_json::from_str(input).unwrap();
    assert_eq!(s, GraphStatus::Completed);
}

#[test]
fn graph_spec_status_deserialize_from_json_string() {
    let input = "\"skipped\"";
    let s: GraphSpecStatus = serde_json::from_str(input).unwrap();
    assert_eq!(s, GraphSpecStatus::Skipped);
}

#[test]
fn graph_node_kind_deserialize_from_json_string() {
    let input = "\"join\"";
    let k: GraphNodeKind = serde_json::from_str(input).unwrap();
    assert_eq!(k, GraphNodeKind::Join);
}

#[test]
fn graph_edge_condition_deserialize_from_json_string() {
    let input = "\"always\"";
    let c: GraphEdgeCondition = serde_json::from_str(input).unwrap();
    assert_eq!(c, GraphEdgeCondition::Always);
}

#[test]
fn graph_run_status_deserialize_from_json_string() {
    let input = "\"fail\"";
    let s: GraphRunStatus = serde_json::from_str(input).unwrap();
    assert_eq!(s, GraphRunStatus::Fail);
}

// ── SpecAdminStatusOutcome: clone & Debug ───────────────────────────

#[test]
fn spec_admin_status_outcome_success_is_clone() {
    let a = SpecAdminStatusOutcome::Success;
    let b = a;
    assert!(matches!(b, SpecAdminStatusOutcome::Success));
    fn _assert_clone<T: Clone>() {}
    _assert_clone::<SpecAdminStatusOutcome>();
}

#[test]
fn spec_admin_status_outcome_not_found_is_clone() {
    let a = SpecAdminStatusOutcome::NotFound;
    let b = a;
    assert!(matches!(b, SpecAdminStatusOutcome::NotFound));
    fn _assert_clone<T: Clone>() {}
    _assert_clone::<SpecAdminStatusOutcome>();
}

#[test]
fn spec_admin_status_outcome_not_standalone_is_clone() {
    let a = SpecAdminStatusOutcome::NotStandalone("s1".to_string());
    let b = a.clone();
    match (&a, &b) {
        (SpecAdminStatusOutcome::NotStandalone(o), SpecAdminStatusOutcome::NotStandalone(c)) => {
            assert_eq!(o, c);
            assert_eq!(c, "s1");
        }
        _ => panic!("expected clone of NotStandalone"),
    }
}

#[test]
fn spec_admin_status_outcome_active_run_is_clone() {
    let a = SpecAdminStatusOutcome::ActiveRun {
        graph_id: "l1".to_string(),
        run_id: "r1".to_string(),
    };
    let b = a.clone();
    match (&a, &b) {
        (
            SpecAdminStatusOutcome::ActiveRun {
                graph_id: al,
                run_id: ar,
            },
            SpecAdminStatusOutcome::ActiveRun {
                graph_id: bl,
                run_id: br,
            },
        ) => {
            assert_eq!(al, bl);
            assert_eq!(ar, br);
            assert_eq!(bl, "l1");
            assert_eq!(br, "r1");
        }
        _ => panic!("expected clone of ActiveRun"),
    }
}

// ── GraphResetOutcome: clone & Debug ─────────────────────────────────

#[test]
fn graph_reset_outcome_is_clone() {
    let outcomes = [
        GraphResetOutcome::NotFound,
        GraphResetOutcome::InFlight {
            run_id: "run-1".to_string(),
            node_id: "node-1".to_string(),
            started_at: chrono::Utc::now(),
        },
        GraphResetOutcome::InvalidSpec("x".to_string()),
        GraphResetOutcome::Reset {
            spec_count: 3,
            skipped_count: 0,
        },
    ];
    for o in outcomes {
        let cloned = o.clone();
        assert_eq!(format!("{o:?}"), format!("{cloned:?}"));
    }
}

// ── GraphNodeKind::Router: as_str / from_str / display_str ───────────

#[test]
fn graph_node_kind_router_as_str() {
    assert_eq!(GraphNodeKind::Router.as_str(), "router");
}

#[test]
fn graph_node_kind_router_from_str_roundtrip() {
    assert_eq!(
        GraphNodeKind::from_str("router"),
        Some(GraphNodeKind::Router)
    );
    assert_eq!(
        GraphNodeKind::from_str(GraphNodeKind::Router.as_str()),
        Some(GraphNodeKind::Router)
    );
}

#[test]
fn graph_node_kind_router_display_str_matches_as_str() {
    assert_eq!(GraphNodeKind::Router.display_str(), "router");
}

#[test]
fn graph_node_kind_router_serde_roundtrip() {
    let json = serde_json::to_string(&GraphNodeKind::Router).unwrap();
    assert_eq!(json, "\"router\"");
    let deserialized: GraphNodeKind = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, GraphNodeKind::Router);
}

// ── GraphEdgeCondition::Route: as_str / route_label / from_parts ─────

#[test]
fn graph_edge_condition_route_as_str_is_fixed_tag() {
    let c = GraphEdgeCondition::Route("escalate".to_string());
    assert_eq!(c.as_str(), "route");
}

#[test]
fn graph_edge_condition_route_label_returns_the_label() {
    let c = GraphEdgeCondition::Route("escalate".to_string());
    assert_eq!(c.route_label(), Some("escalate"));
}

#[test]
fn graph_edge_condition_non_route_has_no_route_label() {
    assert_eq!(GraphEdgeCondition::Pass.route_label(), None);
    assert_eq!(GraphEdgeCondition::Fail.route_label(), None);
    assert_eq!(GraphEdgeCondition::Always.route_label(), None);
}
