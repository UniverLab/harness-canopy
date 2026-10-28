//! Part 02 of `tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

#[test]
fn blueprints_are_listed_after_a_fresh_startup_and_reseeding_is_idempotent() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let names_after_first_start: Vec<String> = db
        .list_blueprints()
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    for expected in [
        "implementer",
        "cargo-gates",
        "reviewer-committer",
        "commit-check",
        "resilience",
    ] {
        assert!(
            names_after_first_start.contains(&expected.to_string()),
            "expected builtin '{expected}' after fresh startup, got {names_after_first_start:?}"
        );
    }

    // Simulate a second daemon startup against the same database.
    db.seed_builtin_blueprints().unwrap();
    let names_after_second_start: Vec<String> = db
        .list_blueprints()
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert_eq!(
        names_after_first_start.len(),
        names_after_second_start.len(),
        "reseeding must not duplicate builtins"
    );
}

#[test]
fn custom_blueprint_create_list_delete_round_trip_and_builtin_delete_refused() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let custom = Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "tdd-implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({ "platform": "claude", "prompt": "write a failing test first" }),
        builtin: false,
        created_at: chrono::Utc::now(),
    };
    db.insert_blueprint(&custom).unwrap();

    let names: Vec<String> = db
        .list_blueprints()
        .unwrap()
        .into_iter()
        .map(|b| b.name)
        .collect();
    assert!(names.contains(&"tdd-implementer".to_string()));

    let removed = db.delete_blueprint_by_name("tdd-implementer").unwrap();
    assert!(removed);
    assert!(db
        .get_blueprint_by_name("tdd-implementer")
        .unwrap()
        .is_none());

    // Deleting a builtin is refused at the validation layer with an
    // actionable message, before ever touching the DB.
    let builtin = db
        .get_blueprint_by_name("implementer")
        .unwrap()
        .expect("builtin should exist");
    let error = super::validate_blueprint_deletable(&builtin).unwrap_err();
    assert!(error.contains("implementer"));
    assert!(error.contains("cannot be deleted"));
}

#[test]
fn graph_add_node_from_blueprint_with_override_merges_config_and_override_wins() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    // The builtin carries no platform (see `builtin_blueprint_specs`),
    // so the caller must supply one via `config_overrides` — this also
    // exercises the required-override path with a successful call.
    let mut overrides = serde_json::Map::new();
    overrides.insert("platform".to_string(), serde_json::json!("claude"));
    overrides.insert("model".to_string(), serde_json::json!("opus"));

    let (kind, config) =
        resolve_node_kind_and_config(&db, None, None, Some("implementer"), Some(overrides))
            .expect("blueprint resolution should succeed");

    assert_eq!(kind, GraphNodeKind::Agent);
    assert_eq!(config["platform"], "claude");
    assert_eq!(config["model"], "opus");
    // The templated key (prompt_preset) survives the shallow merge.
    assert_eq!(config["prompt_preset"], "implementer");
}

/// The caller-must-supply-a-harness contract, from the other side: an
/// agent blueprint's config omits `platform`/`cli` by design (see
/// `builtin_blueprint_specs`), so creating a node from one without
/// `config_overrides` supplying it must fail — never silently fall back
/// to a default harness — with a message that names the missing field
/// and says the omission is intentional.
#[test]
fn graph_add_node_from_agent_blueprint_without_harness_override_fails_with_useful_message() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let error =
        resolve_node_kind_and_config(&db, None, None, Some("implementer"), None).unwrap_err();

    assert!(error.contains("implementer"), "{error}");
    assert!(error.contains("platform"), "{error}");
    assert!(
        error.contains("intentionally"),
        "error should explain the blueprint intentionally omits a harness: {error}"
    );
}

/// A custom blueprint that pins its own platform (the pre-existing,
/// still-supported pattern) needs no override at all — only the
/// harness-free builtins require one.
#[test]
fn graph_add_node_from_custom_blueprint_with_platform_needs_no_override() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_blueprint(&Blueprint {
        id: uuid::Uuid::new_v4().to_string(),
        name: "my-pinned-implementer".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude", "prompt": "go implement it"}),
        builtin: false,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let (kind, config) =
        resolve_node_kind_and_config(&db, None, None, Some("my-pinned-implementer"), None)
            .expect("a custom blueprint with its own platform should not require an override");

    assert_eq!(kind, GraphNodeKind::Agent);
    assert_eq!(config["platform"], "claude");
}

#[test]
fn graph_add_node_with_unknown_blueprint_lists_available_names() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let error = validate_blueprint_exists(&db, "does-not-exist").unwrap_err();

    assert!(error.contains("does-not-exist"));
    assert!(error.contains("implementer"));
    assert!(error.contains("cargo-gates"));
    assert!(error.contains("reviewer-committer"));
    assert!(error.contains("commit-check"));
    assert!(error.contains("resilience"));
}

#[test]
fn resolve_sync_agent_id_reads_canopy_header() {
    let request = axum::http::Request::builder()
        .header(CANOPY_AGENT_ID_HEADER, "agent-123")
        .body(())
        .unwrap();
    let (parts, _) = request.into_parts();

    assert_eq!(
        header_str(&parts, CANOPY_AGENT_ID_HEADER),
        Some("agent-123")
    );
}

