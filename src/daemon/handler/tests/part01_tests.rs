//! Part 01 of `tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[test]
fn clamp_max_recent_pairs_defaults_and_clamps() {
    assert_eq!(super::clamp_max_recent_pairs(None), 15);
    assert_eq!(super::clamp_max_recent_pairs(Some(0)), 1);
    assert_eq!(super::clamp_max_recent_pairs(Some(999)), 50);
    assert_eq!(super::clamp_max_recent_pairs(Some(20)), 20);
}

#[test]
fn probe_json_keeps_slug_and_adds_display() {
    use crate::domain::canopy_config::CanopyConfig;
    use crate::domain::cli_config::CliConfig;
    let mut config = CanopyConfig::default();
    config.clis.push(CliConfig {
        name: "mistral".to_string(),
        provider: Some("Mistral AI".to_string()),
        tool_name: Some("Vibe".to_string()),
        ..Default::default()
    });
    assert_eq!(
        super::display_for_slug(&config, "mistral"),
        "Mistral AI · Vibe"
    );
    // Unknown slug and field-less config both render the slug.
    assert_eq!(super::display_for_slug(&config, "ghost"), "ghost");
    let mut bare = CanopyConfig::default();
    bare.clis.push(CliConfig {
        name: "mistral".to_string(),
        ..Default::default()
    });
    assert_eq!(super::display_for_slug(&bare, "mistral"), "mistral");
    // to_json itself keeps the slug identity key byte-identical.
    let report = crate::daemon::probe::ProbeReport {
        platform: "mistral".to_string(),
        model: None,
        outcome: crate::daemon::probe::ProbeOutcome::NotConfigured,
        duration_ms: 0,
        error: None,
        argv: None,
    };
    let v = report.to_json();
    assert_eq!(v["platform"], serde_json::json!("mistral"));
    assert!(v.get("platform_display").is_none());
}

/// The `#[tool]` macro locates a handler's parameter wrapper by the
/// literal ident `Parameters`, then generates
/// `schema_for_type::<Parameters<T>>()` for the advertised MCP
/// `input_schema`. Swapping in this crate's own `Parameters<T>` (for
/// enriched deserialization-failure messages) must not silently fall
/// back to an empty schema — clients still need the real shape to build
/// correct calls in the first place.
#[test]
fn tool_router_still_advertises_real_input_schemas_for_swapped_parameters_type() {
    let tools = TaskTriggerHandler::tool_router().list_all();

    let create_seed = tools
        .iter()
        .find(|tool| tool.name == "create_seed")
        .expect("create_seed tool should be registered");
    let props = create_seed
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("create_seed input_schema should have properties");
    assert!(props.contains_key("name"));
    assert!(props.contains_key("directives"));

    let intelligence_upsert = tools
        .iter()
        .find(|tool| tool.name == "intelligence_upsert")
        .expect("intelligence_upsert tool should be registered");
    let props = intelligence_upsert
        .input_schema
        .get("properties")
        .and_then(|p| p.as_object())
        .expect("intelligence_upsert input_schema should have properties");
    assert!(props.contains_key("node_data"));
}

