//! Part 21 of `endpoint_tests`: id-less verdicts — `graph_complete_node`
//! (and `graph_report_blocker`) resolve omitted ids from the caller's graph
//! identity and correct right-prefix/wrong-tail ids to the environment's run.
use super::*;

/// Request parts carrying the three graph-identity headers, the way the
/// bridge forwards the agent's env on the proxied path.
fn parts_with_graph_identity(
    graph_id: &str,
    node_id: &str,
    run_id: &str,
) -> axum::http::request::Parts {
    axum::http::Request::builder()
        .header(
            crate::shared::sync_identity::CANOPY_GRAPH_ID_HEADER,
            graph_id,
        )
        .header(crate::shared::sync_identity::CANOPY_NODE_ID_HEADER, node_id)
        .header(
            crate::shared::sync_identity::CANOPY_NODE_RUN_ID_HEADER,
            run_id,
        )
        .body(())
        .unwrap()
        .into_parts()
        .0
}

/// RAII guard pinning the three identity env vars (or clearing them),
/// restoring the previous values on drop. Safe under `cargo nextest` (one
/// process per test) but would race under plain `cargo test`.
struct IdentityEnv {
    prev: [(String, Option<std::ffi::OsString>); 3],
}

impl IdentityEnv {
    fn set(graph_id: &str, node_id: &str, run_id: &str) -> Self {
        use crate::shared::sync_identity::{
            CANOPY_GRAPH_ID_ENV, CANOPY_NODE_ID_ENV, CANOPY_NODE_RUN_ID_ENV,
        };
        let prev = [
            (
                CANOPY_GRAPH_ID_ENV.to_string(),
                std::env::var_os(CANOPY_GRAPH_ID_ENV),
            ),
            (
                CANOPY_NODE_ID_ENV.to_string(),
                std::env::var_os(CANOPY_NODE_ID_ENV),
            ),
            (
                CANOPY_NODE_RUN_ID_ENV.to_string(),
                std::env::var_os(CANOPY_NODE_RUN_ID_ENV),
            ),
        ];
        unsafe {
            std::env::set_var(CANOPY_GRAPH_ID_ENV, graph_id);
            std::env::set_var(CANOPY_NODE_ID_ENV, node_id);
            std::env::set_var(CANOPY_NODE_RUN_ID_ENV, run_id);
        }
        IdentityEnv { prev }
    }

    fn clear() -> Self {
        use crate::shared::sync_identity::{
            CANOPY_GRAPH_ID_ENV, CANOPY_NODE_ID_ENV, CANOPY_NODE_RUN_ID_ENV,
        };
        let this = Self::set("", "", "");
        unsafe {
            std::env::remove_var(CANOPY_GRAPH_ID_ENV);
            std::env::remove_var(CANOPY_NODE_ID_ENV);
            std::env::remove_var(CANOPY_NODE_RUN_ID_ENV);
        }
        this
    }
}