#[test]
fn resolve_sync_agent_id_requires_canopy_header() {
    let request = axum::http::Request::builder()
        .header("x-canopy-session-name", "cedro")
        .header("x-canopy-workdir", "/tmp/workdir")
        .body(())
        .unwrap();
    let (parts, _) = request.into_parts();

    assert_eq!(header_str(&parts, CANOPY_AGENT_ID_HEADER), None);

    let error = missing_sync_identity_error();
    assert_eq!(error.message, MISSING_SYNC_IDENTITY_MESSAGE);
}

#[test]
fn queue_crud_and_ordering_round_trips() {
    let (_dir, db) = queue_test_db();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.insert_graph_spec(&standalone_spec(id)).unwrap();
    }
    insert_queue(&db, "queue-1");

    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();
    db.append_queue_member("queue-1", "spec-c", None).unwrap();

    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["spec-a", "spec-b", "spec-c"]
    );

    let details = db.get_queue_details("queue-1").unwrap().unwrap();
    assert_eq!(details.queue.id, "queue-1");
    assert_eq!(
        details
            .members
            .iter()
            .map(|spec| spec.id.clone())
            .collect::<Vec<_>>(),
        vec!["spec-a", "spec-b", "spec-c"]
    );

    assert!(db.remove_queue_member("queue-1", "spec-b").unwrap());
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["spec-a", "spec-c"]
    );
    assert!(!db.remove_queue_member("queue-1", "spec-b").unwrap());

    assert!(db.list_queues().unwrap().iter().any(|p| p.id == "queue-1"));
}

#[test]
fn queue_reorder_is_total_and_deterministic() {
    let (_dir, db) = queue_test_db();
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.insert_graph_spec(&standalone_spec(id)).unwrap();
    }
    insert_queue(&db, "queue-1");
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.append_queue_member("queue-1", id, None).unwrap();
    }

    let current = db.list_queue_member_spec_ids("queue-1").unwrap();
    let order = vec![
        "spec-c".to_string(),
        "spec-a".to_string(),
        "spec-b".to_string(),
    ];
    assert!(validate_queue_reorder(&current, &order).is_ok());

    db.reorder_queue_members("queue-1", &order).unwrap();
    assert_eq!(db.list_queue_member_spec_ids("queue-1").unwrap(), order);
}

#[test]
fn queue_reorder_rejects_partial_list() {
    let current = vec![
        "spec-a".to_string(),
        "spec-b".to_string(),
        "spec-c".to_string(),
    ];
    let order = vec!["spec-a".to_string(), "spec-b".to_string()];

    let error = validate_queue_reorder(&current, &order).unwrap_err();
    assert!(error.contains("exactly once; got 2"), "{error}");
}

#[test]
fn queue_reorder_rejects_unknown_spec() {
    let current = vec![
        "spec-a".to_string(),
        "spec-b".to_string(),
        "spec-c".to_string(),
    ];
    let order = vec![
        "spec-a".to_string(),
        "spec-b".to_string(),
        "ghost".to_string(),
    ];

    let error = validate_queue_reorder(&current, &order).unwrap_err();
    assert!(error.contains("no spec 'ghost'"), "{error}");
}

/// CM44 FR1/FR2/FR6: setting a member's group reorders the queue and
/// returns the resulting order with groups.
#[tokio::test]
async fn queue_set_group_tool_returns_reordered_queue() {
    let (_dir, db, handler) = queue_test_handler();
    for id in ["a", "b", "c"] {
        db.insert_graph_spec(&standalone_spec(id)).unwrap();
    }
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "a", Some("g1")).unwrap();
    db.append_queue_member("queue-1", "b", None).unwrap();
    db.append_queue_member("queue-1", "c", Some("g1")).unwrap();

    let result = handler
        .do_queue_set_group("queue-1", "b", Some("g1"))
        .await
        .unwrap();

    assert!(!queue_is_err(&result), "{}", result_text(&result));
    let value: serde_json::Value = serde_json::from_str(&queue_raw_text(&result)).unwrap();
    let members = value["queue"]["members"].as_array().unwrap();
    let ids: Vec<&str> = members.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["a", "c", "b"]);
    let b = &members[2];
    assert_eq!(b["group"], serde_json::json!("g1"));
    assert_eq!(b["queue_position"], serde_json::json!(3));
}

/// CM44 FR3: a reorder that would split a group's pending members is
/// refused, names the group and the offending positions, and changes
/// nothing.
#[tokio::test]
async fn queue_reorder_refuses_a_split_group() {
    let (_dir, db, handler) = queue_test_handler();
    for id in ["a", "b", "c"] {
        db.insert_graph_spec(&standalone_spec(id)).unwrap();
    }
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "a", Some("g")).unwrap();
    db.append_queue_member("queue-1", "b", Some("g")).unwrap();
    db.append_queue_member("queue-1", "c", None).unwrap();

    let result = handler
        .do_queue_reorder(
            "queue-1",
            &["a".to_string(), "c".to_string(), "b".to_string()],
        )
        .await
        .unwrap();

    assert!(queue_is_err(&result), "{}", result_text(&result));
    let msg = queue_raw_text(&result);
    assert!(msg.contains("group 'g'"), "{msg}");
    assert!(msg.contains("positions 1, 3"), "{msg}");
    assert!(msg.contains("Nothing was changed"), "{msg}");
    assert_eq!(
        db.list_queue_member_spec_ids("queue-1").unwrap(),
        vec!["a", "b", "c"]
    );
}

