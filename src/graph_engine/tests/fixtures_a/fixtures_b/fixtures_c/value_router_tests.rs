//! Deterministic (`mode: "value"`) router: dispatch, sources, defaults,
//! failures and validation.

use super::*;
use crate::domain::graphs::{validate_value_router_route_coverage, RouterRoute};

fn value_config(source: &str, labels: &[&str], default_route: Option<&str>) -> Value {
    let routes: Vec<Value> = labels
        .iter()
        .map(|label| serde_json::json!({ "label": label }))
        .collect();
    let mut config = serde_json::json!({
        "mode": "value",
        "source": source,
        "routes": routes,
    });
    if let Some(default) = default_route {
        config["default_route"] = serde_json::json!(default);
    }
    config
}

fn value_router_node(id: &str, config: Value) -> GraphNode {
    GraphNode {
        id: id.to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        name: id.to_string(),
        kind: GraphNodeKind::Router,
        config,
        position: 1,
        created_at: chrono::Utc::now(),
    }
}

fn typed_spec() -> GraphSpec {
    let mut spec = decision_spec("spec-1");
    spec.spec_type = Some(crate::domain::spec_type::SpecType::Feat);
    spec.spec_scope = Some("tui".to_string());
    spec
}

fn check_output(stdout: &str) -> Value {
    serde_json::json!({
        "kind": "check",
        "command": "true",
        "exit_code": 0,
        "stdout": stdout,
        "stderr": "",
        "passed": true,
    })
}

async fn run_value_router(node: &GraphNode, previous_output: Option<&Value>) -> NodeExecution {
    let (_dir, db) = test_db();
    let lp = decision_graph();
    let spec = typed_spec();
    execute_router_node(
        &db,
        &lp,
        &spec,
        node,
        previous_output,
        "run-value-router",
        "/tmp",
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn value_router_routes_on_spec_type_without_a_platform() {
    let node = value_router_node("r1", value_config("spec_type", &["feat", "fix"], None));
    let execution = run_value_router(&node, None).await;

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(execution.output["mode"], "value");
    assert_eq!(execution.output["source"], "spec_type");
    assert_eq!(execution.output["value"], "feat");
    assert_eq!(execution.output["route"], "feat");
    assert_eq!(execution.output["used_default"], false);
    assert!(
        node.config.get("platform").is_none(),
        "the value router must run without a platform"
    );
}

#[tokio::test]
async fn value_router_routes_on_spec_scope() {
    let node = value_router_node("r1", value_config("spec_scope", &["tui", "db"], None));
    let execution = run_value_router(&node, None).await;

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(execution.output["value"], "tui");
    assert_eq!(execution.output["route"], "tui");
}

#[tokio::test]
async fn value_router_routes_on_trimmed_check_stdout() {
    let previous = check_output("  FAIL\n");
    let node = value_router_node(
        "r1",
        value_config("previous_output", &["PASS", "FAIL"], None),
    );
    let execution = run_value_router(&node, Some(&previous)).await;

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(execution.output["source"], "previous_output");
    assert_eq!(execution.output["value"], "FAIL");
    assert_eq!(execution.output["route"], "FAIL");
}

#[test]
fn previous_output_for_another_node_is_the_model_router_input() {
    let previous = serde_json::json!({ "stdout": "not-a-check" });
    let expected = serde_json::to_string_pretty(&previous).unwrap();
    assert_eq!(
        read_value_source("previous_output", &typed_spec(), Some(&previous)).unwrap(),
        expected.trim()
    );
}

#[test]
fn entry_router_reads_an_empty_previous_output() {
    assert_eq!(
        read_value_source("previous_output", &typed_spec(), None).unwrap(),
        ""
    );
}

#[test]
fn every_declared_value_source_has_a_reader() {
    for source in crate::domain::graphs::VALUE_ROUTER_SOURCES {
        assert!(
            read_value_source(source, &typed_spec(), None).is_ok(),
            "declared source '{source}' has no read_value_source arm"
        );
    }
}

#[tokio::test]
async fn value_router_uses_default_route_when_no_label_matches() {
    let previous = check_output("unexpected");
    let node = value_router_node(
        "r1",
        value_config("previous_output", &["pass", "other"], Some("other")),
    );
    let execution = run_value_router(&node, Some(&previous)).await;

    assert_eq!(execution.status, GraphRunStatus::Pass);
    assert_eq!(execution.output["route"], "other");
    assert_eq!(execution.output["used_default"], true);
}

#[tokio::test]
async fn value_router_match_does_not_use_the_default() {
    let previous = check_output("pass");
    let node = value_router_node(
        "r1",
        value_config("previous_output", &["pass", "other"], Some("other")),
    );
    let execution = run_value_router(&node, Some(&previous)).await;

    assert_eq!(execution.output["route"], "pass");
    assert_eq!(execution.output["used_default"], false);
}

#[tokio::test]
async fn value_router_no_match_without_default_fails_with_the_exact_message() {
    let previous = check_output("surprise");
    let node = value_router_node("r1", value_config("previous_output", &["a", "b"], None));
    let execution = run_value_router(&node, Some(&previous)).await;

    assert_eq!(execution.status, GraphRunStatus::Fail);
    let expected =
        "router 'r1': value 'surprise' from previous_output matches no route (routes: a, b)";
    assert_eq!(execution.summary, expected);
    assert_eq!(execution.output["error"], expected);
    assert!(execution.output.get("route").is_none());
}

#[tokio::test]
async fn value_router_matching_is_case_sensitive() {
    let previous = check_output("PASS");
    let node = value_router_node("r1", value_config("previous_output", &["pass"], None));
    let execution = run_value_router(&node, Some(&previous)).await;

    assert_eq!(execution.status, GraphRunStatus::Fail);
    assert!(execution.summary.contains("matches no route"));
}

#[test]
fn value_router_config_is_accepted_without_a_platform() {
    let config = value_config("spec_type", &["feat"], None);
    assert!(
        crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &config).is_ok()
    );
}

#[test]
fn value_router_rejects_an_unknown_source_listing_the_valid_ones() {
    let config = value_config("git_status", &["a"], None);
    let error = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &config)
        .unwrap_err();
    assert!(
        error.contains("Valid sources: spec_type, spec_scope, previous_output"),
        "got: {error}"
    );
}

