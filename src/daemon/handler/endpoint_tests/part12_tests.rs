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
