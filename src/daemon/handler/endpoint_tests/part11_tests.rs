//! Part 11 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[tokio::test]
async fn graph_update_ensemble_switching_to_cascade_clears_stale_straggler_and_grace_fields() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Ensemble".to_string(),
            kind: Some("parallel".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            blueprint: None,
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: Some(7),
            quorum_grace_minutes: Some(2),
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    let ensemble_id = extract_id(&created, "ensemble_id");

    let updated = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: Some("cascade".to_string()),
            prompt_template: None,
            members: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&updated),
        "switching to cascade must succeed: {:?}",
        updated
    );
    assert!(
        text(&updated).contains("Cleared") && text(&updated).contains("straggler_timeout_minutes"),
        "response must say the stale fields were cleared: {}",
        text(&updated)
    );

    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.kind, EnsembleKind::Cascade);
    assert_eq!(details.ensemble.straggler_timeout_minutes, None);
    assert_eq!(details.ensemble.quorum_grace_minutes, None);
}

#[tokio::test]
async fn graph_update_ensemble_can_change_kind() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Ensemble".to_string(),
            kind: None,
            prompt_template: Some("do it".to_string()),
            members: Some(vec![
                EnsembleMemberParams {
                    platform: "claude".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
                EnsembleMemberParams {
                    platform: "openrouter".to_string(),
                    model: None,
                    prompt_override: None,
                    timeout_minutes: None,
                    infra_retry_limit: None,
                    infra_crash_max_seconds: None,
                    infra_backoff_seconds: None,
                },
            ]),
            blueprint: None,
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    let ensemble_id = extract_id(&created, "ensemble_id");

    let updated = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            mutex: None,
            commit_rights: None,
            ensemble_id: ensemble_id.clone(),
            kind: Some("cascade".to_string()),
            prompt_template: None,
            members: None,
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: None,
            on_fail_to: None,
            from_node: None,
            condition: None,
            add_entry_from: None,
            add_entry_condition: None,
            remove_entry_from: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&updated),
        "updating kind to cascade should succeed: {:?}",
        updated
    );

    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.kind, EnsembleKind::Cascade);
}

/// CM36 FR3: a 12-member round_robin cannot be switched to parallel
/// (which caps at 8) — refused with a message naming both counts, and the
/// ensemble's kind is left untouched.
#[tokio::test]
async fn graph_update_ensemble_refuses_round_robin_to_parallel_above_eight() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let members: Vec<EnsembleMemberParams> = (0..12)
        .map(|_| EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Wide".to_string(),
            kind: Some("round_robin".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(members),
            blueprint: None,
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");

    let updated = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            kind: Some("parallel".to_string()),
            ..blank_ensemble_update(&ensemble_id)
        }),
    )
    .await
    .unwrap();
    assert!(
        is_err(&updated),
        "switching 12 members to parallel must be refused: {:?}",
        updated
    );
    let msg = text(&updated);
    assert!(msg.contains("parallel"), "{msg}");
    assert!(msg.contains("2-8") && msg.contains("12"), "{msg}");

    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(
        details.ensemble.kind,
        EnsembleKind::RoundRobin,
        "a refused kind change must write nothing"
    );
}

/// CM36: blueprint-supplied members were cloned through without a
/// per-kind bounds check — a 12-member blueprint used with kind parallel
/// built an over-cap ensemble no validator would ever have accepted.
/// A 12-member blueprint with a sequential kind still builds fine.
#[tokio::test]
async fn graph_add_ensemble_blueprint_members_enforce_requested_kind_bounds() {
    use crate::domain::blueprints::EnsembleBlueprint;
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let wide: crate::domain::graphs::EnsembleMemberSpec =
        ("claude".to_string(), None, None, None, None, None, None);
    db.insert_ensemble_blueprint(&EnsembleBlueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "wide-blueprint".to_string(),
        prompt_template: "do it".to_string(),
        members: vec![wide.clone(); 12],
        min_pass: None,
        quorum_grace_minutes: None,
        builtin: false,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let refused = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "TooWide".to_string(),
            kind: Some("parallel".to_string()),
            prompt_template: None,
            members: None,
            blueprint: Some("wide-blueprint".to_string()),
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        is_err(&refused),
        "a 12-member blueprint with kind parallel must be refused: {:?}",
        refused
    );
    let msg = text(&refused);
    assert!(msg.contains("parallel"), "{msg}");
    assert!(msg.contains("2-8") && msg.contains("12"), "{msg}");

    let accepted = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "WideSeq".to_string(),
            kind: Some("round_robin".to_string()),
            prompt_template: None,
            members: None,
            blueprint: Some("wide-blueprint".to_string()),
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&accepted), "{}", text(&accepted));
}