/// Regression guard for the class of bug where a nested-object parameter
/// is advertised as a bare `$ref` into `$defs` with no sibling `type` at
/// the property level. A client that builds tool arguments from a
/// shallow read of the property schema (never resolving `$ref`) sees no
/// declared type there and falls back to sending the value as a string.
/// This has already happened twice — `graph_add_node.config` and
/// `intelligence_upsert.node_data` — so this test walks every
/// registered tool's whole surface instead of asserting on one tool.
#[test]
fn every_tool_property_self_declares_its_type() {
    let tools = TaskTriggerHandler::tool_router().list_all();
    let mut violations = Vec::new();

    for tool in &tools {
        let Some(properties) = tool
            .input_schema
            .get("properties")
            .and_then(|p| p.as_object())
        else {
            continue;
        };

        for (prop_name, prop_schema) in properties {
            // A `$ref` with a sibling `type` counts via `has_type`;
            // a bare `$ref` with no sibling `type` does not, which is
            // exactly the shape this guard rejects.
            let has_type = prop_schema.get("type").is_some();
            let has_combinator =
                prop_schema.get("anyOf").is_some() || prop_schema.get("oneOf").is_some();
            if !has_type && !has_combinator {
                violations.push(format!("{}.{prop_name}", tool.name));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "properties with no declared type at the property level — a client that doesn't \
             resolve $ref can't tell these apart from an untyped string parameter: {violations:?}"
    );
}

#[test]
fn node_tools_all_registered_and_small() {
    use std::collections::BTreeSet;
    let registered: BTreeSet<String> = TaskTriggerHandler::tool_router()
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    for name in super::NODE_TOOLS {
        assert!(
            registered.contains(*name),
            "NODE_TOOLS entry not registered: {name}"
        );
    }
    assert!(super::NODE_TOOLS.len() <= 32);
}

#[test]
fn node_tool_list_matches_node_tools() {
    use std::collections::BTreeSet;
    let names: BTreeSet<String> = super::node_tool_list()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    let expected: BTreeSet<String> = super::NODE_TOOLS.iter().map(|s| s.to_string()).collect();
    assert_eq!(names, expected);
}

/// B7 A1: the MCP tool inventory is byte-identical across the handler
/// decomposition. Sorted tool names must equal the frozen list, and every
/// tool's `description` + `input_schema` JSON hash must equal the frozen
/// snapshot. Any drift in names, descriptions, or schemas fails here.
#[test]
fn tool_inventory_is_byte_identical() {
    use sha2::{Digest, Sha256};
    fn hex(bytes: &[u8]) -> String {
        let mut digest = Sha256::new();
        digest.update(bytes);
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    let mut tools = TaskTriggerHandler::tool_router().list_all();
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, EXPECTED_TOOL_NAMES, "tool name inventory drifted");
    let rows: Vec<String> = tools
        .iter()
        .map(|t| {
            let desc = t.description.as_deref().unwrap_or("");
            let schema = serde_json::to_string(&t.input_schema).expect("schema serializes");
            format!(
                "{}|{}|{}",
                t.name,
                hex(desc.as_bytes()),
                hex(schema.as_bytes())
            )
        })
        .collect();
    assert_eq!(
        rows, EXPECTED_TOOL_ROWS,
        "tool description/schema inventory drifted"
    );
}

#[test]
fn validate_ensemble_members_rejects_below_minimum() {
    let members = vec![ensemble_member_params("claude")];
    let err = validate_ensemble_members(
        &members,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap_err();
    assert!(err.contains("2-8 members"), "{err}");
}

#[test]
fn validate_ensemble_members_rejects_above_maximum() {
    let members: Vec<_> = (0..9).map(|_| ensemble_member_params("claude")).collect();
    let err = validate_ensemble_members(
        &members,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap_err();
    assert!(err.contains("2-8 members"), "{err}");
}

/// CM52: the parallel cap comes from `graphs.ensemble_max_members_parallel`
/// (passed in as `MemberCaps`); an over-cap write is refused with the
/// setting key named so the user knows what to raise.
#[test]
fn validate_ensemble_members_refuses_over_configured_cap_naming_the_key() {
    let caps = crate::domain::graphs::MemberCaps {
        parallel: 4,
        sequential: 16,
    };
    let members: Vec<_> = (0..5).map(|_| ensemble_member_params("claude")).collect();
    let err = validate_ensemble_members(&members, EnsembleKind::Parallel, caps).unwrap_err();
    assert!(
        err.contains("2-4 members"),
        "message must show the live bound: {err}"
    );
    assert!(
        err.contains("graphs.ensemble_max_members_parallel"),
        "message must name the setting key: {err}"
    );
}

#[test]
fn validate_ensemble_members_accepts_boundary_counts() {
    let two: Vec<_> = (0..2).map(|_| ensemble_member_params("claude")).collect();
    assert!(validate_ensemble_members(
        &two,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default()
    )
    .is_ok());
    let eight: Vec<_> = (0..8).map(|_| ensemble_member_params("claude")).collect();
    assert!(validate_ensemble_members(
        &eight,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default()
    )
    .is_ok());
}

/// CM36: a sequential kind dispatches one member at a time, so its list
/// may be longer than parallel's 8 — 12 round_robin members are accepted.
#[test]
fn validate_ensemble_members_round_robin_accepts_12() {
    let m: Vec<_> = (0..12)
        .map(|i| ensemble_member_params(&format!("p{i}")))
        .collect();
    assert!(validate_ensemble_members(
        &m,
        EnsembleKind::RoundRobin,
        crate::domain::graphs::MemberCaps::default()
    )
    .is_ok());
}

/// CM36: the sequential ceiling is 16, and the error names the kind.
#[test]
fn validate_ensemble_members_cascade_rejects_17() {
    let m: Vec<_> = (0..17)
        .map(|i| ensemble_member_params(&format!("p{i}")))
        .collect();
    let err = validate_ensemble_members(
        &m,
        EnsembleKind::Cascade,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap_err();
    assert!(err.contains("cascade"), "{err}");
    assert!(err.contains("2-16 members"), "{err}");
}

#[test]
fn validate_ensemble_members_rejects_empty_platform() {
    let members = vec![
        ensemble_member_params("claude"),
        ensemble_member_params("  "),
    ];
    let err = validate_ensemble_members(
        &members,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap_err();
    assert!(err.contains("platform"), "{err}");
}

/// `prompt_override` is normalized like `model`: trimmed, and a
/// whitespace-only override collapses to `None` (use the shared
/// prompt), not an override of empty string.
#[test]
fn validate_ensemble_members_trims_prompt_override_and_blanks_to_none() {
    let members = vec![
        ensemble_member_params_with_prompt("claude", "  review for security  "),
        {
            let mut m = ensemble_member_params("codex");
            m.prompt_override = Some("   ".to_string());
            m
        },
    ];
    let result = validate_ensemble_members(
        &members,
        EnsembleKind::Parallel,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap();
    assert_eq!(result[0].2.as_deref(), Some("review for security"));
    assert_eq!(result[1].2, None);
}

/// CM23: a member's own `timeout_minutes` follows the ensemble-level
/// convention — negative is rejected, `0` is a legitimate immediate
/// timeout.
#[test]
fn validate_ensemble_members_rejects_negative_member_timeout() {
    let members = vec![
        {
            let mut m = ensemble_member_params("claude");
            m.timeout_minutes = Some(-1);
            m
        },
        ensemble_member_params("openrouter"),
    ];
    let err = validate_ensemble_members(
        &members,
        EnsembleKind::Cascade,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap_err();
    assert!(err.contains("timeout_minutes"), "{err}");
}

#[test]
fn validate_ensemble_members_accepts_zero_member_timeout() {
    let members = vec![
        {
            let mut m = ensemble_member_params("claude");
            m.timeout_minutes = Some(0);
            m
        },
        ensemble_member_params("openrouter"),
    ];
    let result = validate_ensemble_members(
        &members,
        EnsembleKind::Cascade,
        crate::domain::graphs::MemberCaps::default(),
    )
    .unwrap();
    assert_eq!(result[0].3, Some(0));
}

#[test]
fn effective_member_prompt_prefers_override_over_shared() {
    assert_eq!(
        effective_member_prompt(Some("angle-specific prompt"), "shared prompt"),
        "angle-specific prompt"
    );
    assert_eq!(
        effective_member_prompt(None, "shared prompt"),
        "shared prompt"
    );
}

/// F1's "no nested ensembles" rule: wiring into a node that already
/// belongs to another ensemble (as a member or as its join) must be
/// rejected, not silently accepted.
#[test]
fn validate_node_not_ensemble_owned_rejects_member_and_join_nodes() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph_spec(&GraphSpec {
        id: "spec-1".to_string(),
        graph_id: None,
        name: "spec-1".to_string(),
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
    let now = chrono::Utc::now();
    db.insert_graph_node(&GraphNode {
        id: "kickoff".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: "kickoff".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true", "success_condition": "exit_code_0"}),
        position: 1,
        created_at: now,
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
        created_at: now,
    })
    .unwrap();
    let member_nodes = vec![
        GraphNode {
            id: "m1".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-1".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "claude"}),
            position: 2,
            created_at: now,
        },
        GraphNode {
            id: "m2".to_string(),
            spec_id: Some("spec-1".to_string()),
            graph_id: None,
            name: "member-2".to_string(),
            kind: GraphNodeKind::Agent,
            config: serde_json::json!({"platform": "codex"}),
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
        mutex: Vec::new(),
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
        infra_retry_limit: None,
        infra_crash_max_seconds: None,
        infra_backoff_seconds: None,
        on_pass_to: "arbiter".to_string(),
        on_fail_to: None,
        kind: EnsembleKind::Parallel,
        round_robin_index: None,
        created_at: now,

        crew_id: None,
        crew_name: None,
        crew_version_applied: None,
    };
    let members = vec![
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m1".to_string(),
            position: 0,
            platform: "claude".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
        EnsembleMember {
            ensemble_id: "ens1".to_string(),
            node_id: "m2".to_string(),
            position: 1,
            platform: "codex".to_string(),
            model: None,
            prompt_override: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
        },
    ];
    db.insert_ensemble_unit(&ensemble, &members, &member_nodes, &join_node, &edges)
        .unwrap();

    assert!(validate_node_not_ensemble_owned(&db, "m1").is_err());
    assert!(validate_node_not_ensemble_owned(&db, "join1").is_err());
    assert!(validate_node_not_ensemble_owned(&db, "kickoff").is_ok());
    assert!(validate_node_not_ensemble_owned(&db, "arbiter").is_ok());
}

#[test]
fn spec_delete_refuses_graph_bound_spec_with_actionable_error() {
    let spec = spec_with_status("graph-1", "spec-1", 1, GraphSpecStatus::Pending);

    let error = validate_spec_deletable(&spec).unwrap_err();

    assert!(
        error.contains("spec-1"),
        "error should name the spec: {error}"
    );
    assert!(
        error.contains("graph-1"),
        "error should name the graph: {error}"
    );
    assert!(
        error.contains("remove it from the graph") || error.contains("delete the graph"),
        "error should be actionable: {error}"
    );
}

#[test]
fn spec_delete_allows_standalone_spec() {
    let mut spec = spec_with_status("graph-1", "spec-1", 1, GraphSpecStatus::Pending);
    spec.graph_id = None;

    assert!(validate_spec_deletable(&spec).is_ok());
}

#[test]
fn graph_reset_failed_graph_resets_incomplete_specs_to_pending() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-done",
        1,
        GraphSpecStatus::Completed,
    ))
    .unwrap();
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-failed",
        2,
        GraphSpecStatus::Failed,
    ))
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Draft);
    assert!(lp.completed_at.is_none());

    let specs = db.list_graph_specs(&graph_id).unwrap();
    let done = specs.iter().find(|s| s.id == "spec-done").unwrap();
    let failed = specs.iter().find(|s| s.id == "spec-failed").unwrap();
    assert_eq!(failed.status, GraphSpecStatus::Pending);
    assert!(failed.completed_at.is_none());
    // Completed specs are preserved when `specs` isn't given explicitly.
    assert_eq!(done.status, GraphSpecStatus::Completed);
}

/// CM47: notes are keyed by spec, not run — a plain reset keeps them,
/// and only `clear_notes: true` removes them.
#[test]
fn graph_reset_preserves_notes_unless_clear_notes() {
    use crate::db::graphs::notes::NewSpecNote;
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-failed",
        1,
        GraphSpecStatus::Failed,
    ))
    .unwrap();
    db.spec_note_write(NewSpecNote {
        spec_id: "spec-failed",
        owner_kind: "node",
        owner_id: "impl",
        owner_name: "Implementer",
        body: "halfway there",
        author_platform: None,
        author_model: None,
    })
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));
    assert_eq!(db.spec_notes_get("spec-failed").unwrap().len(), 1);

    let result = perform_graph_reset(&db, &graph_id, None, true).unwrap();
    assert!(!result.is_error.unwrap_or(false));
    assert!(
        db.spec_notes_get("spec-failed").unwrap().is_empty(),
        "clear_notes: true must remove the reset specs' notes"
    );
}

