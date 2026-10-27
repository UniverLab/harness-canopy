//! Part 12 of `endpoint_tests` (split from `src/daemon/handler.rs`, pure move).
use super::*;

/// CM49 req 7, end to end through the MCP surface: export a crew-bound
/// ensemble as `crew` + no members; import restores the binding when the
/// crew exists locally, and still succeeds (reporting `missing_crews`)
/// when it does not.
#[tokio::test]
async fn graph_export_import_round_trip_with_crew() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    handler
        .crew_create(Parameters(CrewCreateParams {
            name: "eager".to_string(),
            description: None,
            members: Some(vec![crew_json_member("a"), crew_json_member("b")]),
            from_ensemble: None,
        }))
        .await
        .unwrap();
    let (graph_id, _ens) = build_crew_ensemble(&handler, &workdir, "Crew G").await;

    let exported = handler
        .graph_export(Parameters(GraphExportParams {
            graph_id: graph_id.clone(),
        }))
        .await
        .unwrap();
    assert!(!is_err(&exported), "{}", text(&exported));
    let doc: serde_json::Value = serde_json::from_str(&raw_text(&exported)).unwrap();
    let ensemble = &doc["ensembles"][0];
    assert_eq!(ensemble["crew"], "eager");
    assert_eq!(
        ensemble["members"].as_array().unwrap().len(),
        0,
        "a bound ensemble exports the crew name, not members"
    );

    // Import WITH the local crew: bound, no missing_crews.
    let imported = handler
        .graph_import(Parameters(GraphImportParams {
            document: doc.clone(),
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&imported), "{}", text(&imported));
    let imported_json: serde_json::Value = serde_json::from_str(&raw_text(&imported)).unwrap();
    assert_eq!(
        imported_json["missing_crews"].as_array().unwrap().len(),
        0,
        "the crew exists locally"
    );
    let new_graph_id = imported_json["graph_id"].as_str().unwrap().to_string();
    let ensembles = db.list_ensembles_for_graph(&new_graph_id).unwrap();
    assert_eq!(ensembles[0].members.len(), 2, "members materialized");
    assert_eq!(ensembles[0].ensemble.crew_name.as_deref(), Some("eager"));

    // Import WITHOUT a local crew of that name: still succeeds, reported.
    let mut doc_ghost = doc.clone();
    doc_ghost["ensembles"][0]["crew"] = serde_json::json!("ghost");
    let imported2 = handler
        .graph_import(Parameters(GraphImportParams {
            document: doc_ghost,
            workdir: workdir.clone(),
            name: None,
        }))
        .await
        .unwrap();
    assert!(!is_err(&imported2), "{}", text(&imported2));
    let imported2_json: serde_json::Value = serde_json::from_str(&raw_text(&imported2)).unwrap();
    assert_eq!(
        imported2_json["missing_crews"],
        serde_json::json!(["ghost"])
    );
}

