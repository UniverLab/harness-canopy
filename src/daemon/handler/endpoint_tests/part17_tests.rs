//! Part 17 of `endpoint_tests`: the final-wave surface contract — exactly
//! the 34 visible tools, one smoke per new resource tool through its own
//! dispatch, the refusal shape for an unknown action, and the `get_tools`
//! 34-map every scope serves.
use super::*;
use crate::daemon::graph_install::stub::{StubCatalogue, StubRoute};
use serde_json::json;

/// The 34 visible tools, alphabetical: the decision's final surface plus
/// `remote` and `session_send_file`. The
/// two hidden `loop_*` aliases stay registered but are not visible.
const VISIBLE_34: &[&str] = &[
    "agent",
    "agent_edit",
    "agent_report",
    "catalogue",
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
    "remote",
    "sandbox",
    "scheduled_prompts",
    "session_list",
    "session_send_file",
    "skill",
    "spec",
    "spec_edit",
    "subagent",
];

#[tokio::test]
async fn visible_tool_surface_is_exactly_the_34() {
    let visible = visible_tool_list();
    let names: Vec<&str> = visible.iter().map(|tool| tool.name.as_ref()).collect();
    assert_eq!(names.len(), 34, "visible tool count drifted: {names:?}");
    assert_eq!(names, VISIBLE_34, "visible tool list drifted");
    assert!(
        !names.iter().any(|name| name.starts_with("loop_")),
        "hidden loop_* aliases must stay out of the visible list"
    );
}

