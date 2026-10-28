//! Part 17 of `endpoint_tests`: the final-wave surface contract — exactly
//! the 32 visible tools, one smoke per new resource tool through its own
//! dispatch, the refusal shape for an unknown action, and the `get_tools`
//! 32-map every scope serves.
use super::*;
use serde_json::json;

/// The 32 visible tools, alphabetical: the decision's final surface. The
/// two hidden `loop_*` aliases stay registered but are not visible.
const VISIBLE_32: &[&str] = &[
    "agent",
    "agent_edit",
    "agent_report",
    "config",
    "crew",
    "get_tools",
    "graph",
    "graph_complete_node",
    "graph_control",
    "graph_edge",
    "graph_edit",
    "graph_ensemble",
    "graph_node",
    "graph_report_blocker",
    "harness",
    "identity",
    "intelligence_admin",
    "intelligence_get_context",
    "intelligence_search",
    "intelligence_upsert",
    "node",
    "project",
    "queue",
    "rag",
    "sandbox",
    "scheduled_send",
    "scheduled_send_create",
    "session_list",
    "skill",
    "spec",
    "spec_edit",
    "subagent",
];

#[tokio::test]
async fn visible_tool_surface_is_exactly_the_32() {
    let visible = visible_tool_list();
    let names: Vec<&str> = visible.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names.len(), 32, "visible tool count drifted: {names:?}");
    assert_eq!(names, VISIBLE_32, "visible tool list drifted");
    assert!(
        !names.iter().any(|name| name.starts_with("loop_")),
        "hidden loop_* aliases must stay out of the visible list"
    );
}

#[tokio::test]
async fn node_list_and_get_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let listed = call_resource_action(&handler, "node", "list", ())
        .await
        .unwrap();
    assert!(!is_err(&listed), "{}", text(&listed));
    assert!(raw_text(&listed).contains("implementer"));

    let got = call_resource_action(
        &handler,
        "node",
        "get",
        params_id(NodeGetParams {
            name: "implementer".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&got), "{}", text(&got));
    let body = raw_text(&got);
    assert!(body.contains("implementer"), "{body}");
    assert!(body.contains("\"kind\""), "{body}");
    assert!(body.contains("\"config\""), "{body}");

    let missing = call_resource_action(
        &handler,
        "node",
        "get",
        params_id(NodeGetParams {
            name: "no-such-node".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&missing));
    assert!(text(&missing).contains("Unknown node"));
}

#[tokio::test]
async fn intelligence_admin_list_projects_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "intelligence_admin",
        "list_projects",
        params_id(IntelligenceListProjectsParams {
            query: None,
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(raw_text(&result).contains("projects"));
}

#[tokio::test]
async fn project_search_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "project",
        "search",
        params_id(ProjectSearchParams {
            query: "zzz-no-such-project".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(raw_text(&result).contains("projects"));
}

#[tokio::test]
async fn scheduled_send_list_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "scheduled_send",
        "list",
        params_id(ScheduledSendListParams {
            session_id: Some("nobody".to_string()),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(text(&result).contains("No pending"));
}

#[tokio::test]
async fn skill_list_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(&handler, "skill", "list", ())
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
}

#[tokio::test]
async fn subagent_collect_unknown_id_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "subagent",
        "collect",
        params_id(SubagentCollectParams {
            id: "no-such-run".to_string(),
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&result));
    assert!(text(&result).contains("not found"), "{}", text(&result));
}

#[tokio::test]
async fn rag_compact_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(&handler, "rag", "compact", ())
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    assert!(text(&result).contains("RAG"), "{}", text(&result));
}

#[tokio::test]
async fn identity_list_seeds_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(&handler, "identity", "list_seeds", ())
        .await
        .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let body: serde_json::Value =
        serde_json::from_str(&raw_text(&result)).expect("list_seeds body is JSON");
    assert!(body.is_array(), "list_seeds returns an array: {body}");
}

/// Every new tool refuses an unknown action with `INVALID_PARAMS` and a
/// message listing its own action table; a missing required parameter is
/// refused the same way.
#[tokio::test]
async fn new_tools_refuse_unknown_actions_with_their_full_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let tables: &[(&str, &[&str])] = &[
        ("node", &["list", "get", "install", "remove"]),
        (
            "intelligence_admin",
            &[
                "delete_node",
                "delete_relation",
                "walk",
                "list_projects",
                "link_projects",
            ],
        ),
        ("project", &["register", "remap", "search", "update"]),
        ("scheduled_send", &["list", "cancel"]),
        ("skill", &["list", "get"]),
        ("subagent", &["spawn", "collect"]),
        ("rag", &["search", "compact"]),
        (
            "identity",
            &["get", "evolve", "list_seeds", "create_seed", "remove_seed"],
        ),
    ];
    for (tool, actions) in tables {
        match call_resource_action_value(&handler, tool, "bogus", json!({})).await {
            Ok(result) => panic!("expected a refusal for {tool}, got: {}", raw_text(&result)),
            Err(err) => {
                assert_eq!(
                    err.code,
                    rmcp::model::ErrorCode::INVALID_PARAMS,
                    "wrong error code for {tool} refusal"
                );
                let message = err.message.to_string();
                for action in *actions {
                    assert!(
                        message.contains(&format!("`{action}`")),
                        "{tool} refusal must list action `{action}`: {message}"
                    );
                }
            }
        }
    }

    let missing = call_resource_action_value(&handler, "node", "install", json!({})).await;
    match missing {
        Ok(result) => panic!(
            "expected a refusal for missing ref, got: {}",
            raw_text(&result)
        ),
        Err(err) => {
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(
                err.message.to_string().contains("ref"),
                "install refusal must name `ref`: {}",
                err.message
            );
        }
    }
}

/// Every scope serves the same 32-map: one line per visible tool, each
/// naming the tool and its actions.
#[tokio::test]
async fn get_tools_serves_the_same_32_map_for_every_scope() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let scopes = [
        "session_start",
        "file_write",
        "test_run",
        "close_session",
        "multi_agent",
    ];
    let mut maps: Vec<Vec<String>> = Vec::new();
    for scope in scopes {
        let result = handler
            .get_tools(
                Parameters(GetToolsParams {
                    scope: scope.to_string(),
                    path: None,
                }),
                OptionalExtension(None),
            )
            .await
            .unwrap();
        assert!(!is_err(&result), "{scope}: {}", text(&result));
        let body: serde_json::Value =
            serde_json::from_str(&raw_text(&result)).expect("get_tools body is JSON");
        let tools = body["tools"].as_array().expect("tools is an array");
        assert_eq!(tools.len(), 32, "{scope} must serve 32 tools");
        maps.push(
            tools
                .iter()
                .map(|entry| entry.as_str().expect("string entry").to_string())
                .collect(),
        );
    }
    for (scope, map) in scopes.iter().zip(maps.iter()).skip(1) {
        assert_eq!(map, &maps[0], "{scope} map must equal session_start's");
    }
    for (entry, name) in maps[0].iter().zip(VISIBLE_32.iter()) {
        assert!(
            entry.starts_with(&format!("{name} — ")),
            "map entry must lead with its tool name: {entry}"
        );
    }
}