/// CM49 req 4: `graph_update_ensemble` binds an ensemble to a crew by
/// name and `crew: null` unbinds it, keeping the current rows literal.
#[tokio::test]
async fn graph_update_ensemble_binds_then_unbinds_a_crew() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    handler
        .crew_create(Parameters(CrewCreateParams {
            name: "eager".to_string(),
            description: None,
            members: Some(vec![crew_json_member("a"), crew_json_member("b")]),
            from_ensemble: None,
        }))
        .await
        .unwrap();
    let (_graph_id, ensemble_id) = build_literal_ensemble(&handler, &workdir, "Literal G").await;

    // Bind: the literal rows are replaced by the crew's members.
    let bound = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            crew: Some(Some("eager".to_string())),
            ..ensemble_update_params(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(!is_err(&bound), "{}", text(&bound));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert!(details.ensemble.crew_id.is_some(), "bound to the crew");
    assert_eq!(details.ensemble.crew_name.as_deref(), Some("eager"));
    assert_eq!(details.ensemble.crew_version_applied, Some(1));
    assert_eq!(details.members.len(), 2);
    assert!(
        details
            .members
            .iter()
            .all(|m| m.platform == "openrouter" && m.prompt_override.is_none()),
        "the crew's members are materialized with the shared prompt"
    );

    // Unbind with an explicit null: the rows stay, the columns clear.
    let unbound = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            crew: Some(None),
            ..ensemble_update_params(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(!is_err(&unbound), "{}", text(&unbound));
    let details = db.get_ensemble_details(&ensemble_id).unwrap().unwrap();
    assert!(details.ensemble.crew_id.is_none(), "unbound");
    assert!(details.ensemble.crew_name.is_none(), "unbound");
    assert!(details.ensemble.crew_version_applied.is_none(), "unbound");
    assert_eq!(
        details.members.len(),
        2,
        "the member rows stay as literal members"
    );
    assert!(
        details.members.iter().all(|m| m.platform == "openrouter"),
        "the kept rows are the crew-materialized ones"
    );

    // A crew change is its own call: combining it with any other field is
    // refused rather than silently dropping that field.
    let combined = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            crew: Some(Some("eager".to_string())),
            min_pass: Some(1),
            ..ensemble_update_params(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(is_err(&combined), "{}", text(&combined));
    assert!(
        text(&combined).contains("cannot be combined"),
        "{}",
        text(&combined)
    );
    let with_members = handler
        .graph_update_ensemble(Parameters(GraphUpdateEnsembleParams {
            crew: Some(Some("eager".to_string())),
            members: Some(vec![crate::daemon::params::EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }]),
            ..ensemble_update_params(&ensemble_id)
        }))
        .await
        .unwrap();
    assert!(is_err(&with_members), "{}", text(&with_members));
    assert!(
        text(&with_members).contains("cannot be combined"),
        "{}",
        text(&with_members)
    );
}

/// CM49 req 4: `graph_add_ensemble` refuses a crew beside literal members.
#[tokio::test]
async fn graph_add_ensemble_crew_and_members_are_exclusive() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    handler
        .crew_create(Parameters(CrewCreateParams {
            name: "eager".to_string(),
            description: None,
            members: Some(vec![crew_json_member("a"), crew_json_member("b")]),
            from_ensemble: None,
        }))
        .await
        .unwrap();
    let (graph_id, _ens) = build_literal_ensemble(&handler, &workdir, "Exclusive G").await;
    let entry = handler
        .graph_add_node(Parameters(GraphAddNodeParams {
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            name: "entry".to_string(),
            kind: Some("check".to_string()),
            config: Some(check_node_config("true")),
            blueprint: None,
            config_overrides: None,
        }))
        .await
        .unwrap();
    let entry_id = extract_id(&entry, "node_id");

    let refused = handler
        .graph_add_ensemble(Parameters(GraphAddEnsembleParams {
            mutex: None,
            commit_rights: None,
            spec_id: None,
            graph_id: Some(graph_id.clone()),
            kind: None,
            name: "Both".to_string(),
            prompt_template: Some("draft it".to_string()),
            blueprint: None,
            members: Some(vec![crate::daemon::params::EnsembleMemberParams {
                platform: "claude".to_string(),
                model: None,
                prompt_override: None,
                timeout_minutes: None,
                infra_retry_limit: None,
                infra_crash_max_seconds: None,
                infra_backoff_seconds: None,
            }]),
            condition: "always".to_string(),
            from_node: entry_id.clone(),
            on_pass_to: entry_id,
            on_fail_to: None,
            min_pass: None,
            timeout_minutes: None,
            infra_retry_limit: None,
            infra_crash_max_seconds: None,
            infra_backoff_seconds: None,
            straggler_timeout_minutes: None,
            quorum_grace_minutes: None,
            crew: Some("eager".to_string()),
        }))
        .await
        .unwrap();
    assert!(is_err(&refused), "{}", text(&refused));
    assert!(
        text(&refused).contains("pass crew or members, never both"),
        "{}",
        text(&refused)
    );
}