/// CM44 FR2/FR6: adding a spec to an existing group places it right after
/// the group's last pending member and returns the resulting order.
#[tokio::test]
async fn queue_add_spec_tool_returns_order_with_groups() {
    let (_dir, db, handler) = queue_test_handler();
    for id in ["a", "b", "c", "x"] {
        db.insert_graph_spec(&standalone_spec(id)).unwrap();
    }
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "a", Some("g")).unwrap();
    db.append_queue_member("queue-1", "b", Some("g")).unwrap();
    db.append_queue_member("queue-1", "c", None).unwrap();

    let result = handler
        .do_queue_add_spec("queue-1", "x", Some("g"))
        .await
        .unwrap();

    assert!(!queue_is_err(&result), "{}", result_text(&result));
    let value: serde_json::Value = serde_json::from_str(&queue_raw_text(&result)).unwrap();
    let members = value["queue"]["members"].as_array().unwrap();
    let ids: Vec<&str> = members.iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["a", "b", "x", "c"]);
    let x = &members[2];
    assert_eq!(x["group"], serde_json::json!("g"));
    assert_eq!(x["queue_position"], serde_json::json!(3));
}

#[test]
fn resolve_reported_run_accepts_matching_active_run() {
    let (_dir, db) = reported_run_fixture();
    db.insert_graph_run(&graph_run_row(
        "run-1",
        "graph-1",
        "spec-a",
        GraphRunStatus::Running,
    ))
    .unwrap();

    let report = resolve_reported_run(&db, Some("run-1"), Some("node-1"), None)
        .unwrap()
        .expect("a running run for the claimed node_id must be accepted");
    assert_eq!(report.run.id, "run-1");
    assert!(report.correction.is_none());
}

#[test]
fn resolve_reported_run_rejects_unknown_run_id() {
    let (_dir, db) = reported_run_fixture();

    let result = resolve_reported_run(&db, Some("no-such-run"), Some("node-1"), None).unwrap();
    let error = result.expect_err("an unknown run_id must be rejected");
    assert!(error.is_error.unwrap_or(false));
}

#[test]
fn resolve_reported_run_rejects_node_id_mismatch() {
    let (_dir, db) = reported_run_fixture();
    db.insert_graph_run(&graph_run_row(
        "run-1",
        "graph-1",
        "spec-a",
        GraphRunStatus::Running,
    ))
    .unwrap();

    // "run-1" really belongs to "node-1" (see graph_run_row) — a report
    // claiming a different node_id for the same run_id is malformed.
    let result = resolve_reported_run(&db, Some("run-1"), Some("some-other-node"), None).unwrap();
    let error = result.expect_err("a node_id that doesn't match the run must be rejected");
    assert!(error.is_error.unwrap_or(false));
}

/// The core B12 regression case: a node run that was already finalized
/// (timed out, killed on pause/reset, or superseded by a retry — any of
/// which flips its status away from `running`) must reject a late report
/// naming its exact `run_id`, rather than that report silently landing
/// on whatever's now active for the same `node_id`.
#[test]
fn resolve_reported_run_rejects_already_finalized_run() {
    let (_dir, db) = reported_run_fixture();
    // The stale run: already finalized (e.g. by the timeout/pause kill
    // path), simulating the orphaned agent's late self-report arriving
    // after the engine gave up on it.
    db.insert_graph_run(&graph_run_row(
        "run-stale",
        "graph-1",
        "spec-a",
        GraphRunStatus::Fail,
    ))
    .unwrap();
    // A newer attempt at the SAME node is now the genuinely active run —
    // exactly the run a naive node_id-only lookup would have
    // misattributed the stale report to.
    db.insert_graph_run(&graph_run_row(
        "run-current",
        "graph-1",
        "spec-a",
        GraphRunStatus::Running,
    ))
    .unwrap();

    let result = resolve_reported_run(&db, Some("run-stale"), Some("node-1"), None).unwrap();
    let error = result.expect_err("a finalized run must reject a late report");
    assert!(error.is_error.unwrap_or(false));

    // And the genuinely active run must be left completely untouched.
    let current = db.get_graph_run("run-current").unwrap().unwrap();
    assert_eq!(current.status, GraphRunStatus::Running);
}

