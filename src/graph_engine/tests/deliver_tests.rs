//! Deliver-node tests: config validation, deterministic delivery to a live
//! session, router-vocabulary rendering, provenance, and validator rules.
use super::super::*;
use super::fixtures_a::*;
use std::sync::Arc;
use std::time::Duration;

/// A deliver node under `spec_id` with the given config.
fn deliver_node(name: &str, spec_id: &str, config: serde_json::Value) -> GraphNode {
    GraphNode {
        id: format!("node-{name}"),
        spec_id: Some(spec_id.to_string()),
        graph_id: None,
        name: name.to_string(),
        kind: GraphNodeKind::Deliver,
        config,
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

fn deliver_config(pairs: &[(&str, &str)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (key, value) in pairs {
        map.insert(
            (*key).to_string(),
            serde_json::Value::String((*value).to_string()),
        );
    }
    serde_json::Value::Object(map)
}

/// Temp graph + spec plus an engine whose scheduled-prompt notify the test
/// owns. The spec carries a known description/name in memory so rendering
/// assertions are exact (nothing is re-persisted).
fn deliver_fixture() -> (
    tempfile::TempDir,
    Arc<Database>,
    GraphEngine,
    Arc<tokio::sync::Notify>,
    Graph,
    GraphSpec,
) {
    let (_dir, db, engine, graph_id, spec_id) = graph_fixture().unwrap();
    let notify = Arc::new(tokio::sync::Notify::new());
    let engine = engine.with_scheduled_prompt_notify(Arc::clone(&notify));
    let lp = db.get_graph(&graph_id).unwrap().unwrap();
    let mut spec = db.get_graph_spec(&spec_id).unwrap().unwrap();
    spec.description = Some("Deliver the news.".to_string());
    spec.name = "News spec".to_string();
    (_dir, db, engine, notify, lp, spec)
}

fn live_session(db: &Database, id: &str, name: &str) {
    db.insert_interactive_session(
        id,
        name,
        "opencode",
        "/tmp",
        None,
        None,
        "interactive",
        None,
    )
    .unwrap();
}

fn validate_deliver(config: &serde_json::Value) -> Result<(), String> {
    crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Deliver, config)
}

// ── Config validation ────────────────────────────────────────────────

#[test]
fn deliver_config_accepts_name_target() {
    let config = deliver_config(&[
        ("target_session_name", "orch"),
        ("prompt_template", "news: {{spec_name}}"),
    ]);
    assert!(validate_deliver(&config).is_ok());
}

#[test]
fn deliver_config_accepts_id_target() {
    let config = deliver_config(&[
        ("target_session_id", "sess-1"),
        ("prompt_template", "news: {{spec_name}}"),
    ]);
    assert!(validate_deliver(&config).is_ok());
}

#[test]
fn deliver_config_rejects_both_targets() {
    let config = deliver_config(&[
        ("target_session_name", "orch"),
        ("target_session_id", "sess-1"),
        ("prompt_template", "news"),
    ]);
    let err = validate_deliver(&config).expect_err("both targets must be refused");
    assert!(err.contains("exactly one target"), "got: {err}");
}

#[test]
fn deliver_config_rejects_neither_target() {
    let config = deliver_config(&[("prompt_template", "news")]);
    let err = validate_deliver(&config).expect_err("no target must be refused");
    assert!(err.contains("exactly one target"), "got: {err}");
}

#[test]
fn deliver_config_treats_blank_targets_as_unset() {
    let both_blank = deliver_config(&[
        ("target_session_name", "   "),
        ("target_session_id", ""),
        ("prompt_template", "news"),
    ]);
    let err = validate_deliver(&both_blank).expect_err("blank targets must be refused");
    assert!(err.contains("exactly one target"), "got: {err}");

    let blank_name_ok = deliver_config(&[
        ("target_session_name", "  "),
        ("target_session_id", "sess-1"),
        ("prompt_template", "news"),
    ]);
    assert!(validate_deliver(&blank_name_ok).is_ok());
}

#[test]
fn deliver_config_rejects_harness_keys() {
    for key in ["platform", "model", "cli", "crew"] {
        let config = deliver_config(&[
            ("target_session_name", "orch"),
            ("prompt_template", "news"),
            (key, "claude"),
        ]);
        let err = validate_deliver(&config).expect_err("harness key must be refused");
        assert!(err.contains(key), "got: {err}");
    }
}

#[test]
fn deliver_config_requires_prompt_template() {
    let missing = deliver_config(&[("target_session_name", "orch")]);
    let err = validate_deliver(&missing).expect_err("missing template must be refused");
    assert!(err.contains("prompt_template"), "got: {err}");

    let blank = deliver_config(&[("target_session_name", "orch"), ("prompt_template", "  ")]);
    let err = validate_deliver(&blank).expect_err("blank template must be refused");
    assert!(err.contains("prompt_template"), "got: {err}");
}

#[test]
fn deliver_config_rejects_unbindable_marker() {
    let config = deliver_config(&[
        ("target_session_name", "orch"),
        ("prompt_template", "news {{nope}}"),
    ]);
    let err = validate_deliver(&config).expect_err("bad marker must be refused");
    assert!(err.contains("nope"), "got: {err}");
}

#[test]
fn deliver_config_rejects_check_marker_vocabulary() {
    // `spec_start_head` binds in check commands, never in a deliver prompt.
    let config = deliver_config(&[
        ("target_session_name", "orch"),
        ("prompt_template", "at {{spec_start_head}}"),
    ]);
    let err = validate_deliver(&config).expect_err("check marker must be refused");
    assert!(err.contains("spec_start_head"), "got: {err}");
}

#[test]
fn deliver_config_rejects_output_refs() {
    let config = deliver_config(&[
        ("target_session_name", "orch"),
        ("prompt_template", "was {{output:Kickoff}}"),
    ]);
    let err = validate_deliver(&config).expect_err("output ref must be refused");
    assert!(err.contains("output:Kickoff"), "got: {err}");
}

// ── Execution: live delivery ─────────────────────────────────────────

#[tokio::test]
async fn deliver_to_live_session_enqueues_send() {
    let (_dir, db, engine, notify, lp, spec) = deliver_fixture();
    live_session(&db, "sess-live", "orch");
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[
            ("target_session_name", "orch"),
            (
                "prompt_template",
                "News for {{spec_name}}: {{spec_content}} / prev={{previous_output}} fb={{previous_feedback}}",
            ),
        ]),
    );
    let previous = serde_json::json!({"verdict": "pass"});

    let execution = engine
        .execute_deliver_node(&lp, &spec, &node, Some(&previous), "run-1", "/tmp")
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution
            .output
            .get("target_session_id")
            .and_then(Value::as_str),
        Some("sess-live")
    );
    assert_eq!(
        execution
            .output
            .get("target_session_name")
            .and_then(Value::as_str),
        Some("orch")
    );
    let send_id = execution
        .output
        .get("scheduled_prompt_id")
        .and_then(Value::as_str)
        .expect("pass output carries the send id");
    assert!(!send_id.is_empty());

    // The queued send renders every marker: pretty previous output,
    // the spec description, and the spec name.
    let pending = db
        .list_pending_scheduled_prompts_for_session("sess-live")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, send_id);
    assert!(
        pending[0].prompt.contains("\"verdict\": \"pass\""),
        "{}",
        pending[0].prompt
    );
    assert!(
        pending[0].prompt.contains("Deliver the news."),
        "{}",
        pending[0].prompt
    );
    assert!(
        pending[0].prompt.contains("News spec"),
        "{}",
        pending[0].prompt
    );

    // Provenance carries graph, node and run ids.
    let provenance = pending[0].provenance.as_ref().expect("provenance stored");
    assert_eq!(provenance.kind, "deliver");
    assert_eq!(provenance.graph_id, lp.id);
    assert_eq!(provenance.node_id.as_deref(), Some(node.id.as_str()));
    assert_eq!(provenance.run_id.as_deref(), Some("run-1"));

    // The delivery task is woken.
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .expect("enqueue notifies the delivery task");
}

