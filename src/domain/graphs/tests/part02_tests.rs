//! Part 02 of `tests` (split from `src/domain/graphs.rs`, pure move).
use super::*;

#[test]
fn graph_edge_condition_from_str_does_not_parse_route() {
    // `Route` needs the sibling label, which a bare string can't carry —
    // only `from_parts` (DB round trip) or the `route:` MCP param path
    // can construct it.
    assert_eq!(GraphEdgeCondition::from_str("route"), None);
}

#[test]
fn graph_edge_condition_from_parts_reconstructs_route() {
    let c = GraphEdgeCondition::from_parts("route", Some("escalate".to_string()));
    assert_eq!(c, Some(GraphEdgeCondition::Route("escalate".to_string())));
}

#[test]
fn graph_edge_condition_from_parts_route_without_label_is_none() {
    assert_eq!(GraphEdgeCondition::from_parts("route", None), None);
    assert_eq!(
        GraphEdgeCondition::from_parts("route", Some("".to_string())),
        None
    );
}

#[test]
fn graph_edge_condition_from_parts_falls_back_to_from_str() {
    assert_eq!(
        GraphEdgeCondition::from_parts("pass", None),
        Some(GraphEdgeCondition::Pass)
    );
    assert_eq!(GraphEdgeCondition::from_parts("bogus", None), None);
}

#[test]
fn validate_router_routes_accepts_valid_shape() {
    assert!(validate_router_routes(&two_routes(), "retry").is_ok());
}

#[test]
fn validate_router_routes_rejects_fewer_than_two() {
    let routes = vec![RouterRoute {
        label: "retry".to_string(),
        description: "Retry the current step.".to_string(),
    }];
    let err = validate_router_routes(&routes, "retry").unwrap_err();
    assert!(err.contains("at least 2 routes"));
}

#[test]
fn validate_router_routes_rejects_more_than_eight() {
    let routes = (0..9)
        .map(|i| RouterRoute {
            label: format!("route{i}"),
            description: "A route.".to_string(),
        })
        .collect::<Vec<_>>();
    let err = validate_router_routes(&routes, "route0").unwrap_err();
    assert!(err.contains("at most 8 routes"));
}

#[test]
fn validate_router_routes_rejects_empty_label() {
    let routes = vec![
        RouterRoute {
            label: "  ".to_string(),
            description: "desc".to_string(),
        },
        RouterRoute {
            label: "escalate".to_string(),
            description: "desc".to_string(),
        },
    ];
    let err = validate_router_routes(&routes, "escalate").unwrap_err();
    assert!(err.contains("non-empty label"));
}

#[test]
fn validate_router_routes_rejects_empty_description() {
    let routes = vec![
        RouterRoute {
            label: "retry".to_string(),
            description: "".to_string(),
        },
        RouterRoute {
            label: "escalate".to_string(),
            description: "desc".to_string(),
        },
    ];
    let err = validate_router_routes(&routes, "retry").unwrap_err();
    assert!(err.contains("non-empty description"));
}

#[test]
fn validate_router_routes_rejects_duplicate_labels() {
    let routes = vec![
        RouterRoute {
            label: "retry".to_string(),
            description: "desc one".to_string(),
        },
        RouterRoute {
            label: "retry".to_string(),
            description: "desc two".to_string(),
        },
    ];
    let err = validate_router_routes(&routes, "retry").unwrap_err();
    assert!(err.contains("declared more than once"));
}

#[test]
fn validate_router_routes_rejects_no_fallback() {
    let err = validate_router_routes(&two_routes(), "").unwrap_err();
    assert!(err.contains("must declare one route as fallback"));
}

#[test]
fn validate_router_routes_rejects_fallback_naming_undeclared_route() {
    let err = validate_router_routes(&two_routes(), "nonexistent").unwrap_err();
    assert!(err.contains("does not name a declared route"));
}

#[test]
fn validate_router_edges_declared_accepts_declared_labels() {
    let edges = vec![route_edge("e1", "router-1", "retry")];
    assert!(validate_router_edges_declared(&two_routes(), "router-1", &edges).is_ok());
}

#[test]
fn validate_router_edges_declared_rejects_undeclared_label() {
    let edges = vec![route_edge("e1", "router-1", "nonexistent")];
    let err = validate_router_edges_declared(&two_routes(), "router-1", &edges).unwrap_err();
    assert!(err.contains("undeclared route 'nonexistent'"));
}