/// `Interrupted` is treated like any other non-completed status: a plain
/// `graph_reset` (no explicit `specs`) returns it to `pending`, same as it
/// would `failed` — an interruption isn't the special case here, only
/// `completed` is (preserved unless named explicitly).
#[test]
fn graph_reset_resets_interrupted_spec_to_pending() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-interrupted",
        1,
        GraphSpecStatus::Interrupted,
    ))
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let specs = db.list_graph_specs(&graph_id).unwrap();
    let interrupted = specs.iter().find(|s| s.id == "spec-interrupted").unwrap();
    assert_eq!(interrupted.status, GraphSpecStatus::Pending);
    assert!(interrupted.started_at.is_none());
    assert!(interrupted.completed_at.is_none());
}

#[test]
fn graph_reset_with_explicit_specs_resets_completed_spec_too() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-done",
        1,
        GraphSpecStatus::Completed,
    ))
    .unwrap();

    let result =
        perform_graph_reset(&db, &graph_id, Some(&["spec-done".to_string()]), false).unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let spec = db.get_graph_spec("spec-done").unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Pending);
    assert!(spec.completed_at.is_none());
}

#[test]
fn graph_reset_blanket_preserves_admin_skipped_specs() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    // Administrative skip — must survive blanket reset.
    db.insert_graph_spec(&GraphSpec {
        id: "spec-admin-skipped".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "admin-skipped".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Skipped,
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: Some("admin".to_string()),
        completed_via_reason: Some("premise false".to_string()),
        completed_via_at: Some(chrono::Utc::now()),
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-failed",
        2,
        GraphSpecStatus::Failed,
    ))
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let specs = db.list_graph_specs(&graph_id).unwrap();
    let skipped = specs.iter().find(|s| s.id == "spec-admin-skipped").unwrap();
    let failed = specs.iter().find(|s| s.id == "spec-failed").unwrap();
    assert_eq!(
        skipped.status,
        GraphSpecStatus::Skipped,
        "admin-skipped spec must survive a blanket reset"
    );
    assert_eq!(failed.status, GraphSpecStatus::Pending);

    let text = format!("{:?}", result.content);
    assert!(text.contains("1 spec(s) reset"), "{text}");
    assert!(text.contains("1 skipped spec(s) preserved"), "{text}");
}