#[tokio::test]
async fn deliver_to_live_session_by_id() {
    let (_dir, db, engine, _notify, lp, spec) = deliver_fixture();
    live_session(&db, "sess-by-id", "other");
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[
            ("target_session_id", "sess-by-id"),
            ("prompt_template", "hi {{spec_name}}"),
        ]),
    );

    let execution = engine
        .execute_deliver_node(&lp, &spec, &node, None, "run-9", "/tmp")
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(
        execution
            .output
            .get("target_session_id")
            .and_then(Value::as_str),
        Some("sess-by-id")
    );
    assert!(
        execution.output.get("target_session_name").is_none(),
        "id-targeted sends name no session"
    );
    let pending = db
        .list_pending_scheduled_prompts_for_session("sess-by-id")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].prompt.contains("News spec"),
        "{}",
        pending[0].prompt
    );
}

#[test]
fn deliver_missing_name_target_fails_without_enqueue() {
    let (_dir, db, engine, _notify, lp, spec) = deliver_fixture();
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[("target_session_name", "ghost"), ("prompt_template", "hi")]),
    );

    let execution = engine
        .execute_deliver_node(&lp, &spec, &node, None, "run-2", "/tmp")
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail);
    let error = execution
        .output
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        error.contains("session does not exist or is no longer live"),
        "{error}"
    );
    assert_eq!(
        execution
            .output
            .get("target_session_name")
            .and_then(Value::as_str),
        Some("ghost")
    );
    assert!(
        db.list_due_scheduled_prompts(chrono::Utc::now())
            .unwrap()
            .is_empty(),
        "nothing enqueued on missing target"
    );
}