/// CM36 FR3 follow-up: a combined kind-plus-members update uses the
/// RESULTING count — shrinking a 12-member round_robin to 8 members
/// while switching to parallel is accepted, not refused on the stale
/// stored count.
#[tokio::test]
async fn graph_update_ensemble_kind_plus_shrink_uses_resulting_count() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let spec = insert_test_spec(&db, &lp.id, 1);
    let entry = add_agent_node(&handler, &spec.id, "Entry").await;
    let arbiter = add_agent_node(&handler, &spec.id, "Arbiter").await;

    let members: Vec<EnsembleMemberParams> = (0..12)
        .map(|_| EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    let created = call_graph_action(
        &handler,
        "graph_ensemble",
        "add",
        params_id(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: Some(spec.id.clone()),
            graph_id: None,
            name: "Wide".to_string(),
            kind: Some("round_robin".to_string()),
            prompt_template: Some("do it".to_string()),
            members: Some(members),
            blueprint: None,
            node: None,
            from_node: entry.clone(),
            condition: "always".to_string(),
            min_pass: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            on_pass_to: arbiter.clone(),
            on_fail_to: None,

            crew: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let ensemble_id = extract_id(&created, "ensemble_id");

    let shrunk: Vec<EnsembleMemberParams> = (0..8)
        .map(|_| EnsembleMemberParams {
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        })
        .collect();
    let updated = call_graph_action(
        &handler,
        "graph_ensemble",
        "update",
        params_id(GraphUpdateEnsembleParams {
            kind: Some("parallel".to_string()),
            members: Some(shrunk),
            // The 12-member creation defaulted min_pass to 12; a shrink
            // must lower it alongside, same as any members-only resize.
            min_pass: Some(8),
            ..blank_ensemble_update(&ensemble_id)
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&updated),
        "a shrink-to-8 plus switch-to-parallel must be accepted: {}",
        text(&updated)
    );
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert_eq!(details.ensemble.kind, EnsembleKind::Parallel);
    assert_eq!(details.members.len(), 8);
}

#[tokio::test]
async fn graph_run_idea_and_queue_mutually_exclusive() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let result = call_graph_action(
        &handler,
        "graph_control",
        "run",
        params_id(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: Some("some-queue".to_string()),
            workdir: None,
            idea: Some("build a landing page".to_string()),
            sandbox: None,
            start_node_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        is_err(&result),
        "idea and queue_id together must be rejected"
    );
    assert!(
        text(&result).contains("mutually exclusive"),
        "error must mention mutual exclusion: {}",
        text(&result)
    );
}

#[tokio::test]
async fn idea_placeholder_spec_is_absent_from_spec_listings() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    // The row `run_graph_dispatch` binds to a spec-less / idea-driven graph
    // to carry `{{spec_content}}`: blank name, idea text in the
    // description. It is engine bookkeeping and must not surface as work.
    db.insert_graph_spec(&GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: Some(lp.id.clone()),
        name: String::new(),
        description: Some("Build a hyperframes explainer about our launch".to_string()),
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
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();

    let listed = raw_text(
        &call_resource_action(
            &handler,
            "spec",
            "list",
            params_id(SpecListParams {
                workdir: None,
                status: None,
                unassigned_only: None,
                include_descriptions: Some(true),
                limit: None,
                offset: None,
                spec_type: None,
            }),
        )
        .await
        .unwrap(),
    );
    assert!(
        !listed.contains("hyperframes explainer"),
        "the idea placeholder must not appear in spec_list: {listed}"
    );
    assert!(
        listed.contains("\"total\": 0"),
        "the blank placeholder must not be counted in spec_list total: {listed}"
    );

    let got = call_graph_action(
        &handler,
        "graph",
        "get",
        params_id(GraphGetParams {
            graph_id: lp.id.clone(),
        }),
    )
    .await
    .unwrap();
    let got = raw_text(&got);
    assert!(
        !got.contains("hyperframes explainer"),
        "the idea placeholder must not appear in graph specs: {got}"
    );
}

/// (CB22) `graph_control` with a graph-bound spec whose name is `""` and
/// description is `None` is refused synchronously: the error names the
/// spec and the graph's status is untouched. The TUI's `run` flow sends
/// only `graph_id`, so this is the user-visible refusal for a graph with
/// no usable content and no queue.
#[tokio::test]
async fn graph_run_refuses_blank_bound_spec_and_leaves_graph_untouched() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let blank_id = uuid::Uuid::new_v4().to_string();
    db.insert_graph_spec(&GraphSpec {
        id: blank_id.clone(),
        graph_id: Some(lp.id.clone()),
        name: String::new(),
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
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "run",
        params_id(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: None,
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        is_err(&result),
        "a blank bound spec must refuse the launch: {}",
        text(&result)
    );
    let message = text(&result);
    assert!(
        message.contains(&blank_id),
        "the refusal must name the offending spec: {message}"
    );
    assert!(
        message.contains("both name and description are empty"),
        "the refusal must say what is missing: {message}"
    );

    let stored = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(
        stored.status,
        GraphStatus::Draft,
        "a refused launch must leave the graph's status untouched"
    );
}

/// (CB22) The TUI's graph-only dispatch is rejected synchronously when the
/// graph has neither bound specs nor a selected queue; no placeholder row
/// may be created while the background run is being started.
#[tokio::test]
async fn graph_run_refuses_when_no_specs_or_queue_are_selected() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());

    let result = call_graph_action(
        &handler,
        "graph_control",
        "run",
        params_id(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: None,
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        }),
    )
    .await
    .unwrap();

    assert!(
        is_err(&result),
        "a graph-only launch without specs must be refused: {}",
        text(&result)
    );
    assert!(
        text(&result).contains("no specs to run"),
        "the refusal must explain what is missing: {}",
        text(&result)
    );
    let stored = db.get_graph(&lp.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphStatus::Draft);
    assert!(
        db.list_graph_specs(&lp.id).unwrap().is_empty(),
        "the refused TUI launch must not create a blank spec"
    );
}

/// (CB22) Queue isolation at the handler boundary: a `graph_control` with a
/// valid `queue_id` is accepted even when the graph has an unrelated
/// blank bound spec — queue launches validate only the queue's own
/// members.
#[tokio::test]
async fn graph_run_with_queue_ignores_unrelated_blank_bound_spec() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    db.insert_graph_spec(&GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: Some(lp.id.clone()),
        name: String::new(),
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
    })
    .unwrap();

    let member = GraphSpec {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: None,
        name: "Queue Member".to_string(),
        description: Some(valid_spec_description()),
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
    };
    db.insert_graph_spec(&member).unwrap();
    let queue_id = uuid::Uuid::new_v4().to_string();
    db.insert_queue(&Queue {
        id: queue_id.clone(),
        name: queue_id.clone(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.append_queue_member(&queue_id, &member.id, None).unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "run",
        params_id(GraphRunParams {
            graph_id: lp.id.clone(),
            queue_id: Some(queue_id),
            workdir: None,
            idea: None,
            sandbox: None,
            start_node_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&result),
        "a valid queue launch must ignore the unrelated blank bound spec: {}",
        text(&result)
    );
}

/// (CB22) `graph_control` reports a blank bound spec as a structured
/// `spec_warnings` entry — advisory, not a tool error — including when
/// the graph has no agent targets to probe.
#[tokio::test]
async fn graph_preflight_warns_for_blank_bound_spec_without_probe_targets() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let blank_id = uuid::Uuid::new_v4().to_string();
    db.insert_graph_spec(&GraphSpec {
        id: blank_id.clone(),
        graph_id: Some(lp.id.clone()),
        name: String::new(),
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
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&result),
        "a preflight warning must not be a tool error: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let warnings = body["spec_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("preflight must include spec_warnings: {body}"));
    assert_eq!(warnings.len(), 1, "expected one warning: {body}");
    assert_eq!(warnings[0]["spec_id"], blank_id);
    assert!(
        warnings[0]["warning"]
            .as_str()
            .unwrap_or_default()
            .contains("both name and description are empty"),
        "the warning must say what is missing: {body}"
    );
}

/// (CB22) `graph_control` includes `spec_warnings` alongside real probe
/// results. The agent node references a deliberately unconfigured
/// platform, so the probe reports `NotConfigured` immediately without
/// spawning anything or spending quota.
#[tokio::test]
async fn graph_preflight_includes_spec_warnings_alongside_probes() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let blank_id = uuid::Uuid::new_v4().to_string();
    db.insert_graph_spec(&GraphSpec {
        id: blank_id.clone(),
        graph_id: Some(lp.id.clone()),
        name: String::new(),
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
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "cb22-unconfigured-platform"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&result),
        "a preflight warning must not be a tool error: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert!(
        body["probes"]
            .as_array()
            .is_some_and(|probes| !probes.is_empty()),
        "expected probe results alongside the warning: {body}"
    );
    let warnings = body["spec_warnings"]
        .as_array()
        .unwrap_or_else(|| panic!("preflight must include spec_warnings: {body}"));
    assert_eq!(warnings.len(), 1, "expected one warning: {body}");
    assert_eq!(warnings[0]["spec_id"], blank_id);
}