/// The merged tool is listed under its new name and neither retired name
/// survives anywhere in the registry or the visible list.
#[tokio::test]
async fn tools_list_contains_scheduled_prompts_and_neither_old_name() {
    // Retired names built with `concat!` so no retired literal survives
    // in the repo (a literal would fail the repo-wide retired-name grep).
    let old_list = concat!("scheduled", "_send");
    let old_create = concat!("scheduled", "_send_create");
    let registered: Vec<String> = TaskTriggerHandler::tool_router()
        .list_all()
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert!(
        registered.contains(&"scheduled_prompts".to_string()),
        "registry must list scheduled_prompts: {registered:?}"
    );
    for old in [old_list, old_create] {
        assert!(
            !registered.iter().any(|name| name == old),
            "retired tool {old:?} must be gone from the registry: {registered:?}"
        );
    }
    let visible = visible_tool_list();
    let names: Vec<&str> = visible.iter().map(|tool| tool.name.as_ref()).collect();
    assert!(
        names.contains(&"scheduled_prompts"),
        "visible list must contain scheduled_prompts: {names:?}"
    );
    for old in [old_list, old_create] {
        assert!(
            !names.contains(&old),
            "retired tool {old:?} must be gone from the visible list: {names:?}"
        );
    }
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
async fn scheduled_prompts_list_smoke() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let result = call_resource_action(
        &handler,
        "scheduled_prompts",
        "list",
        params_id(ScheduledPromptListParams {
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

/// RAII guard: points `CANOPY_HOME_OVERRIDE` at `path` for the test body,
/// restoring the previous value on drop. Safe under `cargo nextest` (one
/// process per test) but would race under plain `cargo test`.
struct OverrideHome {
    prev: Option<std::ffi::OsString>,
}

impl OverrideHome {
    fn set(path: &std::path::Path) -> Self {
        let prev = std::env::var_os("CANOPY_HOME_OVERRIDE");
        unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", path) };
        OverrideHome { prev }
    }
}

impl Drop for OverrideHome {
    fn drop(&mut self) {
        match &self.prev {
            Some(value) => unsafe { std::env::set_var("CANOPY_HOME_OVERRIDE", value) },
            None => unsafe { std::env::remove_var("CANOPY_HOME_OVERRIDE") },
        }
    }
}

/// A temp home whose `config.toml` points the catalogue at `catalogue_url`.
/// Returns the home and the override guard (both kept alive by the caller).
fn catalogue_home(catalogue_url: &str) -> (tempfile::TempDir, OverrideHome) {
    let home = tempdir().unwrap();
    let canopy = home.path().join(".canopy");
    std::fs::create_dir_all(&canopy).unwrap();
    std::fs::write(
        canopy.join("config.toml"),
        format!("[graphs]\ncatalogue_url = \"{catalogue_url}\"\n"),
    )
    .unwrap();
    let guard = OverrideHome::set(home.path());
    (home, guard)
}

/// One catalogue item in the shape the live API returns (measured
/// 2026-10-31): `repo` is the combined `owner/repo`, `official` picks the
/// install-ref form, and there is no literal install-ref field.
fn catalogue_item(slug: &str) -> serde_json::Value {
    json!({
        "kind": "graph",
        "slug": slug,
        "name": slug,
        "description": format!("{slug} does things"),
        "repo": "acme/widgets",
        "official": false,
        "stars": 7,
        "sourceRef": "main",
    })
}

/// `catalogue/search` end to end through the MCP surface: a local HTTP stub
/// stands in for the catalogue, `CANOPY_HOME_OVERRIDE` points
/// `[graphs] catalogue_url` at it, and the page comes back truncated to
/// `limit` with the envelope echoed and the verdict passed through.
#[tokio::test]
async fn catalogue_search_dispatch_returns_the_stubbed_page_truncated_to_limit() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let mut judged = catalogue_item("judged");
    judged["verdict"] = json!({
        "label": "pass",
        "model": "canopy-reviewer-1",
        "scan_date": "2026-10-01",
    });
    let body = json!({
        "items": [judged, catalogue_item("second"), catalogue_item("third")],
        "q": "graphs",
        "sort": "new",
        "page": 2,
        "total": 9,
        "truncated": false,
    })
    .to_string();
    let stub = StubCatalogue::start(
        [("/api/search".to_string(), StubRoute::ok(&body))]
            .into_iter()
            .collect(),
    );
    let (_home, _guard) = catalogue_home(&stub.base_url());
    assert_eq!(
        crate::daemon::graph_install::catalogue_url(),
        stub.base_url(),
        "the stub must win: a config-parse fallback would hit the live catalogue"
    );

    let result = call_resource_action(
        &handler,
        "catalogue",
        "search",
        params_id(CatalogueSearchParams {
            q: "graphs".to_string(),
            sort: Some("new".to_string()),
            page: Some(2),
            limit: Some(2),
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&result), "{}", text(&result));
    let found: serde_json::Value =
        serde_json::from_str(&raw_text(&result)).expect("search body is JSON");
    let items = found["items"].as_array().expect("items is an array");
    assert_eq!(items.len(), 2, "limit truncates the page client-side");
    assert_eq!(items[0]["slug"], "judged");
    assert_eq!(items[0]["kind"], "graph");
    assert_eq!(items[0]["repo"], "acme/widgets");
    assert_eq!(items[0]["official"], false);
    assert_eq!(items[0]["stars"], 7);
    assert_eq!(items[1]["slug"], "second");
    assert_eq!(items[0]["verdict"]["label"], "pass");
    assert_eq!(items[0]["verdict"]["model"], "canopy-reviewer-1");
    assert_eq!(items[0]["verdict"]["scan_date"], "2026-10-01");
    assert_eq!(found["q"], "graphs");
    assert_eq!(found["sort"], "new");
    assert_eq!(found["page"], 2);
    assert_eq!(found["total"], 9);
    assert_eq!(found["truncated"], false);

    // An omitted `limit` defaults to 10, leaving a short page whole.
    let whole = call_resource_action(
        &handler,
        "catalogue",
        "search",
        params_id(CatalogueSearchParams {
            q: "graphs".to_string(),
            sort: Some("new".to_string()),
            page: Some(2),
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert!(!is_err(&whole), "{}", text(&whole));
    let found: serde_json::Value =
        serde_json::from_str(&raw_text(&whole)).expect("search body is JSON");
    assert_eq!(
        found["items"].as_array().expect("items is an array").len(),
        3,
        "the default limit keeps a 3-item page whole"
    );

    let hits = stub.hits();
    assert_eq!(hits.len(), 2);
    for hit in &hits {
        assert_eq!(hit.target, "/api/search?q=graphs&sort=new&page=2");
    }
}

/// A bad `sort` and a zero `page` fail as tool errors through the real
/// dispatch — before any request is built, so no network is touched.
#[tokio::test]
async fn catalogue_search_rejects_a_bad_sort_and_page_zero_without_network() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let bad_sort = call_resource_action(
        &handler,
        "catalogue",
        "search",
        params_id(CatalogueSearchParams {
            q: "graphs".to_string(),
            sort: Some("pop".to_string()),
            page: None,
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_sort));
    let message = text(&bad_sort);
    assert!(message.contains("'top'"), "{message}");
    assert!(message.contains("'new'"), "{message}");

    let bad_page = call_resource_action(
        &handler,
        "catalogue",
        "search",
        params_id(CatalogueSearchParams {
            q: "graphs".to_string(),
            sort: None,
            page: Some(0),
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert!(is_err(&bad_page));
    assert!(
        text(&bad_page).contains("starting at 1"),
        "{}",
        text(&bad_page)
    );
}

/// Every new tool refuses an unknown action with `INVALID_PARAMS` and a
/// message listing its own action table; a missing required parameter is
/// refused the same way.
#[tokio::test]
async fn new_tools_refuse_unknown_actions_with_their_full_table() {
    let (_dir, _db, handler) = endpoint_test_handler();
    let tables: &[(&str, &[&str])] = &[
        ("catalogue", &["search"]),
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
        ("scheduled_prompts", &["create", "list", "cancel"]),
        ("skill", &["list", "get"]),
        ("subagent", &["spawn", "collect"]),
        ("rag", &["search", "compact"]),
        ("sandbox", &["list", "get", "land", "discard"]),
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

    let missing_q = call_resource_action_value(&handler, "catalogue", "search", json!({})).await;
    match missing_q {
        Ok(result) => panic!(
            "expected a refusal for missing q, got: {}",
            raw_text(&result)
        ),
        Err(err) => {
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(
                err.message.to_string().contains("q"),
                "search refusal must name `q`: {}",
                err.message
            );
        }
    }
}

/// Every scope serves the same 34-map: one line per visible tool, each
/// naming the tool and its actions.
#[tokio::test]
async fn get_tools_serves_the_same_34_map_for_every_scope() {
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
        assert_eq!(tools.len(), 34, "{scope} must serve 34 tools");
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
    for (entry, name) in maps[0].iter().zip(VISIBLE_34.iter()) {
        assert!(
            entry.starts_with(&format!("{name} — ")),
            "map entry must lead with its tool name: {entry}"
        );
    }
}
