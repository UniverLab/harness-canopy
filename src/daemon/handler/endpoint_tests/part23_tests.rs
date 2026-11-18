//! Part 23 of `endpoint_tests`: spec workdir re-coding and conventional
//! title parsing on `spec_edit update`, plus the shared commit-subject
//! validation across the four naming surfaces.
use super::*;
use crate::domain::graphs::{GraphNode, GraphNodeKind, GraphNodeRun, GraphRunStatus};
use crate::domain::spec_type::SpecType;

fn typed_create(
    handler: &TaskTriggerHandler,
    db: &Database,
    name: &str,
    workdir: Option<&str>,
    spec_type: Option<SpecType>,
) -> String {
    let created = run(call_resource_action(
        handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            intent: None,
            name: name.to_string(),
            description: valid_spec_description(),
            workdir: workdir.map(str::to_string),
            spec_type,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    db.list_specs(None, None, false, None)
        .unwrap()
        .into_iter()
        .find(|s| {
            s.name == name
                || name
                    .split_once(": ")
                    .is_some_and(|(_, subject)| s.name == subject.trim())
        })
        .unwrap()
        .id
}

fn run<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn update_workdir(
    handler: &TaskTriggerHandler,
    spec_id: &str,
    workdir: &str,
) -> Result<CallToolResult, rmcp::ErrorData> {
    run(call_resource_action(
        handler,
        "spec_edit",
        "update",
        params_id(SpecUpdateParams {
            spec_id: spec_id.to_string(),
            name: None,
            description: None,
            workdir: Some(Some(workdir.to_string())),
            spec_type: None,
            spec_scope: None,
            intent: None,
        }),
    ))
}

fn update_name(
    handler: &TaskTriggerHandler,
    spec_id: &str,
    name: &str,
) -> Result<CallToolResult, rmcp::ErrorData> {
    run(call_resource_action(
        handler,
        "spec_edit",
        "update",
        params_id(SpecUpdateParams {
            spec_id: spec_id.to_string(),
            name: Some(name.to_string()),
            description: None,
            workdir: None,
            spec_type: None,
            spec_scope: None,
            intent: None,
        }),
    ))
}

/// The first workdir set on an unbound spec re-codes it: a spec created
/// without a workdir drew FIX-1 from the empty-workdir counter, so setting
/// the project workdir takes the next number of that counter (FIX-2, after
/// a sibling already took FIX-1), and the old empty-workdir number is not
/// reused.
#[test]
fn first_workdir_set_recodes_from_new_counter() {
    let (_dir, db, handler) = endpoint_test_handler();
    let project = "/tmp/recode-project";
    let sibling_id = typed_create(&handler, &db, "sibling", Some(project), Some(SpecType::Fix));
    assert_eq!(
        db.get_graph_spec(&sibling_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );

    let spec_id = typed_create(&handler, &db, "ship it", None, Some(SpecType::Fix));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );

    let updated = update_workdir(&handler, &spec_id, project).unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));

    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.spec_code, Some("FIX-2".to_string()));
    assert_eq!(spec.workdir.as_deref(), Some(project));
    // The old empty-workdir counter is untouched: the next empty-workdir
    // spec still takes FIX-2, never the re-coded number again.
    let other_id = typed_create(&handler, &db, "other work", None, Some(SpecType::Fix));
    assert_eq!(
        db.get_graph_spec(&other_id).unwrap().unwrap().spec_code,
        Some("FIX-2".to_string())
    );
}