/// (CM32) `agent_probe` accepts the slug and its result names the
/// product: `platform` stays `mistral` while a sibling
/// `platform_display` carries `Mistral AI · Vibe`. This pins the
/// handler-level enrichment — the helper's unit tests would survive
/// deleting the `platform_display` insertions, this would not.
#[tokio::test]
async fn agent_probe_result_names_the_product_beside_the_slug() {
    let home = tempdir().unwrap();
    write_cm32_config(home.path());
    let _home_guard = HomeVar::set(home.path());
    let (_dir, _db, handler) = endpoint_test_handler();

    let result = call_resource_action(
        &handler,
        "harness",
        "probe",
        params_id(AgentProbeParams {
            platform: Some("mistral".to_string()),
            model: None,
            timeout_seconds: Some(5),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let probes = body["probes"].as_array().unwrap();
    assert_eq!(probes.len(), 1, "{body}");
    assert_eq!(
        probes[0]["platform"],
        serde_json::json!("mistral"),
        "the identity key stays the slug"
    );
    assert_eq!(
        probes[0]["platform_display"],
        serde_json::json!("Mistral AI \u{b7} Vibe"),
        "the sibling names the product the registry publishes"
    );
    assert_ne!(
        probes[0]["outcome"],
        serde_json::json!("not_configured"),
        "the slug must be accepted as a configured platform"
    );
}

/// (CM32) `graph_control` probe entries carry the same
/// `platform_display` sibling, keyed beside the slug. A node stores
/// `platform: "mistral"` — the slug — and preflight's result names
/// Vibe. Binary unresolvable → fails fast without spawning.
#[tokio::test]
async fn graph_preflight_probe_entries_name_the_product() {
    let home = tempdir().unwrap();
    write_cm32_config(home.path());
    let _home_guard = HomeVar::set(home.path());
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    db.insert_graph_node(&GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "mistral"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    let probes = body["probes"]
        .as_array()
        .unwrap_or_else(|| panic!("expected probe results: {body}"));
    let mistral = probes
        .iter()
        .find(|p| p["platform"] == serde_json::json!("mistral"))
        .unwrap_or_else(|| panic!("expected a mistral probe entry: {body}"));
    assert_eq!(
        mistral["platform_display"],
        serde_json::json!("Mistral AI \u{b7} Vibe")
    );
}

/// CM35 FR2: `agent_probe` reports an interactive-only platform as
/// `interactive_only` — not reachable, counted as a confirmed
/// `would_fail`, never `unknown` — and spawns nothing (the fixture
/// binary would `touch` a marker if it ran). Handler-level half of the
/// guarantee; `probe_target_reports_interactive_only_without_spawning`
/// pins the same gate one layer down.
#[tokio::test]
async fn agent_probe_reports_interactive_only_without_spawning() {
    let home = tempdir().unwrap();
    let marker = write_freebuff_config(home.path());
    let _home_guard = HomeVar::set(home.path());

    let (_dir, _db, handler) = endpoint_test_handler();

    let result = call_resource_action(
        &handler,
        "harness",
        "probe",
        params_id(AgentProbeParams {
            platform: Some("freebuff".to_string()),
            model: None,
            timeout_seconds: Some(5),
        }),
    )
    .await
    .unwrap();

    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert_eq!(body["would_fail"], serde_json::json!(1), "{body}");
    assert_eq!(body["unknown"], serde_json::json!(0), "{body}");
    let probe = &body["probes"][0];
    assert_eq!(probe["platform"], serde_json::json!("freebuff"), "{body}");
    assert_eq!(
        probe["outcome"],
        serde_json::json!("interactive_only"),
        "{body}"
    );
    assert_eq!(probe["reachable"], serde_json::json!(false), "{body}");
    assert!(
        probe["error"].as_str().is_some_and(
            |e| e.contains("platform 'freebuff' is interactive-only (no headless mode)")
        ),
        "{body}"
    );
    assert!(probe["argv"].is_null(), "{body}");
    assert!(!marker.exists(), "probe must not spawn a process");
}

/// (CB25) Without a `reviewer`, preflight returns `"review": null` and
/// leaves the existing probe output untouched — the reviewer must never
/// run implicitly. Covers both the main path and the no-target early
/// return.
#[tokio::test]
async fn graph_preflight_without_reviewer_has_null_review() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    db.insert_graph_node(&GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "cb25-unconfigured-platform"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&result),
        "preflight without reviewer must succeed: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert!(
        body.get("review").is_some_and(|v| v.is_null()),
        "review must be null when no reviewer is requested: {body}"
    );
    assert!(
        body["probes"].as_array().is_some_and(|p| !p.is_empty()),
        "existing probe results must be unchanged: {body}"
    );
    assert!(
        body["spec_warnings"].as_array().is_some(),
        "existing spec_warnings must be unchanged: {body}"
    );

    // No-target early return carries the same null review.
    let bare = insert_test_graph(&db, dir.path());
    let early = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: bare.id.clone(),
            timeout_seconds: None,
            reviewer: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&early), "{}", text(&early));
    let early_body: serde_json::Value = serde_json::from_str(&raw_text(&early)).unwrap();
    assert!(
        early_body.get("review").is_some_and(|v| v.is_null()),
        "early-return review must be null when no reviewer is requested: {early_body}"
    );
}