#[test]
fn graph_reset_blanket_resets_engine_skipped_spec() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    // Engine-produced skip (no admin provenance) — must be reset.
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-engine-skipped",
        1,
        GraphSpecStatus::Skipped,
    ))
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let spec = db.get_graph_spec("spec-engine-skipped").unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Pending,
        "engine-skipped spec must be reset by blanket reset"
    );

    let text = format!("{:?}", result.content);
    assert!(text.contains("1 spec(s) reset"), "{text}");
    assert!(
        !text.contains("skipped"),
        "engine-skipped spec must not be counted as preserved: {text}"
    );
}

#[test]
fn graph_reset_explicit_specs_resets_skipped_spec_and_clears_provenance() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    db.insert_graph_spec(&GraphSpec {
        id: "spec-admin-skipped".to_string(),
        graph_id: Some(graph_id.clone()),
        name: "admin-skipped".to_string(),
        description: None,
        position: 1,
        parallelizable: false,
        status: GraphSpecStatus::Skipped,
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
        spec_start_head: None,
        spec_start_dirty: None,
        spec_end_dirty: None,
        spec_end_dirty_paths: None,
        spec_committed_head: None,
        workdir: None,
        completed_via: Some("admin".to_string()),
        completed_via_reason: Some("premise false".to_string()),
        completed_via_at: Some(chrono::Utc::now()),
        spec_type: None,
        spec_scope: None,
        spec_code: None,
    })
    .unwrap();

    let result = perform_graph_reset(
        &db,
        &graph_id,
        Some(&["spec-admin-skipped".to_string()]),
        false,
    )
    .unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let spec = db.get_graph_spec("spec-admin-skipped").unwrap().unwrap();
    assert_eq!(
        spec.status,
        GraphSpecStatus::Pending,
        "explicitly naming a skipped spec must reset it"
    );
    assert!(
        spec.completed_via.is_none(),
        "explicit reset must clear completed_via"
    );
    assert!(
        spec.completed_via_reason.is_none(),
        "explicit reset must clear completed_via_reason"
    );
    assert!(
        spec.completed_via_at.is_none(),
        "explicit reset must clear completed_via_at"
    );
}