/// A spec with a run keeps its code on a workdir change.
#[test]
fn spec_with_run_keeps_code_on_workdir_change() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    // Pre-consume the target workdir's counter so a spurious re-code would
    // visibly move this spec from FIX-1 to FIX-2.
    let target = typed_create(
        &handler,
        &db,
        "target first",
        Some("/tmp/other"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&target).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );
    let spec_id = typed_create(&handler, &db, "ship it", None, Some(SpecType::Fix));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );
    db.insert_graph_node(&GraphNode {
        id: "node-1".to_string(),
        spec_id: Some(spec_id.clone()),
        graph_id: None,
        name: "node-1".to_string(),
        kind: GraphNodeKind::Check,
        config: serde_json::json!({"command": "true"}),
        position: 1,
        created_at: chrono::Utc::now(),
    })
    .unwrap();
    db.insert_graph_run(&GraphNodeRun {
        id: "run-1".to_string(),
        graph_id: lp.id,
        spec_id: spec_id.clone(),
        node_id: "node-1".to_string(),
        status: GraphRunStatus::Pass,
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

    let updated = update_workdir(&handler, &spec_id, "/tmp/other").unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string()),
        "a spec that already ran keeps its code"
    );
    let next = typed_create(
        &handler,
        &db,
        "target second",
        Some("/tmp/other"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&next).unwrap().unwrap().spec_code,
        Some("FIX-2".to_string()),
        "the target counter was not consumed for the run-holding spec"
    );
}

/// Setting the same workdir consumes no counter number.
#[test]
fn same_workdir_change_consumes_no_counter() {
    let (_dir, db, handler) = endpoint_test_handler();
    let spec_id = typed_create(
        &handler,
        &db,
        "ship it",
        Some("/tmp/same"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );

    let updated = update_workdir(&handler, &spec_id, "/tmp/same").unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );
    let other_id = typed_create(
        &handler,
        &db,
        "sibling",
        Some("/tmp/same"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&other_id).unwrap().unwrap().spec_code,
        Some("FIX-2".to_string())
    );
}

/// Changing an existing workdir tag re-codes the same way: the spec moves
/// to the new workdir's counter and the old workdir's counter does not
/// hand its number out again.
#[test]
fn workdir_change_recodes_from_new_counter() {
    let (_dir, db, handler) = endpoint_test_handler();
    // Pre-consume the destination counter so the re-code is visible: the
    // moved spec must take FIX-2 there, not keep its old FIX-1.
    let b_first = typed_create(
        &handler,
        &db,
        "b first",
        Some("/tmp/workdir-b"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&b_first).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );
    let spec_id = typed_create(
        &handler,
        &db,
        "ship it",
        Some("/tmp/workdir-a"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );

    let updated = update_workdir(&handler, &spec_id, "/tmp/workdir-b").unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(
        spec.spec_code,
        Some("FIX-2".to_string()),
        "the moved spec takes the next number of the new workdir's counter"
    );
    assert_eq!(spec.workdir.as_deref(), Some("/tmp/workdir-b"));

    // `/tmp/workdir-b` consumed FIX-2 for the moved spec, so the next spec
    // there takes FIX-3; `/tmp/workdir-a` also continues at FIX-2 because
    // its counter never went back.
    let b_id = typed_create(
        &handler,
        &db,
        "b sibling",
        Some("/tmp/workdir-b"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&b_id).unwrap().unwrap().spec_code,
        Some("FIX-3".to_string())
    );
    let a_id = typed_create(
        &handler,
        &db,
        "a sibling",
        Some("/tmp/workdir-a"),
        Some(SpecType::Fix),
    );
    assert_eq!(
        db.get_graph_spec(&a_id).unwrap().unwrap().spec_code,
        Some("FIX-2".to_string())
    );
}

/// A conventional title through `spec_edit update` stores the subject alone
/// — the commit message has no repeated prefix — and the code is kept.
#[test]
fn update_with_conventional_title_stores_subject_and_keeps_code() {
    let (_dir, db, handler) = endpoint_test_handler();
    let spec_id = typed_create(&handler, &db, "open F1", None, Some(SpecType::Fix));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().spec_code,
        Some("FIX-1".to_string())
    );

    let updated = update_name(
        &handler,
        &spec_id,
        "fix(tui): open F1 from inside a session",
    )
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.name, "open F1 from inside a session");
    assert_eq!(spec.spec_scope, Some("tui".to_string()));
    assert_eq!(spec.spec_code, Some("FIX-1".to_string()));
    assert_eq!(
        crate::domain::spec_type::commit_message_opt(
            spec.spec_type,
            spec.spec_scope.as_deref(),
            &spec.name,
        ),
        Some("fix(tui): open F1 from inside a session".to_string())
    );

    // A later title with a different type changes the type but never the code.
    let retyped = update_name(&handler, &spec_id, "feat(tui): add the panel").unwrap();
    assert!(!is_err(&retyped), "{}", text(&retyped));
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.spec_type.map(|t| t.as_str()), Some("feat"));
    assert_eq!(spec.name, "add the panel");
    assert_eq!(
        spec.spec_code,
        Some("FIX-1".to_string()),
        "a type change through the name never re-codes a coded spec"
    );
}