#[test]
fn value_router_rejects_an_unknown_mode_listing_the_valid_ones() {
    let mut config = value_config("spec_type", &["a"], None);
    config["mode"] = serde_json::json!("guess");
    let error = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &config)
        .unwrap_err();
    assert!(error.contains("Valid values: model, value"), "got: {error}");
}

#[test]
fn value_router_rejects_model_only_fields() {
    for (key, value) in [
        ("fallback", serde_json::json!("a")),
        ("decision", serde_json::json!(true)),
        ("min_confidence", serde_json::json!(0.5)),
    ] {
        let mut config = value_config("spec_type", &["a"], None);
        config[key] = value;
        let error =
            crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &config)
                .unwrap_err();
        assert!(
            error.contains("model routers only"),
            "field '{key}' must be refused, got: {error}"
        );
    }
}

#[test]
fn value_router_rejects_a_default_naming_no_route() {
    let config = value_config("spec_type", &["a"], Some("missing"));
    let error = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &config)
        .unwrap_err();
    assert!(
        error.contains("does not name a declared route"),
        "got: {error}"
    );
}

#[test]
fn value_router_route_count_is_one_to_eight() {
    let none = value_config("spec_type", &[], None);
    let zero = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &none)
        .unwrap_err();
    assert!(zero.contains("at least 1 route"), "got: {zero}");

    let many: Vec<String> = (0..9).map(|index| format!("route{index}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let nine = value_config("spec_type", &refs, None);
    let error = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &nine)
        .unwrap_err();
    assert!(error.contains("at most 8 routes"), "got: {error}");
}