#[test]
fn graph_reset_no_skipped_specs_omits_preserved_note() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Failed);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-failed",
        1,
        GraphSpecStatus::Failed,
    ))
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    let text = format!("{:?}", result.content);
    assert!(text.contains("1 spec(s) reset"), "{text}");
    assert!(
        !text.contains("skipped"),
        "no skipped note when no specs were skipped: {text}"
    );
}

/// A graph whose last run was against a queue has empty (or irrelevant)
/// bound specs — the queue's *members* are what actually need resetting.
/// `graph_reset` must find them via the graph's persisted
/// `active_run_queue_id`, reset every non-completed one back to pending
/// (completed members untouched), and report the real count — not "0
/// spec(s) reset", the false report from the incident this spec fixes.
#[test]
fn graph_reset_queue_run_resets_pending_queue_members_and_reports_count() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-queue-reset-test".to_string();
    db.insert_graph(&Graph {
        archived: false,
        paused_by_reconciliation: false,
        allow_dirty_start: false,
        require_spec_type: false,
        infra_node_id: None,
        id: graph_id.clone(),
        name: "Graph".to_string(),
        description: None,
        workdir: dir.path().to_string_lossy().to_string(),
        status: GraphStatus::Failed,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: Some("queue-1".to_string()),
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();

    let standalone = |id: &str, position: i64, status: GraphSpecStatus| GraphSpec {
        id: id.to_string(),
        graph_id: None,
        name: id.to_string(),
        description: None,
        position,
        parallelizable: false,
        status,
        started_at: None,
        completed_at: Some(chrono::Utc::now()),
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
    db.insert_graph_spec(&standalone("queue-done", 1, GraphSpecStatus::Completed))
        .unwrap();
    db.insert_graph_spec(&standalone("queue-failed", 2, GraphSpecStatus::Failed))
        .unwrap();
    db.insert_graph_spec(&standalone("queue-pending", 3, GraphSpecStatus::Pending))
        .unwrap();
    db.insert_queue(&Queue {
        id: "queue-1".to_string(),
        name: "queue-1".to_string(),
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    for spec_id in ["queue-done", "queue-failed", "queue-pending"] {
        db.append_queue_member("queue-1", spec_id, None).unwrap();
    }

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));
    let text = format!("{:?}", result.content);
    assert!(text.contains("2 spec(s) reset"), "{text}");

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Draft);

    let done = db.get_graph_spec("queue-done").unwrap().unwrap();
    let failed = db.get_graph_spec("queue-failed").unwrap().unwrap();
    let pending = db.get_graph_spec("queue-pending").unwrap().unwrap();
    assert_eq!(
        done.status,
        GraphSpecStatus::Completed,
        "completed queue member must be left untouched"
    );
    assert_eq!(failed.status, GraphSpecStatus::Pending);
    assert!(failed.completed_at.is_none());
    assert_eq!(pending.status, GraphSpecStatus::Pending);
}

/// A `Running`-status graph with no actual in-flight run is now
/// resettable — the old status-only guard would have refused this
/// (status alone said "running"), but the ground truth is the
/// `graph_runs` table, and here it has nothing running under this graph.
/// This is the intended flip side of
/// `graph_reset_rejects_graph_with_in_flight_run_even_when_status_is_paused`:
/// status and a run's lifetime are independent in both directions.
#[test]
fn graph_reset_allows_running_status_graph_with_no_in_flight_run() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Running);

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(!result.is_error.unwrap_or(false));

    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Draft);
}