/// (CB25) With `reviewer`, preflight reports a `review` object without
/// blocking the probe results. The spawn is real (`spawn_subagent`
/// returns an id immediately), but its outcome is environment-dependent:
/// `running` with a collectible row when the platform spawns, or
/// `not_spawned` with a note when it cannot start (notably, under
/// `debug_assertions` the platform registry's `reqwest::blocking`
/// client cannot run in async context, so debug test runs always take
/// the `not_spawned` branch while release builds spawn for real).
/// Either branch keeps the probes intact and never errors.
#[tokio::test]
async fn graph_preflight_with_reviewer_spawns_background_run() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    db.insert_graph_node(&GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "cb25-unconfigured-platform"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: Some(GraphPreflightReviewer {
                platform: "opencode".to_string(),
                model: None,
            }),
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&result),
        "preflight with reviewer must succeed: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert!(
        body["probes"].as_array().is_some_and(|p| !p.is_empty()),
        "probe results must still be returned: {body}"
    );
    let review = &body["review"];
    assert_eq!(review["pair"], "opencode/default", "{body}");
    match review["status"].as_str().unwrap_or_default() {
        "running" => {
            assert_eq!(review["collect_with"], "subagent_collect", "{body}");
            let id = review["id"]
                .as_str()
                .unwrap_or_else(|| panic!("running review must carry an id: {body}"));
            let record = db
                .get_subagent_run(id)
                .unwrap()
                .unwrap_or_else(|| panic!("spawned reviewer row '{id}' must exist"));
            assert!(
                !record.prompt.is_empty(),
                "spawned reviewer row must carry the audit prompt"
            );
        }
        "not_spawned" => {
            assert!(
                review["note"].as_str().is_some_and(|n| !n.is_empty()),
                "a reviewer that cannot start must say why: {body}"
            );
        }
        other => panic!("review status must be running or not_spawned, got: {body} ({other})"),
    }
}