#[test]
fn validate_router_edges_declared_ignores_edges_from_other_nodes() {
    let edges = vec![route_edge("e1", "other-node", "nonexistent")];
    assert!(validate_router_edges_declared(&two_routes(), "router-1", &edges).is_ok());
}

#[test]
fn validate_router_edges_declared_ignores_non_route_edges() {
    let edges = vec![GraphEdge {
        id: "e1".to_string(),
        spec_id: Some("spec-1".to_string()),
        graph_id: None,
        from_node: "router-1".to_string(),
        to_node: "target".to_string(),
        condition: GraphEdgeCondition::Always,
    }];
    assert!(validate_router_edges_declared(&two_routes(), "router-1", &edges).is_ok());
}

// ── validate_router_route_coverage ───────────────────────────────────

#[test]
fn validate_router_route_coverage_unwired_router_is_valid() {
    // A freshly created router with no edges at all yet is a valid,
    // in-progress state — not every declared route needs an edge until
    // wiring has started.
    assert!(validate_router_route_coverage(&two_routes(), "router-1", &[]).is_ok());
}

#[test]
fn validate_router_route_coverage_rejects_partial_coverage() {
    let edges = vec![route_edge("e1", "router-1", "retry")];
    let err = validate_router_route_coverage(&two_routes(), "router-1", &edges).unwrap_err();
    assert!(err.contains("Router route 'escalate' has no outgoing edge."));
}

#[test]
fn validate_router_route_coverage_accepts_full_coverage() {
    let edges = vec![
        route_edge("e1", "router-1", "retry"),
        route_edge("e2", "router-1", "escalate"),
    ];
    assert!(validate_router_route_coverage(&two_routes(), "router-1", &edges).is_ok());
}

#[test]
fn validate_router_route_coverage_ignores_edges_from_other_nodes() {
    // A route edge that exists but belongs to a different node must not
    // count as this router having started wiring — with zero edges of
    // its own, `router-1` is still in the valid "not yet wired" state.
    let edges = vec![route_edge("e1", "other-node", "retry")];
    assert!(validate_router_route_coverage(&two_routes(), "router-1", &edges).is_ok());
}

#[test]
fn graph_hook_event_as_str_roundtrip() {
    assert_eq!(super::GraphHookEvent::OnCompleted.as_str(), "on_completed");
    assert_eq!(super::GraphHookEvent::OnFailed.as_str(), "on_failed");
    assert_eq!(super::GraphHookEvent::OnBlocked.as_str(), "on_blocked");
    assert_eq!(
        super::GraphHookEvent::OnSpecCompleted.as_str(),
        "on_spec_completed"
    );
}

#[test]
fn graph_hook_event_from_str() {
    assert_eq!(
        super::GraphHookEvent::from_str("on_completed"),
        Some(super::GraphHookEvent::OnCompleted)
    );
    assert_eq!(
        super::GraphHookEvent::from_str("on_failed"),
        Some(super::GraphHookEvent::OnFailed)
    );
    assert_eq!(
        super::GraphHookEvent::from_str("on_blocked"),
        Some(super::GraphHookEvent::OnBlocked)
    );
    assert_eq!(
        super::GraphHookEvent::from_str("on_spec_completed"),
        Some(super::GraphHookEvent::OnSpecCompleted)
    );
    assert_eq!(super::GraphHookEvent::from_str("invalid"), None);
}

#[test]
fn graph_hook_event_serde_roundtrip() {
    let events = [
        super::GraphHookEvent::OnCompleted,
        super::GraphHookEvent::OnFailed,
        super::GraphHookEvent::OnBlocked,
        super::GraphHookEvent::OnSpecCompleted,
    ];
    for event in events {
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: super::GraphHookEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(event, deserialized);
    }
}

#[test]
fn graph_edge_condition_from_str_rejects_legacy_break() {
    assert_eq!(GraphEdgeCondition::from_str("break"), None);
    assert_eq!(
        GraphEdgeCondition::from_str("error"),
        Some(GraphEdgeCondition::Error)
    );
    assert_eq!(GraphEdgeCondition::Error.as_str(), "error");
}

#[test]
fn graph_edge_condition_serde_still_reads_legacy_break_for_import() {
    let parsed: GraphEdgeCondition = serde_json::from_str("\"break\"").unwrap();
    assert_eq!(parsed, GraphEdgeCondition::Error);
    assert_eq!(
        serde_json::to_string(&GraphEdgeCondition::Error).unwrap(),
        "\"error\""
    );
}