/// The ground truth is the `graph_runs` table, not `lp.status` — a graph
/// left `paused` by a sibling node's blocker report while a *different*
/// node keeps executing must still refuse the reset, naming the node and
/// run still in flight so the caller can wait or kill it deliberately
/// (the 2026-08-05 incident this guards against: a reset silently killed
/// and reset the still-running node's spec, and its late completion
/// routed an edge and failed the graph out from under the fresh dispatch
/// the reset then launched).
#[test]
fn graph_reset_rejects_graph_with_in_flight_run_even_when_status_is_paused() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Paused);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-a",
        1,
        GraphSpecStatus::Running,
    ))
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-resilience".to_string(),
        spec_id: Some("spec-a".to_string()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "sleep 30"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-live".to_string(),
        graph_id: graph_id.clone(),
        spec_id: "spec-a".to_string(),
        node_id: "node-resilience".to_string(),
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
    })
    .unwrap();

    let result = perform_graph_reset(&db, &graph_id, None, false).unwrap();
    assert!(result.is_error.unwrap_or(false));
    let text = format!("{:?}", result.content);
    assert!(text.contains("resilience"), "{text}");
    assert!(text.contains("run-live"), "{text}");

    // Nothing touched: status, spec, and the run row all untouched.
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    assert_eq!(lp.status, GraphStatus::Paused);
    let spec = db.get_graph_spec("spec-a").unwrap().unwrap();
    assert_eq!(spec.status, GraphSpecStatus::Running);
    let run = db.get_graph_run("run-live").unwrap().unwrap();
    assert_eq!(run.status, GraphRunStatus::Running);
}

#[test]
fn graph_reset_missing_graph_returns_not_found_error() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let result = perform_graph_reset(&db, "does-not-exist", None, false).unwrap();
    assert!(result.is_error.unwrap_or(false));
    let text = format!("{:?}", result.content);
    assert!(text.contains("not found"), "{text}");
}

/// `graph_run` must keep refusing a `failed` graph, and the refusal must
/// point at both sanctioned ways out: `graph_reset` (manual) and
/// `graph_schedule_autorun` (self-service, for a quota-failed graph).
#[test]
fn graph_run_status_guard_rejects_failed_graph_and_names_both_ways_out() {
    let err = graph_run_status_guard("graph-1", GraphStatus::Failed).unwrap_err();
    assert!(err.contains("graph_reset"), "{err}");
    assert!(err.contains("graph_schedule_autorun"), "{err}");
}

#[test]
fn graph_run_status_guard_rejects_completed_graph() {
    let err = graph_run_status_guard("graph-1", GraphStatus::Completed).unwrap_err();
    assert!(err.contains("graph_reset"), "{err}");
}

#[test]
fn graph_run_status_guard_rejects_running_graph_with_graph_id() {
    let err = graph_run_status_guard("graph-1", GraphStatus::Running).unwrap_err();
    assert!(err.contains("graph-1"), "{err}");
    assert!(err.contains("already running"), "{err}");
}

#[test]
fn graph_run_status_guard_accepts_draft_and_paused_graphs() {
    assert!(graph_run_status_guard("graph-1", GraphStatus::Draft).is_ok());
    assert!(graph_run_status_guard("graph-1", GraphStatus::Paused).is_ok());
}

// ── find_in_flight_run: graph_run's dispatch guard ────────────────────

#[test]
fn find_in_flight_run_none_when_nothing_running() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Paused);
    assert!(find_in_flight_run(&db, &graph_id).unwrap().is_none());
}