/// (CB25) A reviewer that cannot start degrades to a `not_spawned` note;
/// it never fails the preflight and the probes are still returned.
#[tokio::test]
async fn graph_preflight_reviewer_failure_never_fails_preflight() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    db.insert_graph_node(&GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: None,
        graph_id: Some(lp.id.clone()),
        name: "impl".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "cb25-unconfigured-platform"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let result = call_graph_action(
        &handler,
        "graph_control",
        "preflight",
        params_id(GraphPreflightParams {
            graph_id: lp.id.clone(),
            timeout_seconds: Some(5),
            reviewer: Some(GraphPreflightReviewer {
                platform: "no-such-platform-xyz".to_string(),
                model: None,
            }),
        }),
    )
    .await
    .unwrap();
    assert!(
        !is_err(&result),
        "a reviewer that cannot start must not fail preflight: {}",
        text(&result)
    );
    let body: serde_json::Value = serde_json::from_str(&raw_text(&result)).unwrap();
    assert!(
        body["probes"].as_array().is_some_and(|p| !p.is_empty()),
        "probe results must still be returned: {body}"
    );
    assert_eq!(body["review"]["status"], "not_spawned", "{body}");
    assert!(
        body["review"]["note"]
            .as_str()
            .unwrap_or_default()
            .contains("unaffected"),
        "the note must say the probes are unaffected: {body}"
    );
}