#[test]
fn queue_not_consumed_allows_start_when_every_member_is_pending() {
    let (_dir, db) = queue_test_db();
    db.insert_graph_spec(&standalone_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();

    assert!(validate_queue_not_consumed(&db, "queue-1", "graph-requesting").is_ok());
}

#[test]
fn queue_not_consumed_blocks_when_a_member_runs_under_another_graph() {
    let (_dir, db) = queue_test_db();
    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    insert_test_graph(&db, "graph-other");
    insert_test_node(&db, "node-1", "spec-a");
    db.insert_graph_run(&graph_run_row(
        "run-1",
        "graph-other",
        "spec-a",
        GraphRunStatus::Running,
    ))
    .unwrap();

    let error = validate_queue_not_consumed(&db, "queue-1", "graph-requesting").unwrap_err();

    assert!(error.contains("spec-a"), "{error}");
    assert!(error.contains("graph-other"), "{error}");
}

#[test]
fn queue_not_consumed_allows_the_owning_graph_to_resume_its_own_running_spec() {
    // A paused queue run's active spec stays `running` between node
    // executions. Resuming the SAME graph against the SAME queue must not
    // be mistaken for a conflicting run.
    let (_dir, db) = queue_test_db();
    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    insert_test_graph(&db, "graph-owner");
    insert_test_node(&db, "node-1", "spec-a");
    db.insert_graph_run(&graph_run_row(
        "run-1",
        "graph-owner",
        "spec-a",
        GraphRunStatus::Pass,
    ))
    .unwrap();

    assert!(validate_queue_not_consumed(&db, "queue-1", "graph-owner").is_ok());
}

/// B18 (Requirement 2): `skip_next_spec` on a queue-driven paused graph
/// must find its in-flight member through the graph's persisted
/// `active_run_queue_id` — the member's own `graph_id` column stays `None`
/// (queue membership never binds it), so `list_graph_specs(graph_id)` alone
/// can't see it. Before this fix `handle_skip_next_spec` always errored
/// "No running spec found" for a queue-driven pause.
#[test]
fn skip_next_spec_finds_and_skips_the_running_queue_member() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
        .unwrap();

    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();

    handle_skip_next_spec(&db, "graph-owner").unwrap();

    let spec_a = db.get_graph_spec("spec-a").unwrap().unwrap();
    let spec_b = db.get_graph_spec("spec-b").unwrap().unwrap();
    assert_eq!(spec_a.status, GraphSpecStatus::Skipped);
    assert_eq!(spec_b.status, GraphSpecStatus::Pending);
}

#[test]
fn skip_next_spec_prefers_the_graph_bound_spec_over_queue_context() {
    // A graph with its own bound `running` spec must use that, even if a
    // stale `active_run_queue_id` is still sitting on the graph from an
    // earlier, unrelated queue run.
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
        .unwrap();

    let mut bound = running_spec("spec-bound");
    bound.graph_id = Some("graph-owner".to_string());
    db.insert_graph_spec(&bound).unwrap();
    db.insert_graph_spec(&running_spec("spec-queue")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-queue", None)
        .unwrap();

    handle_skip_next_spec(&db, "graph-owner").unwrap();

    let bound_after = db.get_graph_spec("spec-bound").unwrap().unwrap();
    let queue_after = db.get_graph_spec("spec-queue").unwrap().unwrap();
    assert_eq!(bound_after.status, GraphSpecStatus::Skipped);
    assert_eq!(queue_after.status, GraphSpecStatus::Running);
}

#[test]
fn skip_next_spec_errors_when_no_spec_is_running_anywhere() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");

    let error = handle_skip_next_spec(&db, "graph-owner").unwrap_err();
    assert!(error.message.contains("No running spec found"));
}

/// CM42 (FR3): `skip_next_spec` must find a spec left `Interrupted` by a
/// no-verdict ensemble block — not only a `running` one — so the skip verb
/// keeps working on the same paused graph `retry_current_node` can retry.
#[test]
fn skip_next_spec_finds_interrupted_spec() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    let mut bound = interrupted_spec("spec-bound");
    bound.graph_id = Some("graph-owner".to_string());
    db.insert_graph_spec(&bound).unwrap();

    handle_skip_next_spec(&db, "graph-owner").unwrap();

    let after = db.get_graph_spec("spec-bound").unwrap().unwrap();
    assert_eq!(after.status, GraphSpecStatus::Skipped);
}

/// CM42 (FR3): the queue-side half of the same lookup —
/// `queue_running_spec` is `handle_skip_next_spec`'s fallback for a
/// queue-driven pause and must see an `Interrupted` in-flight member too.
#[test]
fn queue_running_spec_finds_interrupted_spec() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
        .unwrap();
    db.insert_graph_spec(&interrupted_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();

    let found = queue_running_spec(&db, "graph-owner").unwrap();

    assert_eq!(
        found.map(|spec| spec.id),
        Some("spec-a".to_string()),
        "the interrupted in-flight member is the current one"
    );
}

/// B35: `retry_current_node` must error when no spec is running in
/// either the graph's bound specs or its queue — same validation shape as
/// `skip_next_spec`.
#[test]
fn retry_current_node_errors_when_no_running_spec() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    let error = handle_retry_current_node(&db, "graph-owner").unwrap_err();
    assert!(error.message.contains("No running spec found"));
}

/// B35: `retry_current_node` must find the running spec through the
/// graph's persisted `active_run_queue_id` (queue member has `graph_id: None`).
#[test]
fn retry_current_node_finds_running_queue_member() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
        .unwrap();

    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();

    // Should succeed without error — a running spec exists.
    assert!(handle_retry_current_node(&db, "graph-owner").is_ok());
}

/// CB50 FR1: `retry_current_node` must accept a bound spec that is
/// `interrupted` — the status every interrupt-pause and every boot
/// reconcile leaves behind — not just `running`.
#[test]
fn retry_current_node_accepts_interrupted_bound_spec() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    let mut bound = interrupted_spec("spec-bound");
    bound.graph_id = Some("graph-owner".to_string());
    db.insert_graph_spec(&bound).unwrap();

    assert!(handle_retry_current_node(&db, "graph-owner").is_ok());
}