/// CM49 req 3: `crew_list` takes no arguments and reports every crew
/// with its version, members and per-kind binding counts.
#[tokio::test]
async fn crew_list_reports_crews_members_and_bindings_without_arguments() {
    let (dir, _db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    handler
        .crew_create(Parameters(CrewCreateParams {
            name: "eager".to_string(),
            description: Some("the implementers".to_string()),
            members: Some(vec![crew_json_member("a"), crew_json_member("b")]),
            from_ensemble: None,
        }))
        .await
        .unwrap();
    let (_graph_id, _ens) = build_crew_ensemble(&handler, &workdir, "Bound G").await;

    let listed = handler.crew_list().await.unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    let body: serde_json::Value = serde_json::from_str(&raw_text(&listed)).unwrap();
    let crews = body.as_array().expect("a JSON array of crews");
    assert_eq!(crews.len(), 1);
    assert_eq!(crews[0]["name"], "eager");
    assert_eq!(crews[0]["description"], "the implementers");
    assert_eq!(crews[0]["version"], 1);
    assert_eq!(crews[0]["members"].as_array().unwrap().len(), 2);
    assert_eq!(
        crews[0]["members"][0]["model"], "a",
        "member order is preserved"
    );
    assert_eq!(
        crews[0]["bindings"],
        serde_json::json!({ "ensembles": 1, "nodes": 0, "hooks": 0 }),
        "the bound ensemble is counted"
    );
}

// ── `session_send_file`: session resolution + partial success (FR2) ─────

/// A file that sniffs as PNG: the 8-byte magic plus tail.
fn shelf_png(path: &std::path::Path) {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.resize(64, 0x42);
    std::fs::write(path, bytes).unwrap();
}

fn shelf_session(db: &std::sync::Arc<Database>, id: &str, name: &str) {
    db.insert_interactive_session(id, name, "claude", "/proj", None, None, "interactive", None)
        .unwrap();
}

async fn send_file(
    handler: &TaskTriggerHandler,
    paths: Vec<String>,
    session_id: Option<String>,
) -> serde_json::Value {
    let result = handler
        .session_send_file(
            Parameters(SessionSendFileParams {
                paths,
                caption: None,
                session_id,
            }),
            OptionalExtension(None),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    serde_json::from_str(&raw_text(&result)).unwrap()
}

/// FR2 case 1 via the env identity transport (the header transport is in
/// part13): an explicit `session_id` wins over the caller's own session.
#[tokio::test]
async fn session_send_file_explicit_session_id_wins() {
    let (dir, db, handler) = endpoint_test_handler();
    shelf_session(&db, "caller", "caller-name");
    shelf_session(&db, "other", "other-name");
    let _guard = AgentIdVar::set("caller");

    let shot = dir.path().join("shot.png");
    shelf_png(&shot);
    let out = send_file(
        &handler,
        vec![shot.to_string_lossy().into_owned()],
        Some("other".to_string()),
    )
    .await;

    assert_eq!(out["items"].as_array().unwrap().len(), 1);
    assert_eq!(out["errors"].as_array().unwrap().len(), 0);
    assert_eq!(out["items"][0]["mime"], "image/png");
    let other = handler.shelf.list(Some("other"));
    assert_eq!(other.len(), 1, "the file lands on the named shelf");
    assert_eq!(other[0].from_label, "other-name");
    assert!(handler.shelf.list(Some("caller")).is_empty());
    assert!(handler.shelf.list(None).is_empty());
}

/// FR2 case 2 via the env identity transport (the header transport is in
/// part13): no explicit id — the caller's identity maps to its session.
#[tokio::test]
async fn session_send_file_resolves_caller_session_from_identity() {
    let (dir, db, handler) = endpoint_test_handler();
    shelf_session(&db, "me", "me-name");
    let _guard = AgentIdVar::set("me");

    let shot = dir.path().join("shot.png");
    shelf_png(&shot);
    let out = send_file(&handler, vec![shot.to_string_lossy().into_owned()], None).await;

    assert_eq!(out["items"].as_array().unwrap().len(), 1);
    let mine = handler.shelf.list(Some("me"));
    assert_eq!(mine.len(), 1, "the file lands on the caller's shelf");
    assert_eq!(mine[0].from_label, "me-name");
    assert!(handler.shelf.list(None).is_empty());
}

/// FR2: one bad path never blocks the others — each failure is a coded
/// entry in `errors` alongside the accepted `items`.
#[tokio::test]
async fn session_send_file_partial_success_reports_per_path_errors() {
    let (dir, db, handler) = endpoint_test_handler();
    shelf_session(&db, "me", "me-name");
    let _guard = AgentIdVar::set("me");

    let good = dir.path().join("good.png");
    shelf_png(&good);
    let fake = dir.path().join("fake.png");
    std::fs::write(&fake, b"just some text, not an image").unwrap();
    let missing = dir.path().join("missing.png");
    let paths = vec![
        good.to_string_lossy().into_owned(),
        missing.to_string_lossy().into_owned(),
        dir.path().to_string_lossy().into_owned(),
        fake.to_string_lossy().into_owned(),
    ];
    let out = send_file(&handler, paths, None).await;

    assert_eq!(out["items"].as_array().unwrap().len(), 1);
    assert_eq!(out["items"][0]["name"], "good.png");
    let errors = out["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 3, "three bad paths, three errors: {out}");
    let codes: Vec<&str> = errors
        .iter()
        .map(|e| e["code"].as_str().expect("a code per error"))
        .collect();
    assert_eq!(codes, vec!["not_found", "not_a_file", "unsupported_type"]);
    for error in errors {
        assert!(!error["message"].as_str().unwrap_or("").is_empty());
        assert!(!error["path"].as_str().unwrap_or("").is_empty());
    }
    assert_eq!(handler.shelf.list(Some("me")).len(), 1);
}