/// `graph_run` must refuse a duplicate dispatch while a node executes,
/// regardless of the graph's own status — a `paused` graph can still have
/// a live run when a sibling node's blocker report flipped status
/// without terminating it (see `graph_reset_rejects_graph_with_in_flight_run_even_when_status_is_paused`
/// for the same ground truth on the reset side).
#[test]
fn find_in_flight_run_finds_running_row_regardless_of_graph_status() {
    let (_dir, db, graph_id) = graph_reset_fixture(GraphStatus::Paused);
    db.insert_graph_spec(&spec_with_status(
        &graph_id,
        "spec-a",
        1,
        GraphSpecStatus::Running,
    ))
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "node-resilience".to_string(),
        spec_id: Some("spec-a".to_string()),
        graph_id: None,
        name: "resilience".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "sleep 30"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-live".to_string(),
        graph_id: graph_id.clone(),
        spec_id: "spec-a".to_string(),
        node_id: "node-resilience".to_string(),
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
    })
    .unwrap();

    let run = find_in_flight_run(&db, &graph_id)
        .unwrap()
        .expect("the still-running row must be found even though status is paused");
    assert_eq!(run.id, "run-live");

    let node_name = node_name_for_error(&db, &run.node_id).unwrap();
    let message = in_flight_run_error(&graph_id, &run.id, &node_name, run.started_at);
    assert!(message.contains("resilience"), "{message}");
    assert!(message.contains("run-live"), "{message}");
}

#[test]
fn validate_node_config_rejects_double_encoded_string() {
    let config = serde_json::json!("{\"platform\": \"mimo\"}");
    let error = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(error.contains("must be a JSON object"), "{error}");
}

#[test]
fn validate_node_config_agent_requires_platform_or_cli() {
    let config = serde_json::json!({ "model": "opus" });
    let error = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(error.contains("'agent'"), "{error}");
    assert!(error.contains("platform"), "{error}");
}

/// CM49: a crew is the agent config's harness alternative — accepted
/// alone, refused beside a literal pair.
#[test]
fn validate_node_config_agent_crew_is_the_harness_alternative() {
    assert!(validate_node_config(
        GraphNodeKind::Agent,
        &serde_json::json!({ "crew": "implementers" })
    )
    .is_ok());
    let with_platform = serde_json::json!({ "crew": "implementers", "platform": "claude" });
    let error = validate_node_config(GraphNodeKind::Agent, &with_platform).unwrap_err();
    assert!(
        error.contains("'crew' or 'platform', never both"),
        "{error}"
    );
    let with_cli = serde_json::json!({ "crew": "implementers", "cli": "codex" });
    let error = validate_node_config(GraphNodeKind::Agent, &with_cli).unwrap_err();
    assert!(
        error.contains("'crew' or 'platform', never both"),
        "{error}"
    );
}

#[test]
fn validate_node_config_check_requires_command() {
    let config = serde_json::json!({ "timeout_seconds": 30 });
    let error = validate_node_config(GraphNodeKind::Check, &config).unwrap_err();
    assert!(error.contains("'check'"), "{error}");
    assert!(error.contains("command"), "{error}");
}

#[test]
fn validate_node_config_gate_requires_value_for_output_contains() {
    let config = serde_json::json!({ "evaluate": "output_contains" });
    let error = validate_node_config(GraphNodeKind::Gate, &config).unwrap_err();
    assert!(error.contains("'gate'"), "{error}");
    assert!(error.contains("value"), "{error}");

    let config_without_evaluate = serde_json::json!({});
    let error = validate_node_config(GraphNodeKind::Gate, &config_without_evaluate).unwrap_err();
    assert!(error.contains("value"), "{error}");
}

#[test]
fn validate_node_config_accepts_valid_configs() {
    assert!(validate_node_config(
        GraphNodeKind::Agent,
        &serde_json::json!({ "platform": "claude" })
    )
    .is_ok());
    assert!(
        validate_node_config(GraphNodeKind::Agent, &serde_json::json!({ "cli": "codex" })).is_ok()
    );
    assert!(validate_node_config(
        GraphNodeKind::Check,
        &serde_json::json!({ "command": "cargo test" })
    )
    .is_ok());
    assert!(validate_node_config(
        GraphNodeKind::Gate,
        &serde_json::json!({ "evaluate": "output_contains", "value": "ok" })
    )
    .is_ok());
}

#[test]
fn validate_node_config_router_accepts_valid_shape() {
    let config = router_config(&two_routes_json(), "retry");
    assert!(validate_node_config(GraphNodeKind::Router, &config).is_ok());
}

#[test]
fn validate_node_config_router_rejects_fewer_than_two_routes() {
    let config = router_config(
        &serde_json::json!([{ "label": "retry", "description": "Retry." }]),
        "retry",
    );
    let error = validate_node_config(GraphNodeKind::Router, &config).unwrap_err();
    assert!(error.contains("at least 2 routes"), "{error}");
}