/// CB50 FR1: same, but the interrupted spec is a queue member (found
/// through `active_run_queue_id`), not a bound spec — the queue-driven
/// half of the same bug (B18's own scenario).
#[test]
fn retry_current_node_finds_interrupted_queue_member() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    db.set_graph_active_run_queue("graph-owner", Some("queue-1"))
        .unwrap();

    db.insert_graph_spec(&interrupted_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();
    db.append_queue_member("queue-1", "spec-b", None).unwrap();

    assert!(handle_retry_current_node(&db, "graph-owner").is_ok());
}

/// CB50 FR5: when nothing is resumable, the error names the spec that
/// IS current, its status, and the verb that applies to it.
#[test]
fn retry_current_node_error_names_spec_status_and_applicable_verb() {
    let (_dir, db) = queue_test_db();
    insert_test_graph(&db, "graph-owner");
    let mut failed = standalone_spec("spec-failed");
    failed.status = GraphSpecStatus::Failed;
    failed.graph_id = Some("graph-owner".to_string());
    db.insert_graph_spec(&failed).unwrap();

    let error = handle_retry_current_node(&db, "graph-owner").unwrap_err();
    assert!(error.message.contains("spec-failed"), "{}", error.message);
    assert!(error.message.contains("failed"), "{}", error.message);
    assert!(error.message.contains("graph_reset"), "{}", error.message);
    assert!(!error.message.contains("loop_"), "{}", error.message);
}

#[test]
fn queue_reorder_rejects_duplicate_spec() {
    let current = vec![
        "spec-a".to_string(),
        "spec-b".to_string(),
        "spec-c".to_string(),
    ];
    let order = vec![
        "spec-a".to_string(),
        "spec-a".to_string(),
        "spec-b".to_string(),
    ];

    let error = validate_queue_reorder(&current, &order).unwrap_err();
    assert!(error.contains("more than once"), "{error}");
}

#[test]
fn queue_reorder_locking_refuses_ordering_that_moves_a_running_member() {
    // R6: the currently running spec is immutable in the queue's order.
    // Swapping it with a pending member must be refused, even though the
    // result is still a valid total permutation.
    let (_dir, db) = queue_test_db();
    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-c")).unwrap();
    insert_queue(&db, "queue-1");
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.append_queue_member("queue-1", id, None).unwrap();
    }
    let current = db.list_queue_member_spec_ids("queue-1").unwrap();

    // Moves spec-a (running) from position 0 to position 1.
    let order = vec![
        "spec-b".to_string(),
        "spec-a".to_string(),
        "spec-c".to_string(),
    ];
    assert!(validate_queue_reorder(&current, &order).is_ok());

    let error = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
    assert!(error.contains("spec-a"), "{error}");
    assert!(error.contains("running"), "{error}");
}

#[test]
fn queue_reorder_locking_refuses_ordering_that_moves_a_completed_member() {
    let (_dir, db) = queue_test_db();
    let mut done = standalone_spec("spec-a");
    done.status = GraphSpecStatus::Completed;
    db.insert_graph_spec(&done).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    insert_queue(&db, "queue-1");
    for id in ["spec-a", "spec-b"] {
        db.append_queue_member("queue-1", id, None).unwrap();
    }
    let current = db.list_queue_member_spec_ids("queue-1").unwrap();

    let order = vec!["spec-b".to_string(), "spec-a".to_string()];
    let error = validate_queue_reorder_locking(&db, &current, &order).unwrap_err();
    assert!(error.contains("spec-a"), "{error}");
    assert!(error.contains("completed"), "{error}");
}

#[test]
fn queue_reorder_locking_allows_permuting_pending_members_only() {
    let (_dir, db) = queue_test_db();
    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-b")).unwrap();
    db.insert_graph_spec(&standalone_spec("spec-c")).unwrap();
    insert_queue(&db, "queue-1");
    for id in ["spec-a", "spec-b", "spec-c"] {
        db.append_queue_member("queue-1", id, None).unwrap();
    }
    let current = db.list_queue_member_spec_ids("queue-1").unwrap();

    // spec-a (running) stays at position 0; only the pending tail moves.
    let order = vec![
        "spec-a".to_string(),
        "spec-c".to_string(),
        "spec-b".to_string(),
    ];
    assert!(validate_queue_reorder_locking(&db, &current, &order).is_ok());
}

#[test]
fn queue_remove_spec_refuses_the_currently_running_spec() {
    let (_dir, db) = queue_test_db();
    db.insert_graph_spec(&running_spec("spec-a")).unwrap();
    insert_queue(&db, "queue-1");
    db.append_queue_member("queue-1", "spec-a", None).unwrap();

    let error = validate_queue_member_removable(&db, "queue-1", "spec-a").unwrap_err();
    assert!(error.contains("spec-a"), "{error}");
    assert!(error.contains("running"), "{error}");

    // Once it's no longer running, removal is allowed again.
    db.update_graph_spec_status(
        "spec-a",
        GraphSpecStatus::Completed,
        None,
        Some(chrono::Utc::now()),
    )
    .unwrap();
    assert!(validate_queue_member_removable(&db, "queue-1", "spec-a").is_ok());
}