#[test]
fn value_router_rejects_duplicate_and_empty_labels() {
    let duplicate = value_config("spec_type", &["a", "a"], None);
    let error =
        crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &duplicate)
            .unwrap_err();
    assert!(error.contains("declared more than once"), "got: {error}");

    let empty = serde_json::json!({
        "mode": "value",
        "source": "spec_type",
        "routes": [{ "label": "" }],
    });
    let error = crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &empty)
        .unwrap_err();
    assert!(error.contains("non-empty label"), "got: {error}");
}

#[test]
fn model_mode_and_mode_less_router_configs_keep_today_behaviour() {
    let base = serde_json::json!({
        "routes": [
            {"label": "a", "description": "A"},
            {"label": "b", "description": "B"}
        ],
        "fallback": "b",
    });
    assert!(
        crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &base).is_ok()
    );
    let mut explicit = base;
    explicit["mode"] = serde_json::json!("model");
    assert!(
        crate::daemon::handler_nodes::validate_node_config(GraphNodeKind::Router, &explicit)
            .is_ok()
    );
}

#[test]
fn value_router_requires_exactly_one_outgoing_edge_per_route() {
    let routes = vec![
        RouterRoute {
            label: "a".to_string(),
            description: String::new(),
        },
        RouterRoute {
            label: "b".to_string(),
            description: String::new(),
        },
    ];
    let one_edge = [route_edge("e1", "spec-1", "r1", "n-a", "a")];
    let missing = validate_value_router_route_coverage(
        &routes,
        "r1",
        one_edge
            .iter()
            .map(|edge| (edge.from_node.as_str(), edge.condition.route_label())),
    )
    .unwrap_err();
    assert!(
        missing.contains("'b'") && missing.contains("exactly one outgoing edge"),
        "got: {missing}"
    );

    let duplicate = [
        route_edge("e1", "spec-1", "r1", "n-a", "a"),
        route_edge("e2", "spec-1", "r1", "n-a2", "a"),
        route_edge("e3", "spec-1", "r1", "n-b", "b"),
    ];
    let ambiguous = validate_value_router_route_coverage(
        &routes,
        "r1",
        duplicate
            .iter()
            .map(|edge| (edge.from_node.as_str(), edge.condition.route_label())),
    )
    .unwrap_err();
    assert!(ambiguous.contains("got 2"), "got: {ambiguous}");
}

fn value_router_document() -> Value {
    serde_json::json!({
        "format_version": 1,
        "name": "picky",
        "kind": "router",
        "config": value_config("spec_type", &["feat"], None),
    })
}

#[test]
fn graph_validate_accepts_a_value_router_node_document() {
    let report = crate::daemon::graph_validate::validate_document_value(&value_router_document());
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["node_kind"], "router");
}

#[test]
fn graph_validate_rejects_model_only_fields_in_a_value_router_document() {
    let mut document = value_router_document();
    document["config"]["fallback"] = serde_json::json!("feat");
    let report = crate::daemon::graph_validate::validate_document_value(&document);
    assert_eq!(report["ok"], false);
    let message = report["problems"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("model routers only"), "got: {message}");
}

#[test]
fn graph_validate_requires_one_edge_per_value_route() {
    let document = serde_json::json!({
        "format_version": 6,
        "name": "routing",
        "nodes": [
            {"name": "start", "kind": "check", "position": 1, "config": {"command": "true"}},
            {"name": "picky", "kind": "router", "position": 2,
             "config": value_config("spec_type", &["feat", "fix"], None)},
            {"name": "feat-branch", "kind": "check", "position": 3, "config": {"command": "true"}},
            {"name": "fix-branch", "kind": "check", "position": 4, "config": {"command": "true"}},
        ],
        "edges": [
            {"from_node": "start", "to_node": "picky", "condition": "always"},
            {"from_node": "picky", "to_node": "feat-branch", "condition": {"route": "feat"}},
        ],
        "ensembles": [],
    });
    let report = crate::daemon::graph_validate::validate_document_value(&document);
    assert_eq!(report["ok"], false);
    assert_eq!(report["problems"][0]["path"], "nodes[1].config.routes");
    let message = report["problems"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("'fix'"), "got: {message}");
}