impl Drop for IdentityEnv {
    fn drop(&mut self) {
        for (name, prev) in &self.prev {
            match prev {
                Some(v) => unsafe { std::env::set_var(name, v) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}

async fn complete_node(
    handler: &TaskTriggerHandler,
    run_id: Option<String>,
    node_id: Option<String>,
    parts: Option<axum::http::request::Parts>,
) -> CallToolResult {
    handler
        .graph_complete_node(
            Parameters(GraphCompleteNodeParams {
                run_id,
                node_id,
                status: "pass".to_string(),
                output: "out".to_string(),
                summary: "sum".to_string(),
            }),
            OptionalExtension(parts),
        )
        .await
        .unwrap()
}

/// A completion with no ids resolves from the environment: no headers, the
/// three vars set (the stdio path, where the MCP server inherits the agent's
/// env directly) — the env run is finalized with no correction note.
#[tokio::test]
async fn graph_complete_node_without_ids_resolves_from_env() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::set(&lp.id, &run.node_id, &run.id);

    let result = complete_node(&handler, None, None, None).await;
    assert!(!is_err(&result), "{}", text(&result));
    assert!(
        !raw_text(&result).contains("Corrected"),
        "an exact env resolution is not a correction: {}",
        text(&result)
    );

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphRunStatus::Pass);
}

/// The same via headers (the proxied path): env cleared so only the three
/// headers can identify the run.
#[tokio::test]
async fn graph_complete_node_without_ids_resolves_from_identity_headers() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::clear();

    let result = complete_node(
        &handler,
        None,
        None,
        Some(parts_with_graph_identity(&lp.id, &run.node_id, &run.id)),
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphRunStatus::Pass);
}

/// The 2026-10-27 transcription error: right 8-char prefix, invented tail on
/// both ids. The verdict lands on the environment's run and the result says
/// which ids it corrected.
#[tokio::test]
async fn graph_complete_node_with_wrong_tail_ids_is_corrected_to_env_run() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::set(&lp.id, &run.node_id, &run.id);

    let wrong_tail = |real: &str| format!("{}ffffffff-ffff-ffff-ffff-ffffffffffff", &real[..8]);
    let bad_run = wrong_tail(&run.id);
    let bad_node = wrong_tail(&run.node_id);
    assert_ne!(bad_run, run.id);
    assert_ne!(bad_node, run.node_id);

    let result = complete_node(
        &handler,
        Some(bad_run.clone()),
        Some(bad_node.clone()),
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("Corrected"), "{body}");
    for id in [&bad_run, &run.id, &bad_node, &run.node_id] {
        assert!(body.contains(id), "result must name {id}: {body}");
    }

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphRunStatus::Pass);
}

/// A wrong-prefix id keeps failing — and the error names the run id the
/// environment carries, so the agent can retry with it.
#[tokio::test]
async fn graph_complete_node_with_wrong_prefix_ids_fails_naming_env_run() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::set(&lp.id, &run.node_id, &run.id);

    let result = complete_node(
        &handler,
        Some("zzzzzzzz-0000-0000-0000-000000000000".to_string()),
        Some("yyyyyyyy-0000-0000-0000-000000000000".to_string()),
        None,
    )
    .await;
    assert!(is_err(&result), "{}", text(&result));
    let body = text(&result);
    assert!(
        body.contains(&run.id),
        "error must name the env run id: {body}"
    );
    assert!(body.contains("CANOPY_NODE_RUN_ID"), "{body}");

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(
        stored.status,
        GraphRunStatus::Running,
        "a rejected report must not touch the run"
    );
}

/// A partial transcription error — only the run id's tail is invented, the
/// node id is exactly right — is corrected for the run id alone: the result
/// must not claim it also "corrected" a node id that never changed (FR3:
/// the result says which ids it corrected).
#[tokio::test]
async fn graph_complete_node_corrects_only_the_ids_that_were_wrong() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::set(&lp.id, &run.node_id, &run.id);

    let bad_run = format!("{}ffffffff-ffff-ffff-ffff-ffffffffffff", &run.id[..8]);
    assert_ne!(bad_run, run.id);

    let result = complete_node(
        &handler,
        Some(bad_run.clone()),
        Some(run.node_id.clone()),
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    let body = raw_text(&result);
    assert!(body.contains("Corrected"), "{body}");
    for id in [&bad_run, &run.id] {
        assert!(body.contains(id), "result must name {id}: {body}");
    }
    assert!(
        !body.contains("node_id"),
        "the node id was already right; the result must not report correcting it: {body}"
    );

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphRunStatus::Pass);
}