#[test]
fn queue_add_spec_rejects_nonexistent_spec() {
    let (_dir, db) = queue_test_db();
    insert_queue(&db, "queue-1");

    let error = validate_spec_exists(&db, "ghost-spec").unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

#[test]
fn queue_operations_reject_nonexistent_queue() {
    let (_dir, db) = queue_test_db();

    let error = validate_queue_exists(&db, "does-not-exist").unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

#[test]
fn resolve_graph_target_requires_exactly_one_of_spec_or_graph() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let neither = resolve_graph_target(&db, None, None).unwrap_err();
    assert!(neither.contains("exactly one"), "{neither}");

    let both = resolve_graph_target(&db, Some("spec-x"), Some("graph-x")).unwrap_err();
    assert!(both.contains("exactly one"), "{both}");
}

#[test]
fn resolve_graph_target_rejects_unknown_graph_id() {
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();

    let error = resolve_graph_target(&db, None, Some("does-not-exist")).unwrap_err();
    assert!(error.contains("not found"), "{error}");
}

#[test]
fn graph_get_response_includes_graph_level_graph_alongside_specs() {
    // R1: `graph_get` (via `graph_details_json`) must surface the
    // top-level graph, not just each spec's own nodes/edges.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-with-graph".to_string();
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
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "graph-node".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let details = db.get_graph_details(&graph_id).unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();

    assert_eq!(json["graph"]["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(json["graph"]["nodes"][0]["id"], "graph-node");
    assert_eq!(json["graph"]["nodes"][0]["graph_id"], graph_id);
    assert!(json["specs"].as_array().unwrap().is_empty());
}

#[test]
fn graph_get_reports_queue_spec_not_placeholder() {
    // CB30-1: while a graph executes a queue, the graph level names the
    // queue spec in progress — its id and name — not the placeholder.
    let (_dir, db) = queue_driven_test_db();

    let details = db.get_graph_details("graph-q").unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();
    let specs = json["specs"].as_array().unwrap();
    assert!(
        !specs.is_empty(),
        "queue-driven graph_get must name the running queue spec: {json}"
    );
    assert!(
        specs
            .iter()
            .all(|s| !s["name"].as_str().unwrap_or("").trim().is_empty()),
        "no surface lists a spec whose name is blank: {json}"
    );
    let running = specs
        .iter()
        .find(|s| s["id"] == "spec-a")
        .expect("running queue spec must appear in graph_get specs");
    assert_eq!(running["name"], "CM4 — queue work");
    assert_eq!(json["queue"]["id"], "q1");
    assert_eq!(json["queue"]["name"], "q1-name");

    // The shared helper agrees: it is the same answer every surface
    // must give.
    let lp = db.get_graph("graph-q").unwrap().unwrap();
    match active_graph_spec_context(&db, &lp).unwrap() {
        ActiveGraphSpec::QueueMember {
            spec,
            queue_id,
            queue_name,
        } => {
            assert_eq!(spec.id, "spec-a");
            assert_eq!(spec.name, "CM4 — queue work");
            assert_eq!(queue_id, "q1");
            assert_eq!(queue_name, "q1-name");
        }
        _ => panic!("queue-driven graph must resolve to QueueMember"),
    }
}

#[test]
fn graph_get_idea_run_shows_mode_idea_not_placeholder() {
    // CB30-4(idea): a graph run from a loose idea reports that it is
    // running an idea, and does not present the placeholder as a spec.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph(&cb30_test_graph("graph-idea", GraphStatus::Running))
        .unwrap();
    db.insert_graph_spec(&placeholder_spec(
        "ph-idea",
        "graph-idea",
        GraphSpecStatus::Running,
    ))
    .unwrap();

    let details = db.get_graph_details("graph-idea").unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();
    assert!(
        json["specs"].as_array().unwrap().is_empty(),
        "idea-driven graph_get must not list the placeholder: {json}"
    );
    assert_eq!(json["mode"], "idea");
    assert!(
        json.get("queue").is_none(),
        "idea-driven graph_get must not carry a queue: {json}"
    );

    let lp = db.get_graph("graph-idea").unwrap().unwrap();
    assert!(
        matches!(
            active_graph_spec_context(&db, &lp).unwrap(),
            ActiveGraphSpec::Idea { .. }
        ),
        "placeholder-only graph must resolve to Idea"
    );
    let summary = build_graph_summary_json(&db, &lp).unwrap();
    assert!(
        summary["current_spec"].is_null(),
        "idea-driven summary must not name the placeholder: {summary}"
    );
}

#[test]
fn graph_get_bound_specs_unchanged() {
    // CB30-3: a graph running its own bound specs still reports those,
    // unchanged — no queue/mode annotation.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    db.insert_graph(&cb30_test_graph("graph-bound", GraphStatus::Running))
        .unwrap();
    db.insert_graph_spec(&named_spec(
        "spec-1",
        Some("graph-bound"),
        "First work",
        0,
        GraphSpecStatus::Running,
    ))
    .unwrap();
    db.insert_graph_spec(&named_spec(
        "spec-2",
        Some("graph-bound"),
        "Second work",
        1,
        GraphSpecStatus::Pending,
    ))
    .unwrap();

    let details = db.get_graph_details("graph-bound").unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();
    let names: Vec<&str> = json["specs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["First work", "Second work"]);
    assert!(json.get("queue").is_none());
    assert!(json.get("mode").is_none());

    let lp = db.get_graph("graph-bound").unwrap().unwrap();
    match active_graph_spec_context(&db, &lp).unwrap() {
        ActiveGraphSpec::Bound(spec) => assert_eq!(spec.name, "First work"),
        _ => panic!("bound-spec graph must resolve to Bound"),
    }
    let summary = build_graph_summary_json(&db, &lp).unwrap();
    assert_eq!(summary["current_spec"], "First work");
    assert!(summary.get("queue").is_none());
}

#[test]
fn graph_list_summary_shows_queue_spec() {
    // CB30-5: `graph list` names the running queue spec, never blank,
    // and carries the queue link.
    let (_dir, db) = queue_driven_test_db();

    let lp = db.get_graph("graph-q").unwrap().unwrap();
    let summary = build_graph_summary_json(&db, &lp).unwrap();
    assert_eq!(summary["current_spec"], "CM4 — queue work");
    assert_eq!(summary["queue"]["id"], "q1");
    assert_eq!(summary["queue"]["name"], "q1-name");
}

#[test]
fn completed_queue_graph_is_not_idea() {
    // CB30/B31: a completed queue-driven graph keeps `active_run_queue_id`
    // set (B31 — last-run context). The shared helper must NOT report it
    // as `Idea` just because only the placeholder remains in bound specs.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let mut lp = cb30_test_graph("graph-done", GraphStatus::Completed);
    lp.active_run_queue_id = Some("q-done".to_string());
    db.insert_graph(&lp).unwrap();
    insert_queue(&db, "q-done");
    db.insert_graph_spec(&placeholder_spec(
        "ph-done",
        "graph-done",
        GraphSpecStatus::Completed,
    ))
    .unwrap();
    let done = named_spec(
        "spec-done",
        None,
        "CM4 — done",
        0,
        GraphSpecStatus::Completed,
    );
    db.insert_graph_spec(&done).unwrap();
    db.append_queue_member("q-done", "spec-done", None).unwrap();

    match active_graph_spec_context(&db, &lp).unwrap() {
        ActiveGraphSpec::None => {}
        other => panic!("completed queue graph must be None, got {other:?}"),
    }
    let json =
        graph_details_json(&db, &db.get_graph_details("graph-done").unwrap().unwrap()).unwrap();
    assert!(
        json.get("mode").is_none(),
        "completed queue graph must not say 'idea': {json}"
    );
    assert_eq!(json["queue"]["id"], "q-done");
    assert!(
        json["specs"].as_array().unwrap().is_empty(),
        "no spec to show once the queue is drained: {json}"
    );
    let summary = build_graph_summary_json(&db, &lp).unwrap();
    assert!(summary["current_spec"].is_null());
    assert_eq!(summary["queue"]["id"], "q-done");
}

#[test]
fn graph_get_response_surfaces_pinned_skills_on_a_node() {
    // S2: an agent node's pinned `skills` array must be visible to
    // agents inspecting/building graphs via graph_get, not just used
    // internally at spawn time.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-with-pinned-skills".to_string();
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
        status: GraphStatus::Draft,
        trigger: None,
        created_at: chrono::Utc::now(),
        started_at: None,
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();
    db.insert_graph_node(&GraphNode {
        id: "pinned-node".to_string(),
        spec_id: None,
        graph_id: Some(graph_id.clone()),
        name: "implement".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({
            "platform": "claude",
            "skills": ["coder", "rust-idiomatic-patterns"]
        }),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();

    let details = db.get_graph_details(&graph_id).unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();

    assert_eq!(
        json["graph"]["nodes"][0]["config"]["skills"],
        serde_json::json!(["coder", "rust-idiomatic-patterns"])
    );
}

#[test]
fn graph_get_response_exposes_autorun_at_so_agents_never_need_sql() {
    // B14: a failed graph's pending autorun schedule must be visible via
    // `graph_get` — before this, an agent had to query
    // `background_agents.db` directly to learn whether a recovery wait
    // was already scheduled.
    let dir = tempdir().unwrap();
    let db = Database::new(&dir.path().join("test.db")).unwrap();
    let graph_id = "graph-with-autorun".to_string();
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
        completed_at: None,
        autorun_at: None,
        auto_continue_at: None,
        auto_continue_action: None,
        active_run_queue_id: None,
        hooks: std::collections::BTreeMap::new(),
    })
    .unwrap();

    // Unset: field must be present and null, not just absent.
    let details = db.get_graph_details(&graph_id).unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();
    assert!(json["autorun_at"].is_null());

    // `autorun_at` round-trips through an INTEGER (unix seconds) column,
    // so compare against a second-precision timestamp.
    let at = chrono::DateTime::from_timestamp(
        (chrono::Utc::now() + chrono::Duration::hours(2)).timestamp(),
        0,
    )
    .unwrap();
    db.schedule_graph_autorun(&graph_id, at).unwrap();

    let details = db.get_graph_details(&graph_id).unwrap().unwrap();
    let json = graph_details_json(&db, &details).unwrap();
    assert_eq!(json["autorun_at"].as_str().unwrap(), at.to_rfc3339());
}

/// B41: a `failed` graph with a pending autorun is exactly the state a
/// scheduled wake-up needs to be cancellable from — cancelling must
/// succeed and report the time that was cleared.
#[tokio::test]
async fn graph_schedule_autorun_cancels_pending_schedule_on_failed_graph() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-failed-autorun";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Failed,
    ))
    .unwrap();
    let scheduled_at = chrono::Utc::now() + chrono::Duration::hours(1);
    db.schedule_graph_autorun(graph_id, scheduled_at).unwrap();

    let result = handler
        .graph_schedule_autorun(Parameters(GraphScheduleAutorunParams {
            graph_id: graph_id.to_string(),
            at: None,
            quota_reset_message: None,
        }))
        .await
        .unwrap();

    let text = result_text(&result);
    assert!(text.contains("cancelled"), "{text}");
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert!(lp.autorun_at.is_none(), "cancel must clear autorun_at");
    assert_eq!(
        lp.status,
        GraphStatus::Failed,
        "cancelling must not touch graph status"
    );
}

/// B41: cancelling must also work on a `completed` graph — the other
/// status a graph with a pending autorun can hold.
#[tokio::test]
async fn graph_schedule_autorun_cancels_pending_schedule_on_completed_graph() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-completed-autorun";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Completed,
    ))
    .unwrap();
    let scheduled_at = chrono::Utc::now() + chrono::Duration::hours(1);
    db.schedule_graph_autorun(graph_id, scheduled_at).unwrap();

    let result = handler
        .graph_schedule_autorun(Parameters(GraphScheduleAutorunParams {
            graph_id: graph_id.to_string(),
            at: None,
            quota_reset_message: None,
        }))
        .await
        .unwrap();

    assert!(result_text(&result).contains("cancelled"));
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert!(lp.autorun_at.is_none());
}