/// (CB25) The reviewer prompt carries the graph id and, when the run
/// targets one, the graph's active run queue id (read from the persisted
/// graph row — no new preflight param), plus the audit-only instruction.
/// Unit-tested directly: spawning a real reviewer is not possible under
/// `debug_assertions` (see the `spawns_background_run` test), but the
/// prompt construction is what carries the ids.
#[test]
fn graph_preflight_reviewer_prompt_carries_graph_and_queue_ids() {
    let prompt = build_preflight_reviewer_prompt("graph-1", Some("q-1"), "");
    assert!(
        prompt.contains("graph_id='graph-1'"),
        "reviewer prompt must carry the graph id: {prompt}"
    );
    assert!(
        prompt.contains("queue_id='q-1'"),
        "reviewer prompt must carry the queue id: {prompt}"
    );
    assert!(
        prompt.contains("Audit ONLY"),
        "reviewer prompt must carry the audit-only instruction: {prompt}"
    );

    let no_queue = build_preflight_reviewer_prompt("graph-1", None, "");
    assert!(
        no_queue.contains("graph_id='graph-1'"),
        "reviewer prompt must carry the graph id without a queue: {no_queue}"
    );
    assert!(
        !no_queue.contains("queue_id="),
        "reviewer prompt must not name a queue when there is none: {no_queue}"
    );
}

/// (CB25) When the `graph-reviewer` skill resolves in the store, its
/// instructions are inlined under the pinned-skill delimiter; otherwise
/// the prompt carries the unresolved note and the review still
/// proceeds. Unit-tested directly for the same spawn reason as above.
#[test]
fn graph_preflight_reviewer_inlines_skill_when_store_has_it() {
    // Seeded store (empty sources): a present, fresh skill dir is
    // served without any network fetch.
    let dir = tempfile::tempdir().unwrap();
    let store = crate::dynamic_skills::SkillStore::new(dir.path().to_path_buf(), Vec::new(), 15);
    let skill_dir = dir.path().join("graph-reviewer");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# graph-reviewer\nCB25 seeded reviewer body.\n",
    )
    .unwrap();
    let checked = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        skill_dir.join(".canopy-skill.toml"),
        format!("source_url = \"test\"\ncommit_hash = \"test\"\nlast_checked = \"{checked}\"\n"),
    )
    .unwrap();

    let section = resolve_reviewer_skill_section(&store);
    assert!(
        section.contains("## Skill: graph-reviewer"),
        "resolved skill must use the pinned-skill delimiter: {section}"
    );
    assert!(
        section.contains("CB25 seeded reviewer body."),
        "resolved skill must inline the skill instructions: {section}"
    );

    // Unseeded store degrades to the unresolved note, never an error.
    let bare_dir = tempfile::tempdir().unwrap();
    let bare =
        crate::dynamic_skills::SkillStore::new(bare_dir.path().to_path_buf(), Vec::new(), 15);
    let note = resolve_reviewer_skill_section(&bare);
    assert!(
        note.contains("## Skill: graph-reviewer")
            && note.contains("Could not resolve pinned skill"),
        "missing skill must degrade to the unresolved note: {note}"
    );
}