#[test]
fn deliver_ambiguous_name_fails_listing_ids() {
    let (_dir, db, engine, _notify, lp, spec) = deliver_fixture();
    live_session(&db, "sess-a", "orch");
    live_session(&db, "sess-b", "orch");
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[("target_session_name", "orch"), ("prompt_template", "hi")]),
    );

    let execution = engine
        .execute_deliver_node(&lp, &spec, &node, None, "run-3", "/tmp")
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail);
    let error = execution
        .output
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        error.contains("multiple live sessions share this name"),
        "{error}"
    );
    let ids = execution
        .output
        .get("matching_ids")
        .and_then(Value::as_array)
        .expect("ambiguous failure lists the matching ids");
    let mut listed: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
    listed.sort_unstable();
    assert_eq!(listed, vec!["sess-a", "sess-b"]);
    assert!(
        db.list_due_scheduled_prompts(chrono::Utc::now())
            .unwrap()
            .is_empty(),
        "nothing enqueued on ambiguous target"
    );
}

#[test]
fn deliver_dead_id_target_fails_without_enqueue() {
    let (_dir, db, engine, _notify, lp, spec) = deliver_fixture();
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[
            ("target_session_id", "sess-gone"),
            ("prompt_template", "hi"),
        ]),
    );

    let execution = engine
        .execute_deliver_node(&lp, &spec, &node, None, "run-4", "/tmp")
        .unwrap();
    assert_eq!(execution.status, GraphRunStatus::Fail);
    let error = execution
        .output
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(
        error.contains("session does not exist or is no longer live"),
        "{error}"
    );
    assert!(
        db.list_due_scheduled_prompts(chrono::Utc::now())
            .unwrap()
            .is_empty(),
        "nothing enqueued on dead target"
    );
}

// ── Rendering ────────────────────────────────────────────────────────

#[tokio::test]
async fn deliver_renders_none_for_missing_previous_output() {
    let (_dir, db, engine, _notify, lp, spec) = deliver_fixture();
    live_session(&db, "sess-1", "orch");
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[
            ("target_session_name", "orch"),
            ("prompt_template", "prev was {{previous_output}}"),
        ]),
    );

    engine
        .execute_deliver_node(&lp, &spec, &node, None, "run-5", "/tmp")
        .unwrap();
    let pending = db
        .list_pending_scheduled_prompts_for_session("sess-1")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].prompt.contains("(none)"),
        "{}",
        pending[0].prompt
    );
}

#[tokio::test]
async fn deliver_bounds_large_previous_output() {
    let (_dir, db, engine, _notify, lp, spec) = deliver_fixture();
    live_session(&db, "sess-2", "orch");
    let node = deliver_node(
        "herald",
        &spec.id,
        deliver_config(&[
            ("target_session_name", "orch"),
            ("prompt_template", "log: {{previous_output}}"),
        ]),
    );
    let big = serde_json::json!({"log": "x".repeat(64 * 1024)});

    engine
        .execute_deliver_node(&lp, &spec, &node, Some(&big), "run-6", "/tmp")
        .unwrap();
    let pending = db
        .list_pending_scheduled_prompts_for_session("sess-2")
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert!(
        pending[0].prompt.contains("bytes elided"),
        "{}",
        &pending[0].prompt[..200.min(pending[0].prompt.len())]
    );
    assert!(
        pending[0].prompt.len() < 64 * 1024,
        "bounded, not interpolated whole"
    );
}

// ── Provenance ───────────────────────────────────────────────────────