#[test]
fn validate_node_config_router_rejects_no_fallback() {
    let config = serde_json::json!({ "routes": two_routes_json() });
    let error = validate_node_config(GraphNodeKind::Router, &config).unwrap_err();
    assert!(error.contains("fallback"), "{error}");
}

#[test]
fn validate_node_config_router_rejects_missing_routes_field() {
    let error = validate_node_config(GraphNodeKind::Router, &serde_json::json!({})).unwrap_err();
    assert!(error.contains("'routes'"), "{error}");
}

/// The exact incident shape (graph `824de730-7fec-4031-800a-7933d2cf94c1`,
/// node `d0458fe0-24b8-4a29-8a69-7bb0e742e046`): an agent node config
/// with `prompt` instead of `prompt_template` must fail loudly, naming
/// `prompt_template` as the field that's actually read — not be accepted
/// and silently run on the bare fallback template.
#[test]
fn validate_node_config_agent_rejects_prompt_key_naming_prompt_template() {
    let config = serde_json::json!({ "platform": "claude", "prompt": "do the thing" });
    let error = validate_node_config(GraphNodeKind::Agent, &config).unwrap_err();
    assert!(error.contains("'prompt'"), "{error}");
    assert!(
        error.contains("prompt_template"),
        "error must name the correct key: {error}"
    );
}

#[test]
fn validate_node_config_rejects_unknown_key_for_every_kind() {
    let agent_error = validate_node_config(
        GraphNodeKind::Agent,
        &serde_json::json!({ "platform": "claude", "unexpected_field": true }),
    )
    .unwrap_err();
    assert!(agent_error.contains("unexpected_field"), "{agent_error}");
    assert!(agent_error.contains("'agent'"), "{agent_error}");

    let check_error = validate_node_config(
        GraphNodeKind::Check,
        &serde_json::json!({ "command": "true", "unexpected_field": true }),
    )
    .unwrap_err();
    assert!(check_error.contains("unexpected_field"), "{check_error}");

    let gate_error = validate_node_config(
            GraphNodeKind::Gate,
            &serde_json::json!({ "evaluate": "output_contains", "value": "ok", "unexpected_field": true }),
        )
        .unwrap_err();
    assert!(gate_error.contains("unexpected_field"), "{gate_error}");

    let router_error = validate_node_config(
            GraphNodeKind::Router,
            &serde_json::json!({ "routes": two_routes_json(), "fallback": "retry", "unexpected_field": true }),
        )
        .unwrap_err();
    assert!(router_error.contains("unexpected_field"), "{router_error}");
}

/// `commit_rights` (B37) is engine-checked on every node kind
/// uniformly, so it must be accepted everywhere, not just on agent
/// nodes.
#[test]
fn validate_node_config_accepts_commit_rights_on_every_kind() {
    assert!(validate_node_config(
        GraphNodeKind::Agent,
        &serde_json::json!({ "platform": "claude", "commit_rights": true })
    )
    .is_ok());
    assert!(validate_node_config(
        GraphNodeKind::Check,
        &serde_json::json!({ "command": "true", "commit_rights": true })
    )
    .is_ok());
}

/// `require_report` is an agent-only key (unlike `commit_rights`, which
/// is engine-checked on every kind) — accepted on `agent`, rejected as
/// unrecognized everywhere else, since only an agent node's process exit
/// can be judged against a self-report.
#[test]
fn validate_node_config_accepts_require_report_on_agent_only() {
    assert!(validate_node_config(
        GraphNodeKind::Agent,
        &serde_json::json!({ "platform": "claude", "require_report": true })
    )
    .is_ok());

    let check_error = validate_node_config(
        GraphNodeKind::Check,
        &serde_json::json!({ "command": "true", "require_report": true }),
    )
    .unwrap_err();
    assert!(check_error.contains("require_report"), "{check_error}");
}

/// Neither `prompt_template` nor `prompt_preset` is still a VALID agent
/// config (the bare fallback template is deliberately kept reachable —
/// see the spec's "keep the fallback" constraint) — this only rejects
/// keys the engine never reads, not this legitimate default-reliant
/// shape. Visibility into "this node runs on the default" is a separate
/// concern, handled by `agent_prompt_source`/`graph_node_json`'s
/// `prompt_source` field, not by rejecting the config outright.
#[test]
fn validate_node_config_agent_without_prompt_template_or_preset_is_still_valid() {
    assert!(validate_node_config(
        GraphNodeKind::Agent,
        &serde_json::json!({ "platform": "claude" })
    )
    .is_ok());
}

#[test]
fn unknown_config_keys_is_empty_for_join_regardless_of_content() {
    let config = serde_json::json!({ "anything": "goes", "ensemble_id": "e1" });
    let map = config.as_object().unwrap();
    assert!(unknown_config_keys(GraphNodeKind::Join, map).is_empty());
}