/// `graph_edit update_spec` parses a conventional title the same way:
/// the subject is stored alone and the title's scope lands even when the
/// type is unchanged.
#[test]
fn graph_update_spec_conventional_title_updates_scope() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let attached = run(call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id.clone(),
            name: "old subject".to_string(),
            description: Some(valid_spec_description()),
            position: 1,
            parallelizable: false,
            spec_type: Some(SpecType::Fix),
            spec_scope: Some("tui".to_string()),
        }),
    ))
    .unwrap();
    assert!(!is_err(&attached), "{}", text(&attached));
    let spec_id = db
        .list_graph_specs(&lp.id)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "old subject")
        .unwrap()
        .id;

    let renamed = run(call_graph_action(
        &handler,
        "graph_edit",
        "update_spec",
        params_id(GraphUpdateSpecParams {
            spec_id: spec_id.clone(),
            name: Some("fix(docs): renamed subject".to_string()),
            description: None,
            position: None,
            parallelizable: None,
            spec_type: None,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert!(!is_err(&renamed), "{}", text(&renamed));
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.name, "renamed subject");
    assert_eq!(spec.spec_scope, Some("docs".to_string()));
    assert_eq!(spec.spec_type.map(|t| t.as_str()), Some("fix"));
}

/// A plain name through `spec_edit update` is stored verbatim.
#[test]
fn update_with_plain_name_stores_verbatim() {
    let (_dir, db, handler) = endpoint_test_handler();
    let spec_id = typed_create(&handler, &db, "old name", None, Some(SpecType::Fix));

    let updated = update_name(&handler, &spec_id, "new plain name").unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().name,
        "new plain name"
    );
}

/// An explicit `spec_type` wins over the title's parsed type.
#[test]
fn update_explicit_type_wins_over_title() {
    let (_dir, db, handler) = endpoint_test_handler();
    let created = run(call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            intent: None,
            name: "old name".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    let spec_id = extract_id(&created, "spec_id");

    let updated = run(call_resource_action(
        &handler,
        "spec_edit",
        "update",
        params_id(SpecUpdateParams {
            spec_id: spec_id.clone(),
            name: Some("fix(tui): something".to_string()),
            description: None,
            workdir: None,
            spec_type: Some(SpecType::Feat),
            spec_scope: None,
            intent: None,
        }),
    ))
    .unwrap();
    assert!(!is_err(&updated), "{}", text(&updated));
    let spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    assert_eq!(spec.spec_type.map(|t| t.as_str()), Some("feat"));
    assert_eq!(spec.name, "something");
}

/// A create without a workdir carries the no-workdir line in the response.
#[test]
fn create_without_workdir_warns_line() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let created = run(call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            intent: None,
            name: "fix: warn me".to_string(),
            description: valid_spec_description(),
            workdir: None,
            spec_type: None,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert!(!is_err(&created), "{}", text(&created));
    assert!(
        text(&created).contains(
            "no workdir: the code comes from the empty-workdir counter; set a workdir before queueing so the code is final"
        ),
        "{}",
        text(&created)
    );
}

/// Asserts the response is an error naming the repeated-prefix rule.
fn assert_repeated_prefix(result: &CallToolResult) {
    assert!(is_err(result), "{}", text(result));
    assert!(text(result).contains("repeated prefix"), "{}", text(result));
}

/// Each of the four naming surfaces refuses a doubled prefix.
#[test]
fn four_surfaces_refuse_doubled_prefix() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let lp = insert_test_graph(&db, dir.path());

    // create
    let created = run(call_resource_action(
        &handler,
        "spec_edit",
        "create",
        params_id(SpecCreateParams {
            intent: None,
            name: "fix: fix: doubled".to_string(),
            description: valid_spec_description(),
            workdir: Some(workdir.clone()),
            spec_type: None,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert_repeated_prefix(&created);

    // update
    let spec_id = typed_create(
        &handler,
        &db,
        "clean subject",
        Some(&workdir),
        Some(SpecType::Fix),
    );
    let updated = update_name(&handler, &spec_id, "fix: fix: doubled").unwrap();
    assert_repeated_prefix(&updated);

    // attach_spec
    let attached = run(call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id.clone(),
            name: "fix: fix: doubled".to_string(),
            description: Some(valid_spec_description()),
            position: 1,
            parallelizable: false,
            spec_type: None,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert_repeated_prefix(&attached);

    // update_spec
    let plain = run(call_graph_action(
        &handler,
        "graph_edit",
        "attach_spec",
        params_id(GraphAddSpecParams {
            graph_id: lp.id.clone(),
            name: "plain subject".to_string(),
            description: Some(valid_spec_description()),
            position: 2,
            parallelizable: false,
            spec_type: Some(SpecType::Fix),
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert!(!is_err(&plain), "{}", text(&plain));
    let bound_id = db
        .list_graph_specs(&lp.id)
        .unwrap()
        .into_iter()
        .find(|s| s.name == "plain subject")
        .unwrap()
        .id;
    let renamed = run(call_graph_action(
        &handler,
        "graph_edit",
        "update_spec",
        params_id(GraphUpdateSpecParams {
            spec_id: bound_id,
            name: Some("fix: fix: doubled".to_string()),
            description: None,
            position: None,
            parallelizable: None,
            spec_type: None,
            spec_scope: None,
        }),
    ))
    .unwrap();
    assert_repeated_prefix(&renamed);
}

/// Each of the four naming surfaces refuses an empty and a multi-line
/// subject.
#[test]
fn four_surfaces_refuse_empty_and_multiline_subjects() {
    let (dir, db, handler) = endpoint_test_handler();
    let workdir = dir.path().to_string_lossy().to_string();
    let lp = insert_test_graph(&db, dir.path());
    let spec_id = typed_create(
        &handler,
        &db,
        "clean subject",
        Some(&workdir),
        Some(SpecType::Fix),
    );

    for name in ["", "fix: one\ntwo"] {
        let created = run(call_resource_action(
            &handler,
            "spec_edit",
            "create",
            params_id(SpecCreateParams {
                intent: None,
                name: name.to_string(),
                description: valid_spec_description(),
                workdir: Some(workdir.clone()),
                spec_type: Some(SpecType::Fix),
                spec_scope: None,
            }),
        ))
        .unwrap();
        assert!(is_err(&created), "name {name:?}: {}", text(&created));

        let updated = update_name(&handler, &spec_id, name).unwrap();
        assert!(is_err(&updated), "name {name:?}: {}", text(&updated));

        let attached = run(call_graph_action(
            &handler,
            "graph_edit",
            "attach_spec",
            params_id(GraphAddSpecParams {
                graph_id: lp.id.clone(),
                name: name.to_string(),
                description: Some(valid_spec_description()),
                position: 10,
                parallelizable: false,
                spec_type: Some(SpecType::Fix),
                spec_scope: None,
            }),
        ))
        .unwrap();
        assert!(is_err(&attached), "name {name:?}: {}", text(&attached));

        let renamed = run(call_graph_action(
            &handler,
            "graph_edit",
            "update_spec",
            params_id(GraphUpdateSpecParams {
                spec_id: spec_id.clone(),
                name: Some(name.to_string()),
                description: None,
                position: None,
                parallelizable: None,
                spec_type: None,
                spec_scope: None,
            }),
        ))
        .unwrap();
        // `update_spec` accepts a standalone spec id here (no position
        // conflict on an unbound spec), so the refusal is the subject rule.
        assert!(is_err(&renamed), "name {name:?}: {}", text(&renamed));
        assert!(
            text(&renamed).contains("empty") || text(&renamed).contains("single line"),
            "name {name:?}: {}",
            text(&renamed)
        );
    }
    assert_eq!(
        db.get_graph_spec(&spec_id).unwrap().unwrap().name,
        "clean subject"
    );
}