#[test]
fn deliver_provenance_carries_node_and_run_ids() {
    let provenance =
        crate::db::scheduled_prompts::ScheduledPromptProvenance::deliver("g1", "n1", "r1");
    assert_eq!(provenance.kind, "deliver");
    assert_eq!(provenance.graph_id, "g1");
    assert_eq!(provenance.node_id.as_deref(), Some("n1"));
    assert_eq!(provenance.run_id.as_deref(), Some("r1"));
    let json = serde_json::to_string(&provenance).unwrap();
    assert!(json.contains("\"node_id\":\"n1\""), "{json}");
    assert!(json.contains("\"run_id\":\"r1\""), "{json}");
}

#[test]
fn deliver_provenance_old_rows_deserialize_without_ids() {
    let raw = r#"{"kind":"hook","graph_id":"graph-1","event":"on_failed"}"#;
    let provenance: crate::db::scheduled_prompts::ScheduledPromptProvenance =
        serde_json::from_str(raw).unwrap();
    assert!(provenance.node_id.is_none());
    assert!(provenance.run_id.is_none());
}

// ── Kind plumbing ────────────────────────────────────────────────────

#[test]
fn deliver_kind_round_trips_as_string() {
    assert_eq!(GraphNodeKind::Deliver.as_str(), "deliver");
    assert_eq!(
        GraphNodeKind::from_str("deliver"),
        Some(GraphNodeKind::Deliver)
    );
    assert_eq!(GraphNodeKind::Deliver.display_str(), "deliver");
    let json = serde_json::to_string(&GraphNodeKind::Deliver).unwrap();
    assert_eq!(json, "\"deliver\"");
    let back: GraphNodeKind = serde_json::from_str(&json).unwrap();
    assert_eq!(back, GraphNodeKind::Deliver);
}

// ── Structural validator ─────────────────────────────────────────────

/// Run the shared graph validator over `kick -> deliver -> done` with
/// `conds` as the deliver node's outgoing edges. The kick edge gives
/// deliver an incoming edge, so the entry-point and reachability rules
/// stay quiet and only the deliver edge-count rule can fire.
fn validate_deliver_graph(conds: &[GraphEdgeCondition]) -> Result<(), String> {
    use crate::domain::validation::{GraphEdgeView, GraphNodeView};
    let kick_id = "kick".to_string();
    let deliver_id = "deliver".to_string();
    let done_id = "done".to_string();
    let no_labels: Vec<String> = Vec::new();
    let mut nodes = vec![
        GraphNodeView {
            id: &kick_id,
            kind: GraphNodeKind::Check,
            route_labels: &no_labels,
        },
        GraphNodeView {
            id: &deliver_id,
            kind: GraphNodeKind::Deliver,
            route_labels: &no_labels,
        },
    ];
    let mut edges = vec![GraphEdgeView {
        from: &kick_id,
        to: &deliver_id,
        condition: &GraphEdgeCondition::Always,
    }];
    // A `done` node only exists when deliver routes somewhere: with zero
    // outgoing edges deliver is the sole terminal, and a dangling `done`
    // would trip the entry-point rule before the edge-count rule.
    if !conds.is_empty() {
        nodes.push(GraphNodeView {
            id: &done_id,
            kind: GraphNodeKind::Check,
            route_labels: &no_labels,
        });
        edges.extend(conds.iter().map(|condition| GraphEdgeView {
            from: &deliver_id,
            to: &done_id,
            condition,
        }));
    }
    crate::domain::validation::validate_graph(&nodes, &edges).map(|_| ())
}

#[test]
fn deliver_validator_accepts_one_outgoing_edge() {
    assert!(validate_deliver_graph(&[GraphEdgeCondition::Always]).is_ok());
    assert!(validate_deliver_graph(&[GraphEdgeCondition::Pass]).is_ok());
}

#[test]
fn deliver_validator_rejects_zero_edges() {
    let err = validate_deliver_graph(&[]).expect_err("zero edges must be refused");
    assert!(err.contains("exactly one outgoing edge"), "got: {err}");
}

#[test]
fn deliver_validator_rejects_two_edges() {
    let err = validate_deliver_graph(&[GraphEdgeCondition::Pass, GraphEdgeCondition::Fail])
        .expect_err("two edges must be refused");
    assert!(err.contains("exactly one outgoing edge"), "got: {err}");
}

#[test]
fn deliver_validator_ignores_error_edge() {
    // An autowired infra `error` edge never carries a verdict: pass + error
    // is still exactly one step onward, error alone is zero.
    assert!(validate_deliver_graph(&[GraphEdgeCondition::Pass, GraphEdgeCondition::Error]).is_ok());
    let err = validate_deliver_graph(&[GraphEdgeCondition::Error])
        .expect_err("error-only must be refused");
    assert!(err.contains("exactly one outgoing edge"), "got: {err}");
}