#[test]
fn append_traversed_project_knowledge_reports_cap_full() {
    use crate::db::intelligence::IntelligenceNodeInput;
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("fact-cap".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("cap fact".to_string()),
        body: Some("body".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("cap-hash".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    let mut tagged: super::TaggedProjectKnowledge = Vec::new();
    let done = super::TaskTriggerHandler::append_traversed_project_knowledge(
        &db,
        &mut tagged,
        "cap-hash",
        None,
        20,
        0,
    )
    .unwrap();
    assert!(!done, "total cap of zero stops traversal");
    assert!(tagged.is_empty());
}

#[test]
fn append_traversed_project_knowledge_appends_within_cap() {
    use crate::db::intelligence::IntelligenceNodeInput;
    let dir = tempfile::tempdir().unwrap();
    let db = Database::new(&dir.path().join("t.db")).unwrap();
    db.upsert_intelligence_node(IntelligenceNodeInput {
        id: Some("fact-ok".to_string()),
        kind: Some("fact".to_string()),
        status: None,
        title: Some("ok fact".to_string()),
        body: Some("body".to_string()),
        body_replace: None,
        metadata: None,
        project_hash: Some(Some("ok-hash".to_string())),
        session_id: None,
        relations: None,
    })
    .unwrap();
    let mut tagged: super::TaggedProjectKnowledge = Vec::new();
    let more = super::TaskTriggerHandler::append_traversed_project_knowledge(
        &db,
        &mut tagged,
        "ok-hash",
        None,
        20,
        60,
    )
    .unwrap();
    assert!(more, "room remains under the total cap");
    assert_eq!(tagged.len(), 1);
    assert_eq!(tagged[0].1, "ok-hash");
}

/// CM49 req 5(b): `crew_update`'s eager sync skips a bound ensemble whose
/// graph is running, leaving its member rows and `crew_version_applied`
/// behind — while an identical idle graph is synced.
#[tokio::test]
async fn crew_update_eager_sync_skips_a_running_graph() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    call_resource_action(
        &handler,
        "crew",
        "create",
        params_id(CrewCreateParams {
            name: "eager".to_string(),
            description: None,
            members: Some(vec![crew_json_member("a"), crew_json_member("b")]),
            from_ensemble: None,
        }),
    )
    .await
    .unwrap();

    let (running_graph, running_ens) = build_crew_ensemble(&handler, &workdir, "Running G").await;
    let (_idle_graph, idle_ens) = build_crew_ensemble(&handler, &workdir, "Idle G").await;
    db.update_graph_status(&running_graph, GraphStatus::Running, None, None)
        .unwrap();

    // Grow the crew to three members.
    call_resource_action(
        &handler,
        "crew",
        "update",
        params_id(CrewUpdateParams {
            name: "eager".to_string(),
            members: Some(vec![
                crew_json_member("a"),
                crew_json_member("b"),
                crew_json_member("c"),
            ]),
            description: None,
            rename: None,
        }),
    )
    .await
    .unwrap();

    let running = db.get_ensemble_details(&running_ens).unwrap().unwrap();
    assert_eq!(
        running.members.len(),
        2,
        "a running graph's member rows are left behind"
    );
    assert_eq!(
        running.ensemble.crew_version_applied,
        Some(1),
        "a running graph's applied version is left behind"
    );

    let idle = db.get_ensemble_details(&idle_ens).unwrap().unwrap();
    assert_eq!(idle.members.len(), 3, "an idle graph is synced eagerly");
    assert_eq!(idle.ensemble.crew_version_applied, Some(2));
}