/// B41: cancelling when nothing is scheduled must succeed and say so —
/// it is not an error, since the caller's intent (no pending autorun) is
/// already satisfied.
#[tokio::test]
async fn graph_schedule_autorun_cancel_with_nothing_scheduled_is_not_an_error() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-no-autorun";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Failed,
    ))
    .unwrap();

    let result = handler
        .graph_schedule_autorun(Parameters(GraphScheduleAutorunParams {
            graph_id: graph_id.to_string(),
            at: None,
            quota_reset_message: None,
        }))
        .await
        .unwrap();

    assert!(!result.is_error.unwrap_or(false), "{result:?}");
    let text = result_text(&result);
    assert!(text.contains("no pending autorun"), "{text}");
}

// ── graph_schedule_continue: deferred resume of a paused graph ───────

/// Setting a schedule on a paused graph must persist `auto_continue_at`
/// and default the action to `retry_current_node` when omitted.
#[tokio::test]
async fn graph_schedule_continue_sets_pending_schedule_with_default_action() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-paused-continue";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Paused,
    ))
    .unwrap();
    let scheduled_at = chrono::DateTime::from_timestamp(
        (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp(),
        0,
    )
    .unwrap();

    let result = handler
        .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
            graph_id: graph_id.to_string(),
            at: Some(scheduled_at.to_rfc3339()),
            action: None,
        }))
        .await
        .unwrap();

    assert!(!result.is_error.unwrap_or(false), "{result:?}");
    let text = result_text(&result);
    assert!(text.contains("retry_current_node"), "{text}");
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert_eq!(lp.auto_continue_at, Some(scheduled_at));
    assert_eq!(
        lp.auto_continue_action.as_deref(),
        Some("retry_current_node")
    );
}