/// FR2's "when either is omitted": one id supplied, the other omitted, and
/// no identity anywhere — still the missing-identity error, naming the env
/// var, regardless of the supplied id.
#[tokio::test]
async fn graph_complete_node_with_one_id_omitted_and_no_identity_fails() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::clear();

    let result = complete_node(&handler, Some(run.id.clone()), None, None).await;
    assert!(is_err(&result), "{}", text(&result));
    assert!(
        text(&result).contains("CANOPY_NODE_RUN_ID"),
        "{}",
        text(&result)
    );

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(
        stored.status,
        GraphRunStatus::Running,
        "a rejected report must not touch the run"
    );
}

/// No ids and no identity anywhere: the call fails naming the missing
/// environment variable.
#[tokio::test]
async fn graph_complete_node_without_ids_or_identity_fails() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::clear();

    let result = complete_node(&handler, None, None, None).await;
    assert!(is_err(&result), "{}", text(&result));
    assert!(
        text(&result).contains("CANOPY_NODE_RUN_ID"),
        "{}",
        text(&result)
    );

    let stored = db.get_graph_run(&run.id).unwrap().unwrap();
    assert_eq!(stored.status, GraphRunStatus::Running);
}

/// FR4: an explicit id that names a different, existing running run is
/// honored even when the identity names another run — no silent redirect.
#[tokio::test]
async fn graph_complete_node_with_explicit_other_run_is_honored() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run_a = insert_running_node_run(&db, &lp.id);
    // A second running run under the same graph — built manually at spec
    // position 2 because the fixture hardcodes position 1.
    let spec_b = insert_test_spec(&db, &lp.id, 2);
    let node_b = GraphNode {
        id: uuid::Uuid::new_v4().to_string(),
        spec_id: Some(spec_b.id.clone()),
        graph_id: None,
        name: "Node".to_string(),
        kind: GraphNodeKind::Agent,
        config: serde_json::json!({"platform": "claude"}),
        position: 1,
        created_at: chrono::Utc::now(),
    };
    db.insert_graph_node(&node_b).unwrap();
    let run_b = GraphNodeRun {
        id: uuid::Uuid::new_v4().to_string(),
        graph_id: lp.id.clone(),
        spec_id: spec_b.id,
        node_id: node_b.id,
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
    db.insert_graph_run(&run_b).unwrap();
    let _guard = IdentityEnv::set(&lp.id, &run_a.node_id, &run_a.id);

    let result = complete_node(
        &handler,
        Some(run_b.id.clone()),
        Some(run_b.node_id.clone()),
        None,
    )
    .await;
    assert!(!is_err(&result), "{}", text(&result));
    assert!(
        !raw_text(&result).contains("Corrected"),
        "an honored explicit id is not a correction: {}",
        text(&result)
    );

    assert_eq!(
        db.get_graph_run(&run_b.id).unwrap().unwrap().status,
        GraphRunStatus::Pass,
        "the explicitly named run is the one finalized"
    );
    assert_eq!(
        db.get_graph_run(&run_a.id).unwrap().unwrap().status,
        GraphRunStatus::Running,
        "the identity's run must be untouched"
    );
}

/// The blocker shares the resolution path: no ids, identity from headers,
/// graph paused.
#[tokio::test]
async fn graph_report_blocker_without_ids_resolves_from_identity_headers() {
    let (dir, db, handler) = endpoint_test_handler();
    let lp = insert_test_graph(&db, dir.path());
    let run = insert_running_node_run(&db, &lp.id);
    let _guard = IdentityEnv::clear();

    let result = handler
        .graph_report_blocker(
            Parameters(GraphReportBlockerParams {
                run_id: None,
                node_id: None,
                description: "waiting on human input".to_string(),
            }),
            OptionalExtension(Some(parts_with_graph_identity(
                &lp.id,
                &run.node_id,
                &run.id,
            ))),
        )
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert_eq!(
        db.get_graph(&lp.id).unwrap().unwrap().status,
        GraphStatus::Paused
    );
    assert_eq!(
        db.get_graph_run(&run.id).unwrap().unwrap().status,
        GraphRunStatus::Fail,
        "a blocker finalizes the run as fail"
    );
}