/// An explicit `skip_next_spec` action must be persisted as given.
#[tokio::test]
async fn graph_schedule_continue_persists_explicit_skip_action() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-paused-continue-skip";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Paused,
    ))
    .unwrap();

    handler
        .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
            graph_id: graph_id.to_string(),
            at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            action: Some("skip_next_spec".to_string()),
        }))
        .await
        .unwrap();

    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert_eq!(lp.auto_continue_action.as_deref(), Some("skip_next_spec"));
}

/// An invalid action must be rejected without touching the schedule.
#[tokio::test]
async fn graph_schedule_continue_rejects_invalid_action() {
    use crate::daemon::params_extract::Parameters;

    let (dir, db, handler) = queue_test_handler();
    let graph_id = "graph-paused-continue-bad-action";
    db.insert_graph(&autorun_test_graph(
        graph_id,
        &dir.path().to_string_lossy(),
        GraphStatus::Paused,
    ))
    .unwrap();

    let result = handler
        .graph_schedule_continue(Parameters(GraphScheduleContinueParams {
            graph_id: graph_id.to_string(),
            at: Some((chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339()),
            action: Some("not_a_real_action".to_string()),
        }))
        .await
        .unwrap();

    assert!(result.is_error.unwrap_or(false), "{result:?}");
    let lp = db.get_graph(graph_id).unwrap().unwrap();
    assert!(
        lp.auto_continue_at.is_none(),
        "an invalid action must not schedule anything"
    );
}
